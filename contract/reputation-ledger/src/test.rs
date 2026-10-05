#![cfg(test)]

use crate::{DataKey, Error, ReputationLedger, ReputationLedgerClient};
use orizon_shared::ttl::DAY_IN_LEDGERS;
use soroban_sdk::{
    symbol_short,
    testutils::{
        storage::{Instance as _, Persistent as _},
        Address as _, Ledger, LedgerInfo,
    },
    Address, BytesN, Env, Symbol,
};

const EPOCH_SECONDS: u64 = 604_800;
const MAX_WEIGHT: i128 = 1_000_000_000;

fn setup(env: &Env) -> (ReputationLedgerClient<'_>, Address, Address) {
    let admin = Address::generate(env);
    let scorer = Address::generate(env);
    let id = env.register(ReputationLedger, (admin.clone(), scorer.clone()));
    (ReputationLedgerClient::new(env, &id), admin, scorer)
}

fn job(env: &Env, n: u8) -> BytesN<16> {
    BytesN::from_array(env, &[n; 16])
}

fn advance_epochs(env: &Env, n: u64) {
    env.ledger()
        .with_mut(|li| li.timestamp += n * EPOCH_SECONDS);
}

/// Replicate the contract's integer decay loop for exact expectations.
fn decay(mut v: i128, epochs: u64) -> i128 {
    for _ in 0..epochs {
        v = v * 925 / 1000;
    }
    v
}

#[test]
fn constructor_and_scorer_gating() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, scorer) = setup(&env);
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);

    let stranger = Address::generate(&env);
    let err = client.try_submit(
        &stranger,
        &agent,
        &job(&env, 1),
        &90,
        &1_000_000,
        &payer,
        &symbol_short!("rating"),
    );
    assert_eq!(err.err().unwrap().unwrap(), Error::Unauthorized);
    assert_eq!(client.rep_state(&agent).count, 0);

    // the scorer wired in by the constructor can submit
    client.submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &90,
        &1_000_000,
        &payer,
        &symbol_short!("rating"),
    );
    assert_eq!(client.rep_state(&agent).count, 1);
}

#[test]
fn set_scorer_swaps_authority() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, old_scorer) = setup(&env);
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);

    let new_scorer = Address::generate(&env);
    client.set_scorer(&new_scorer);

    let err = client.try_submit(
        &old_scorer,
        &agent,
        &job(&env, 1),
        &80,
        &1_000_000,
        &payer,
        &symbol_short!("rating"),
    );
    assert_eq!(err.err().unwrap().unwrap(), Error::Unauthorized);

    client.submit(
        &new_scorer,
        &agent,
        &job(&env, 1),
        &80,
        &1_000_000,
        &payer,
        &symbol_short!("rating"),
    );
    assert_eq!(client.rep_state(&agent).count, 1);
}

#[test]
fn rejects_out_of_range_inputs() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, scorer) = setup(&env);
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);
    let kind = symbol_short!("rating");

    // rating above 100
    let err = client.try_submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &101,
        &1_000_000,
        &payer,
        &kind,
    );
    assert_eq!(err.err().unwrap().unwrap(), Error::OutOfRange);

    // zero, negative, and above-cap weights
    let err = client.try_submit(&scorer, &agent, &job(&env, 1), &50, &0, &payer, &kind);
    assert_eq!(err.err().unwrap().unwrap(), Error::OutOfRange);
    let err = client.try_submit(&scorer, &agent, &job(&env, 1), &50, &-1, &payer, &kind);
    assert_eq!(err.err().unwrap().unwrap(), Error::OutOfRange);
    let err = client.try_submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &50,
        &(MAX_WEIGHT + 1),
        &payer,
        &kind,
    );
    assert_eq!(err.err().unwrap().unwrap(), Error::OutOfRange);

    // boundary values are accepted
    client.submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &100,
        &MAX_WEIGHT,
        &payer,
        &kind,
    );
    assert_eq!(client.rep_state(&agent).count, 1);
}

