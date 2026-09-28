#![cfg(test)]

use crate::{AgentRegistry, AgentRegistryClient, Error};
use soroban_sdk::{symbol_short, testutils::Address as _, vec, Address, Env, String};

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
