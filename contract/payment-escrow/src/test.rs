#![cfg(test)]

use crate::{Error, PaymentEscrow, PaymentEscrowClient};
use orizon_agent_registry::{AgentRegistry, AgentRegistryClient};
use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger as _, LedgerInfo},
    token, vec, Address, Env, String,
};

fn setup(env: &Env) -> Fixture {
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(env);
    let payer = Address::generate(env);
    let agent_owner = Address::generate(env);

    // USDC — use a stock SAC token issued by a fresh admin.
    let usdc_admin = Address::generate(env);
    let usdc_sac = env.register_stellar_asset_contract_v2(usdc_admin.clone());
    let usdc_id: Address = usdc_sac.address();
    let usdc_admin_client = token::StellarAssetClient::new(env, &usdc_id);
    let usdc_client = token::TokenClient::new(env, &usdc_id);
    // Fund payer with 10 USDC (7 decimals)
    usdc_admin_client.mint(&payer, &100_000_000);

    // Registry
    let reg_id = env.register(AgentRegistry, (admin.clone(),));
    let reg = AgentRegistryClient::new(env, &reg_id);
    let agent_id = symbol_short!("copy_v3");
    reg.register(
        &agent_owner,
        &agent_id,
        &String::from_str(env, "copywrite.v3"),
        &vec![env, symbol_short!("copy")],
        &120_000,
    );

    // Escrow
    let settler = admin.clone();
    let esc_id = env.register(
        PaymentEscrow,
        (
            admin.clone(),
            usdc_id.clone(),
            reg_id.clone(),
            settler.clone(),
        ),
    );
    let escrow = PaymentEscrowClient::new(env, &esc_id);

    Fixture {
        admin,
        payer,
        agent_owner,
        agent_id,
        usdc_id,
        usdc_client,
        escrow,
    }
}

struct Fixture<'a> {
    admin: Address,
    payer: Address,
    agent_owner: Address,
    agent_id: soroban_sdk::Symbol,
    usdc_id: Address,
    usdc_client: token::TokenClient<'a>,
    escrow: PaymentEscrowClient<'a>,
}

#[test]
fn authorize_charge_receipt() {
    let env = Env::default();
    env.ledger().set(LedgerInfo {
        timestamp: 1_000,
        protocol_version: 25,
        sequence_number: 1,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 10_000,
    });
    let f = setup(&env);

    // payer pre-authorizes 0.5 USDC (5_000_000 stroops) for the copy agent
    let auth_id = f
        .escrow
        .authorize(&f.payer, &f.agent_id, &5_000_000, &9_999);

    // settler (admin in test) charges 0.012 USDC of it
    let receipt_id = f.escrow.charge(
        &f.admin,
        &auth_id,
        &120_000,
        &soroban_sdk::BytesN::from_array(&env, &[7u8; 16]),
    );

    let r = f.escrow.receipt(&receipt_id);
    assert_eq!(r.amount, 120_000);
    assert_eq!(r.agent_id, f.agent_id);

    let a = f.escrow.authorization(&auth_id);
    assert_eq!(a.spent, 120_000);

    // USDC moved
    assert_eq!(f.usdc_client.balance(&f.agent_owner), 120_000);
    assert_eq!(f.usdc_client.balance(&f.payer), 100_000_000 - 120_000);
}

#[test]
fn cannot_overdraw_authorization() {
    let env = Env::default();
    env.ledger().set(LedgerInfo {
        timestamp: 1_000,
        protocol_version: 25,
        sequence_number: 1,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 10_000,
    });
    let f = setup(&env);
    let auth_id = f.escrow.authorize(&f.payer, &f.agent_id, &100_000, &9_999);
    let err = f.escrow.try_charge(
        &f.admin,
        &auth_id,
        &200_000,
        &soroban_sdk::BytesN::from_array(&env, &[1u8; 16]),
    );
    assert_eq!(err.err().unwrap().unwrap(), Error::Insufficient);
}