#[test]
fn replay_guard_is_persistent() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, scorer) = setup(&env);
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);
    let kind = symbol_short!("rating");

    client.submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &90,
        &1_000_000,
        &payer,
        &kind,
    );

    // same (agent, job) again → Replay
    let err = client.try_submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &10,
        &1_000_000,
        &payer,
        &kind,
    );
    assert_eq!(err.err().unwrap().unwrap(), Error::Replay);

    // a different job is fine
    client.submit(
        &scorer,
        &agent,
        &job(&env, 2),
        &90,
        &1_000_000,
        &payer,
        &kind,
    );

    // the guard survives long timestamp advances (persistent storage — the
    // v1 temporary-storage marker would have lapsed after hours)
    advance_epochs(&env, 12);
    let err = client.try_submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &10,
        &1_000_000,
        &payer,
        &kind,
    );
    assert_eq!(err.err().unwrap().unwrap(), Error::Replay);
    let err = client.try_submit(
        &scorer,
        &agent,
        &job(&env, 2),
        &10,
        &1_000_000,
        &payer,
        &kind,
    );
    assert_eq!(err.err().unwrap().unwrap(), Error::Replay);
}

#[test]
fn weighted_average_accumulates() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, scorer) = setup(&env);
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);
    let kind = symbol_short!("rating");

    client.submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &100,
        &1_000_000,
        &payer,
        &kind,
    );
    client.submit(
        &scorer,
        &agent,
        &job(&env, 2),
        &50,
        &3_000_000,
        &payer,
        &kind,
    );

    let s = client.rep_state(&agent);
    assert_eq!(s.sum_w, 25_000_000_000); // 10_000×1e6 + 5_000×3e6
    assert_eq!(s.weight, 4_000_000);
    assert_eq!(s.count, 2);
    assert_eq!(s.disputed, 0);

    // weight-weighted mean: (10_000×1 + 5_000×3) / 4 = 6_250 bps
    assert_eq!(client.avg_bps(&agent), 6_250);
}

#[test]
fn rep_bps_smooths_toward_evidence() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, scorer) = setup(&env);
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);
    let kind = symbol_short!("rating");

    // no evidence: prior wins regardless of prior weight
    assert_eq!(client.rep_bps(&agent, &7_000, &0), 7_000);
    assert_eq!(client.rep_bps(&agent, &7_000, &1_000_000), 7_000);

    // one perfect rating with weight equal to the prior → midpoint
    client.submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &100,
        &1_000_000,
        &payer,
        &kind,
    );
    let mid = client.rep_bps(&agent, &5_000, &1_000_000);
    assert_eq!(mid, 7_500);

    // more evidence weight pulls the estimate toward the raw mean (10_000)
    for n in 2..=9u8 {
        client.submit(
            &scorer,
            &agent,
            &job(&env, n),
            &100,
            &1_000_000,
            &payer,
            &kind,
        );
    }
    let strong = client.rep_bps(&agent, &5_000, &1_000_000);
    assert_eq!(strong, 9_500); // (5_000×1e6 + 9e10) / 1e7
    assert!(strong > mid);
    assert!(strong < client.avg_bps(&agent));
}

#[test]
fn decay_shrinks_weight_but_not_average() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, scorer) = setup(&env);
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);
    let kind = symbol_short!("rating");

    client.submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &80,
        &100_000_000,
        &payer,
        &kind,
    );
    advance_epochs(&env, 4);

    let s = client.rep_state(&agent);
    let expected_weight = decay(100_000_000, 4);
    let expected_sum = decay(800_000_000_000, 4);
    assert_eq!(s.weight, expected_weight);
    assert_eq!(s.sum_w, expected_sum);
    assert_eq!(s.last_epoch, 4);
    assert_eq!(s.count, 1); // lifetime count is not decayed

    // ≈ 0.925^4 ≈ 0.732 of the original weight
    assert!(s.weight > 73_000_000 && s.weight < 74_000_000);

    // pure decay scales sum and weight together — the mean is unchanged
    assert_eq!(client.avg_bps(&agent), 8_000);

    // a fresh, heavier rating dominates the decayed residue
    client.submit(
        &scorer,
        &agent,
        &job(&env, 2),
        &20,
        &MAX_WEIGHT,
        &payer,
        &kind,
    );
    let expected_avg =
        ((expected_sum + 2_000 * MAX_WEIGHT) / (expected_weight + MAX_WEIGHT)) as u32;
    assert_eq!(client.avg_bps(&agent), expected_avg);
    assert!(expected_avg > 2_000 && expected_avg < 2_500); // near the new rating, far from 8_000
}

