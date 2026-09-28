#![cfg(test)]

use crate::{AttestationRegistry, AttestationRegistryClient, Error};
use soroban_sdk::{symbol_short, testutils::Address as _, vec, Address, BytesN, Env};

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
