#![cfg(test)]

use crate::{AgentRegistry, AgentRegistryClient, DataKey, Error};
use orizon_shared::ttl::DAY_IN_LEDGERS;
use soroban_sdk::{
    symbol_short,
    testutils::{
        storage::{Instance as _, Persistent as _},
        Address as _, Ledger as _, LedgerInfo,
    },
    vec, Address, Env, String, Symbol,
};

fn setup(env: &Env) -> (AgentRegistryClient<'_>, Address) {
    let admin = Address::generate(env);
    let id = env.register(AgentRegistry, (admin.clone(),));
    (AgentRegistryClient::new(env, &id), admin)
}

#[test]
fn register_then_read() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = setup(&env);
    let owner = Address::generate(&env);

    client.register(
        &owner,
        &symbol_short!("copy_v3"),
        &String::from_str(&env, "copywrite.v3"),
        &vec![&env, symbol_short!("copy"), symbol_short!("en")],
        &120_000, // 0.012 USDC (7 decimals)
    );

    let a = client.get(&symbol_short!("copy_v3"));
    assert_eq!(a.owner, owner);
    assert_eq!(a.price, 120_000);
    assert!(a.active);

    let ids = client.list_ids();
    assert_eq!(ids.len(), 1);
}

#[test]
fn cannot_register_twice() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = setup(&env);
    let owner = Address::generate(&env);
    let id = symbol_short!("copy_v3");

    client.register(
        &owner,
        &id,
        &String::from_str(&env, "copywrite.v3"),
        &vec![&env, symbol_short!("copy")],
        &120_000,
    );

    let err = client.try_register(
        &owner,
        &id,
        &String::from_str(&env, "copywrite.v3"),
        &vec![&env, symbol_short!("copy")],
        &120_000,
    );
    assert_eq!(err.err().unwrap().unwrap(), Error::AlreadyExists);
}

#[test]
fn update_price_and_deactivate() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _admin) = setup(&env);
    let owner = Address::generate(&env);
    let id = symbol_short!("seo_b");

    client.register(
        &owner,
        &id,
        &String::from_str(&env, "seo.brief"),
        &vec![&env, symbol_short!("seo")],
        &90_000,
    );

    client.update_price(&id, &180_000);
    assert_eq!(client.get(&id).price, 180_000);

    client.set_active(&id, &false);
    assert!(!client.get(&id).active);
}

// ── storage lifetime (D-083) ──────────────────────────────────────────
//
// The live registry never extended anything, so its instance (which holds
// the id list), wasm and agent records lived only the network minimum
// (7 days) past their last write. These run on testnet's real limits.

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

fn agent_ttl(env: &Env, client: &AgentRegistryClient, id: &Symbol) -> u32 {
    env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .get_ttl(&DataKey::Agent(id.clone()))
    })
}

fn instance_ttl(env: &Env, client: &AgentRegistryClient) -> u32 {
    env.as_contract(&client.address, || env.storage().instance().get_ttl())
}

fn register_one(env: &Env, client: &AgentRegistryClient, id: &Symbol) -> Address {
    let owner = Address::generate(env);
    client.register(
        &owner,
        id,
        &String::from_str(env, "copywrite.v3"),
        &vec![env, symbol_short!("copy")],
        &120_000,
    );
    owner
}

#[test]
fn ttl_deploy_and_register_extend_to_the_network_maximum() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_testnet_limits(&env);
    let (client, _admin) = setup(&env);
    let max = env.storage().max_ttl();
    assert_eq!(instance_ttl(&env, &client), max);

    age(&env, 2);
    let id = symbol_short!("copy_v3");
    register_one(&env, &client, &id);
    assert_eq!(agent_ttl(&env, &client, &id), max);
    assert_eq!(instance_ttl(&env, &client), max);
}

#[test]
fn ttl_owner_updates_re_extend_the_agent_and_the_instance() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_testnet_limits(&env);
    let (client, _admin) = setup(&env);
    let max = env.storage().max_ttl();
    let id = symbol_short!("copy_v3");
    register_one(&env, &client, &id);

    age(&env, 2);
    assert_eq!(agent_ttl(&env, &client, &id), max - 2 * DAY_IN_LEDGERS);
    client.update_price(&id, &150_000);
    assert_eq!(agent_ttl(&env, &client, &id), max);
    assert_eq!(instance_ttl(&env, &client), max);

    age(&env, 2);
    client.set_active(&id, &false);
    assert_eq!(agent_ttl(&env, &client, &id), max);
    assert_eq!(instance_ttl(&env, &client), max);
}

#[test]
fn ttl_reads_re_extend_the_agent_and_the_instance() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_testnet_limits(&env);
    let (client, _admin) = setup(&env);
    let max = env.storage().max_ttl();
    let id = symbol_short!("copy_v3");
    register_one(&env, &client, &id);

    age(&env, 2);
    client.get(&id);
    assert_eq!(agent_ttl(&env, &client, &id), max);
    assert_eq!(instance_ttl(&env, &client), max);

    // `owner_of` is what the escrow calls at settle.
    age(&env, 2);
    client.owner_of(&id);
    assert_eq!(agent_ttl(&env, &client, &id), max);

    age(&env, 2);
    client.list_ids();
    assert_eq!(instance_ttl(&env, &client), max);

    age(&env, 2);
    client.admin();
    assert_eq!(instance_ttl(&env, &client), max);

    // Inside the renew window a read pays no rent.
    env.ledger()
        .with_mut(|li| li.sequence_number += DAY_IN_LEDGERS - 1);
    client.get(&id);
    assert_eq!(instance_ttl(&env, &client), max - (DAY_IN_LEDGERS - 1));
}

#[test]
fn ttl_an_untouched_agent_stays_live_past_the_minimum_lifetime() {
    let env = Env::default();
    env.mock_all_auths();
    ledger_with_testnet_limits(&env);
    let (client, _admin) = setup(&env);
    let max = env.storage().max_ttl();
    let id = symbol_short!("copy_v3");
    register_one(&env, &client, &id);

    age(&env, 170);
    assert_eq!(agent_ttl(&env, &client, &id), max - 170 * DAY_IN_LEDGERS);
    assert_eq!(instance_ttl(&env, &client), max - 170 * DAY_IN_LEDGERS);
}