#[test]
fn full_forgetting_after_max_epochs() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, scorer) = setup(&env);
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);

    client.submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &90,
        &500_000_000,
        &payer,
        &symbol_short!("rating"),
    );
    advance_epochs(&env, 96);

    let s = client.rep_state(&agent);
    assert_eq!(s.weight, 0);
    assert_eq!(s.sum_w, 0);
    assert_eq!(s.count, 1); // lifetime counters survive full forgetting
    assert_eq!(client.avg_bps(&agent), 0);

    // with no surviving evidence the smoothed estimate is exactly the prior
    assert_eq!(client.rep_bps(&agent, &6_000, &5_000_000), 6_000);
    assert_eq!(client.rep_bps(&agent, &6_000, &0), 6_000);
}

#[test]
fn dispute_kind_tracks_dispute_rate() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, scorer) = setup(&env);
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);

    for n in 1..=3u8 {
        client.submit(
            &scorer,
            &agent,
            &job(&env, n),
            &90,
            &1_000_000,
            &payer,
            &symbol_short!("rating"),
        );
    }
    client.submit(
        &scorer,
        &agent,
        &job(&env, 4),
        &0,
        &1_000_000,
        &payer,
        &symbol_short!("dispute"),
    );

    let s = client.rep_state(&agent);
    assert_eq!(s.count, 4);
    assert_eq!(s.disputed, 1);
    assert_eq!(client.dispute_rate_bps(&agent), 2_500); // 1 / 4

    // agent with no ratings → 0, not a division panic
    assert_eq!(client.dispute_rate_bps(&symbol_short!("nobody")), 0);
}

#[test]
fn payer_weight_accumulates_per_payer() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin, scorer) = setup(&env);
    let agent = symbol_short!("copy_v3");
    let kind = symbol_short!("rating");
    let payer_a = Address::generate(&env);
    let payer_b = Address::generate(&env);

    client.submit(
        &scorer,
        &agent,
        &job(&env, 1),
        &90,
        &1_000_000,
        &payer_a,
        &kind,
    );
    client.submit(
        &scorer,
        &agent,
        &job(&env, 2),
        &70,
        &2_000_000,
        &payer_a,
        &kind,
    );
    client.submit(
        &scorer,
        &agent,
        &job(&env, 3),
        &50,
        &5_000_000,
        &payer_b,
        &kind,
    );

    assert_eq!(client.payer_weight(&agent, &payer_a), 3_000_000);
    assert_eq!(client.payer_weight(&agent, &payer_b), 5_000_000);
    assert_eq!(client.payer_weight(&agent, &Address::generate(&env)), 0);

    // cumulative payer stake is never decayed
    advance_epochs(&env, 20);
    assert_eq!(client.payer_weight(&agent, &payer_a), 3_000_000);
}

// ── storage lifetime (D-083) ──────────────────────────────────────────
//
// The live ledger never extended anything, so its instance, wasm and every
// rating record lived only the network minimum (7 days) past their last
// write. These run on testnet's real limits.

fn ledger_with_testnet_limits(env: &Env) {
    env.ledger().set(LedgerInfo {
        timestamp: 1_000,
        protocol_version: 25,
        sequence_number: 100,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 120_960,
        max_entry_ttl: 3_110_400,
    });
}

fn age(env: &Env, days: u32) {
    env.ledger()
        .with_mut(|li| li.sequence_number += days * DAY_IN_LEDGERS);
}

fn ttl_of(env: &Env, client: &ReputationLedgerClient, key: &DataKey) -> u32 {
    env.as_contract(&client.address, || env.storage().persistent().get_ttl(key))
}

fn instance_ttl(env: &Env, client: &ReputationLedgerClient) -> u32 {
    env.as_contract(&client.address, || env.storage().instance().get_ttl())
}

