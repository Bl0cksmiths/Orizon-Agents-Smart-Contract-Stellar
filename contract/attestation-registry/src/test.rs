#![cfg(test)]

use crate::{AttestationRegistry, AttestationRegistryClient, DataKey, Error};
use orizon_shared::ttl::{DAY_IN_LEDGERS, EXTEND_TO};
use soroban_sdk::{
    symbol_short,
    testutils::{
        storage::{Instance as _, Persistent as _},
        Address as _, Ledger as _, LedgerInfo,
    },
    vec, Address, BytesN, Env,
};

fn setup(env: &Env) -> (AttestationRegistryClient<'_>, Address, Address) {
    let admin = Address::generate(env);
    let sealer = Address::generate(env);
    let id = env.register(AttestationRegistry, (admin.clone(), sealer.clone()));
    (AttestationRegistryClient::new(env, &id), admin, sealer)
}

#[test]
fn seal_and_read() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, sealer) = setup(&env);

    let job = BytesN::from_array(&env, &[1u8; 16]);
    let intent_hash = BytesN::from_array(&env, &[2u8; 32]);
    let orch = Address::generate(&env);
    let agents = vec![&env, symbol_short!("seo_b"), symbol_short!("copy_v3")];
    let receipts = vec![
        &env,
        BytesN::from_array(&env, &[3u8; 16]),
        BytesN::from_array(&env, &[4u8; 16]),
    ];

    client.seal(
        &sealer,
        &job,
        &orch,
        &intent_hash,
        &agents,
        &receipts,
        &180_000,
    );

    assert!(client.exists(&job));
    let a = client.get(&job);
    assert_eq!(a.total_spent, 180_000);
    assert_eq!(a.agents.len(), 2);
}

#[test]
fn write_once() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _admin, sealer) = setup(&env);

    let job = BytesN::from_array(&env, &[1u8; 16]);
    let intent_hash = BytesN::from_array(&env, &[2u8; 32]);
    let orch = Address::generate(&env);
    let agents = vec![&env, symbol_short!("seo_b")];
    let receipts = vec![&env, BytesN::from_array(&env, &[3u8; 16])];

    client.seal(&sealer, &job, &orch, &intent_hash, &agents, &receipts, &10);
    let err = client.try_seal(&sealer, &job, &orch, &intent_hash, &agents, &receipts, &10);
    assert_eq!(err.err().unwrap().unwrap(), Error::AlreadyExists);
}

// ── storage lifetime (D-083) ──────────────────────────────────────────
//
// The live registry never extended anything: its instance, wasm and seals
// lived only the network's minimum persistent TTL (7 days) past their last
// write, then read as archived. These tests run on a ledger with testnet's
// real limits.

const MIN_PERSISTENT_TTL: u32 = 120_960;
const MAX_ENTRY_TTL: u32 = 3_110_400;

fn ledger_with_limits(env: &Env, max_entry_ttl: u32) {
    env.ledger().set(LedgerInfo {
        timestamp: 1_000,
        protocol_version: 25,
        sequence_number: 100,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: MIN_PERSISTENT_TTL,
        max_entry_ttl,
    });
}

fn advance_ledgers(env: &Env, ledgers: u32) {
    env.ledger().with_mut(|li| li.sequence_number += ledgers);
}

fn job_ttl(env: &Env, client: &AttestationRegistryClient, job: &BytesN<16>) -> u32 {
    env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Job(job.clone()))
    })
}

fn instance_ttl(env: &Env, client: &AttestationRegistryClient) -> u32 {
    env.as_contract(&client.address, || env.storage().instance().get_ttl())
}

fn seal_one(env: &Env, client: &AttestationRegistryClient, sealer: &Address, n: u8) -> BytesN<16> {
    let job = BytesN::from_array(env, &[n; 16]);
    client.seal(
        sealer,
        &job,
        &Address::generate(env),
        &BytesN::from_array(env, &[2u8; 32]),
        &vec![env, symbol_short!("seo_b")],
        &vec![env, BytesN::from_array(env, &[3u8; 16])],
        &10,
    );
    job
}