/// One rating; returns the three keys it writes: Rep, Rated, PayerW.
fn rate(
    env: &Env,
    client: &ReputationLedgerClient,
    scorer: &Address,
    agent: &Symbol,
    payer: &Address,
    n: u8,
) -> [DataKey; 3] {
    client.submit(
        scorer,
        agent,
        &job(env, n),
        &90,
        &1_000_000,
        payer,
        &symbol_short!("rating"),
    );
    [
        DataKey::Rep(agent.clone()),
        DataKey::Rated(agent.clone(), job(env, n)),
        DataKey::PayerW(agent.clone(), payer.clone()),
    ]
}

#[test]
fn ttl_deploy_and_submit_extend_to_the_network_maximum() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_testnet_limits(&env);
    let (client, _admin, scorer) = setup(&env);
    let max = env.storage().max_ttl();
    assert_eq!(instance_ttl(&env, &client), max);

    age(&env, 2);
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);
    for key in rate(&env, &client, &scorer, &agent, &payer, 1) {
        assert_eq!(ttl_of(&env, &client, &key), max);
    }
    assert_eq!(instance_ttl(&env, &client), max);

    // A later rating re-extends the agent's shared records; the first job's
    // replay marker is left to age.
    age(&env, 2);
    let [rep, _, payer_w] = rate(&env, &client, &scorer, &agent, &payer, 2);
    assert_eq!(ttl_of(&env, &client, &rep), max);
    assert_eq!(ttl_of(&env, &client, &payer_w), max);
    assert_eq!(
        ttl_of(&env, &client, &DataKey::Rated(agent.clone(), job(&env, 1))),
        max - 2 * DAY_IN_LEDGERS
    );
}

#[test]
fn ttl_reads_re_extend_the_score_and_the_instance() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_testnet_limits(&env);
    let (client, _admin, scorer) = setup(&env);
    let max = env.storage().max_ttl();
    let agent = symbol_short!("copy_v3");
    let payer = Address::generate(&env);
    let [rep, _, payer_w] = rate(&env, &client, &scorer, &agent, &payer, 1);

    // Every score view goes through the same read.
    age(&env, 2);
    client.rep_state(&agent);
    assert_eq!(ttl_of(&env, &client, &rep), max);
    assert_eq!(instance_ttl(&env, &client), max);
    age(&env, 2);
    client.avg_bps(&agent);
    assert_eq!(ttl_of(&env, &client, &rep), max);
    age(&env, 2);
    client.rep_bps(&agent, &7_000, &120_000_000);
    assert_eq!(ttl_of(&env, &client, &rep), max);
    age(&env, 2);
    client.dispute_rate_bps(&agent);
    assert_eq!(ttl_of(&env, &client, &rep), max);

    assert_eq!(ttl_of(&env, &client, &payer_w), max - 8 * DAY_IN_LEDGERS);
    client.payer_weight(&agent, &payer);
    assert_eq!(ttl_of(&env, &client, &payer_w), max);

    // Agents and payers with no records read as zero, extending nothing.
    let nobody = symbol_short!("nobody");
    assert_eq!(client.rep_state(&nobody).count, 0);
    assert_eq!(client.payer_weight(&nobody, &payer), 0);

    // Inside the renew window a read pays no rent.
    env.ledger()
        .with_mut(|li| li.sequence_number += DAY_IN_LEDGERS - 1);
    client.avg_bps(&agent);
    assert_eq!(ttl_of(&env, &client, &rep), max - (DAY_IN_LEDGERS - 1));
}

#[test]
fn ttl_set_scorer_extends_the_instance() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_testnet_limits(&env);
    let (client, _admin, _scorer) = setup(&env);
    let max = env.storage().max_ttl();

    age(&env, 2);
    client.set_scorer(&Address::generate(&env));
    assert_eq!(instance_ttl(&env, &client), max);
}

#[test]
fn ttl_untouched_ratings_stay_live_past_the_minimum_lifetime() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_testnet_limits(&env);
    let (client, _admin, scorer) = setup(&env);
    let max = env.storage().max_ttl();
    let payer = Address::generate(&env);
    let keys = rate(&env, &client, &scorer, &symbol_short!("copy_v3"), &payer, 1);

    age(&env, 170);
    for key in keys {
        assert_eq!(ttl_of(&env, &client, &key), max - 170 * DAY_IN_LEDGERS);
    }
    assert_eq!(instance_ttl(&env, &client), max - 170 * DAY_IN_LEDGERS);
}