#[test]
fn ttl_deploy_and_seal_extend_to_the_network_maximum() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_limits(&env, MAX_ENTRY_TTL);
    let (client, _admin, sealer) = setup(&env);
    let max = env.storage().max_ttl();
    assert_eq!(max, MAX_ENTRY_TTL - 1);

    // Deploying already gives the instance the full lifetime.
    assert_eq!(instance_ttl(&env, &client), max);

    // Two days on, a seal lives the maximum and tops the instance back up.
    advance_ledgers(&env, 2 * DAY_IN_LEDGERS);
    assert_eq!(instance_ttl(&env, &client), max - 2 * DAY_IN_LEDGERS);
    let job = seal_one(&env, &client, &sealer, 1);
    assert_eq!(job_ttl(&env, &client, &job), max);
    assert_eq!(instance_ttl(&env, &client), max);
}

#[test]
fn ttl_reads_re_extend_an_aged_seal_and_the_instance() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_limits(&env, MAX_ENTRY_TTL);
    let (client, _admin, sealer) = setup(&env);
    let max = env.storage().max_ttl();
    let job = seal_one(&env, &client, &sealer, 1);

    // `get` re-extends a seal that has aged past the renew window.
    advance_ledgers(&env, 2 * DAY_IN_LEDGERS);
    assert_eq!(job_ttl(&env, &client, &job), max - 2 * DAY_IN_LEDGERS);
    client.get(&job);
    assert_eq!(job_ttl(&env, &client, &job), max);
    assert_eq!(instance_ttl(&env, &client), max);

    // So does `exists`.
    advance_ledgers(&env, 2 * DAY_IN_LEDGERS);
    assert!(client.exists(&job));
    assert_eq!(job_ttl(&env, &client, &job), max);
    assert_eq!(instance_ttl(&env, &client), max);

    // `exists` on a job that was never sealed is a plain false.
    assert!(!client.exists(&BytesN::from_array(&env, &[9u8; 16])));
}

#[test]
fn ttl_inside_the_renew_window_nothing_is_re_extended() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_limits(&env, MAX_ENTRY_TTL);
    let (client, _admin, sealer) = setup(&env);
    let max = env.storage().max_ttl();
    let job = seal_one(&env, &client, &sealer, 1);

    // Less than a day on, a read leaves both lifetimes alone (no rent paid).
    advance_ledgers(&env, DAY_IN_LEDGERS - 1);
    client.get(&job);
    assert_eq!(job_ttl(&env, &client, &job), max - (DAY_IN_LEDGERS - 1));
    assert_eq!(instance_ttl(&env, &client), max - (DAY_IN_LEDGERS - 1));
}

#[test]
fn ttl_set_sealer_extends_the_instance() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_limits(&env, MAX_ENTRY_TTL);
    let (client, _admin, _sealer) = setup(&env);
    let max = env.storage().max_ttl();

    advance_ledgers(&env, 2 * DAY_IN_LEDGERS);
    client.set_sealer(&Address::generate(&env));
    assert_eq!(instance_ttl(&env, &client), max);
}

#[test]
fn ttl_is_clamped_to_a_lower_network_maximum() {
    let env = Env::default();
    env.mock_all_auths();
    let low_max = 30 * DAY_IN_LEDGERS;
    ledger_with_limits(&env, low_max);
    let (client, _admin, sealer) = setup(&env);
    let max = env.storage().max_ttl();
    assert!(max < EXTEND_TO);

    let job = seal_one(&env, &client, &sealer, 1);
    assert_eq!(job_ttl(&env, &client, &job), max);
    assert_eq!(instance_ttl(&env, &client), max);
}

#[test]
fn ttl_an_untouched_seal_stays_live_past_the_minimum_lifetime() {
    // D-083 itself: the live registry's seals and instance lived only the
    // network minimum (7 days) and were then archived. A seal now stays live
    // for 170 days with nothing reading it. (The test host auto-restores an
    // archived entry on access, as protocol 23 transactions do, so the check
    // is on the lifetime itself, taken before anything touches the seal.)
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_limits(&env, MAX_ENTRY_TTL);
    let (client, _admin, sealer) = setup(&env);
    let max = env.storage().max_ttl();
    let job = seal_one(&env, &client, &sealer, 1);

    let untouched = 170 * DAY_IN_LEDGERS;
    assert!(untouched > MIN_PERSISTENT_TTL);
    advance_ledgers(&env, untouched);
    assert_eq!(job_ttl(&env, &client, &job), max - untouched);
    assert_eq!(instance_ttl(&env, &client), max - untouched);
    assert_eq!(client.get(&job).total_spent, 10);
}
