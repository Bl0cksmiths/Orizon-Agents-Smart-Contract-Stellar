#![cfg(test)]
//! PaymentEscrow v2 tests.
//!
//! No test here uses `mock_all_auths*`. v1's tests passed under
//! `mock_all_auths_allowing_non_root_auth()` while the network rejected every
//! charge, so every call below, set-up included, runs in enforcing mode with
//! the exact authorization tree spelled out through `env.mock_auths`, and the
//! money paths assert the recorded tree with `env.auths()`.

extern crate std;

use crate::{
    DataKey, Error, PaymentEscrow, PaymentEscrowClient, Payout, DAY_IN_LEDGERS, ENTRY_EXTEND_TO,
    INSTANCE_EXTEND_TO, MAX_PAYOUTS,
};
use orizon_agent_registry::{AgentRegistry, AgentRegistryClient};
use soroban_sdk::{
    symbol_short,
    testutils::{
        storage::{Instance as _, Persistent as _},
        Address as _, AuthorizedFunction, AuthorizedInvocation, Events as _, Ledger as _,
        LedgerInfo, MockAuth, MockAuthInvoke,
    },
    token, vec, Address, BytesN, ConversionError, Env, IntoVal, InvokeError, String, Symbol, Val,
    Vec,
};

const T0: u64 = 1_000;
const EXPIRES: u64 = 2_000;
const FUND: i128 = 100_000_000;
const MAX: i128 = 5_000_000;
const PRICE_A: i128 = 1_200_000;
const PRICE_B: i128 = 800_000;
const MAX_ENTRY_TTL: u32 = 3_110_400;
/// High enough that the registry's and the asset's own entries outlive the
/// few simulated days the TTL test advances the ledger by.
const MIN_PERSISTENT_TTL: u32 = 500_000;

/// What a generated `try_*` client method returns.
type TryResult<T, C = ConversionError> = Result<Result<T, C>, Result<Error, InvokeError>>;

struct F {
    env: Env,
    admin: Address,
    settler: Address,
    payer: Address,
    stranger: Address,
    owner_a: Address,
    owner_b: Address,
    agent_a: Symbol,
    agent_b: Symbol,
    label: Symbol,
    usdc_id: Address,
    usdc: token::TokenClient<'static>,
    escrow_id: Address,
    escrow: PaymentEscrowClient<'static>,
}

fn setup() -> F {
    let env = Env::default();
    env.ledger().set(LedgerInfo {
        timestamp: T0,
        protocol_version: 25,
        sequence_number: 100,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: MIN_PERSISTENT_TTL,
        max_entry_ttl: MAX_ENTRY_TTL,
    });

    let admin = Address::generate(&env);
    let settler = Address::generate(&env);
    let payer = Address::generate(&env);
    let stranger = Address::generate(&env);
    let owner_a = Address::generate(&env);
    let owner_b = Address::generate(&env);

    // A real Stellar Asset Contract; the mint is authorized by its admin only.
    let usdc_admin = Address::generate(&env);
    let usdc_id = env
        .register_stellar_asset_contract_v2(usdc_admin.clone())
        .address();
    env.mock_auths(&[MockAuth {
        address: &usdc_admin,
        invoke: &MockAuthInvoke {
            contract: &usdc_id,
            fn_name: "mint",
            args: (&payer, FUND).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    token::StellarAssetClient::new(&env, &usdc_id).mint(&payer, &FUND);

    // The real AgentRegistry, two agents with two different owners.
    let registry_id = env.register(AgentRegistry, (admin.clone(),));
    let agent_a = symbol_short!("copy_v3");
    let agent_b = symbol_short!("code_v1");
    register_agent(&env, &registry_id, &owner_a, &agent_a, PRICE_A);
    register_agent(&env, &registry_id, &owner_b, &agent_b, PRICE_B);

    let escrow_id = env.register(
        PaymentEscrow,
        (
            admin.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            settler.clone(),
        ),
    );

    F {
        label: Symbol::new(&env, "orizon_batch"),
        usdc: token::TokenClient::new(&env, &usdc_id),
        escrow: PaymentEscrowClient::new(&env, &escrow_id),
        env,
        admin,
        settler,
        payer,
        stranger,
        owner_a,
        owner_b,
        agent_a,
        agent_b,
        usdc_id,
        escrow_id,
    }
}

fn register_agent(env: &Env, registry_id: &Address, owner: &Address, id: &Symbol, price: i128) {
    let name = String::from_str(env, "agent");
    let skills = vec![env, symbol_short!("skill")];
    env.mock_auths(&[MockAuth {
        address: owner,
        invoke: &MockAuthInvoke {
            contract: registry_id,
            fn_name: "register",
            args: (owner, id.clone(), name.clone(), skills.clone(), price).into_val(env),
            sub_invokes: &[],
        },
    }]);
    AgentRegistryClient::new(env, registry_id).register(owner, id, &name, &skills, &price);
}

// ── Explicit authorization trees ──────────────────────────────────────

/// `signer` signs `authorize(payer, …)` with the nested custody `transfer`
/// beneath it: the one entry a real payer's wallet signs.
fn mock_payer_authorize(f: &F, signer: &Address, max: i128, expires_at: u64) {
    let transfer = [MockAuthInvoke {
        contract: &f.usdc_id,
        fn_name: "transfer",
        args: (&f.payer, &f.escrow_id, max).into_val(&f.env),
        sub_invokes: &[],
    }];
    f.env.mock_auths(&[MockAuth {
        address: signer,
        invoke: &MockAuthInvoke {
            contract: &f.escrow_id,
            fn_name: "authorize",
            args: (&f.payer, f.label.clone(), max, expires_at).into_val(&f.env),
            sub_invokes: &transfer,
        },
    }]);
}

fn try_authorize(f: &F, max: i128, expires_at: u64) -> TryResult<BytesN<16>> {
    mock_payer_authorize(f, &f.payer, max, expires_at);
    f.escrow
        .try_authorize(&f.payer, &f.label, &max, &expires_at)
}

fn authorize(f: &F) -> BytesN<16> {
    try_authorize(f, MAX, EXPIRES).unwrap().unwrap()
}

/// `signer` signs `settle(caller, …)` with no sub-invocations: the payouts
/// move out of the contract's own custody, so nobody else signs.
fn try_settle_signed(
    f: &F,
    signer: &Address,
    caller: &Address,
    auth_id: &BytesN<16>,
    payouts: &Vec<Payout>,
) -> TryResult<Vec<BytesN<16>>> {
    let job_id = job(&f.env);
    f.env.mock_auths(&[MockAuth {
        address: signer,
        invoke: &MockAuthInvoke {
            contract: &f.escrow_id,
            fn_name: "settle",
            args: (caller, auth_id.clone(), job_id.clone(), payouts.clone()).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    f.escrow.try_settle(caller, auth_id, &job_id, payouts)
}

fn try_settle(
    f: &F,
    caller: &Address,
    auth_id: &BytesN<16>,
    payouts: &Vec<Payout>,
) -> TryResult<Vec<BytesN<16>>> {
    try_settle_signed(f, caller, caller, auth_id, payouts)
}

fn settle(f: &F, auth_id: &BytesN<16>, payouts: &Vec<Payout>) -> Vec<BytesN<16>> {
    try_settle(f, &f.settler, auth_id, payouts)
        .unwrap()
        .unwrap()
}

fn try_reclaim_signed(
    f: &F,
    signer: &Address,
    payer: &Address,
    auth_id: &BytesN<16>,
) -> TryResult<i128, soroban_sdk::Error> {
    f.env.mock_auths(&[MockAuth {
        address: signer,
        invoke: &MockAuthInvoke {
            contract: &f.escrow_id,
            fn_name: "reclaim",
            args: (payer, auth_id.clone()).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    f.escrow.try_reclaim(payer, auth_id)
}

fn try_reclaim(f: &F, auth_id: &BytesN<16>) -> TryResult<i128, soroban_sdk::Error> {
    try_reclaim_signed(f, &f.payer, &f.payer, auth_id)
}

fn try_set_settler_signed(f: &F, signer: &Address, new_settler: &Address) -> TryResult<()> {
    f.env.mock_auths(&[MockAuth {
        address: signer,
        invoke: &MockAuthInvoke {
            contract: &f.escrow_id,
            fn_name: "set_settler",
            args: (new_settler,).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    f.escrow.try_set_settler(new_settler)
}

// ── Small helpers ─────────────────────────────────────────────────────

fn job(env: &Env) -> BytesN<16> {
    BytesN::from_array(env, &[7u8; 16])
}

fn payout(agent_id: &Symbol, amount: i128) -> Payout {
    Payout {
        agent_id: agent_id.clone(),
        amount,
    }
}

fn contract_err<T: core::fmt::Debug, C: core::fmt::Debug>(r: TryResult<T, C>) -> Error {
    match r {
        Err(Ok(e)) => e,
        other => panic!("expected a contract error, got {other:?}"),
    }
}

/// A host-level failure: here, always an authorization the host refused.
fn assert_host_err<T: core::fmt::Debug, C: core::fmt::Debug>(r: TryResult<T, C>) {
    assert!(matches!(r, Err(Err(_))), "expected a host error, got {r:?}");
}

fn invocation(
    contract: &Address,
    fn_name: &str,
    args: Vec<Val>,
    sub_invocations: std::vec::Vec<AuthorizedInvocation>,
) -> AuthorizedInvocation {
    AuthorizedInvocation {
        function: AuthorizedFunction::Contract((
            contract.clone(),
            Symbol::new(contract.env(), fn_name),
            args,
        )),
        sub_invocations,
    }
}

/// One expected escrow event, as `ContractEvents` compares them.
fn event(f: &F, topics: Vec<Val>, data: Val) -> (Address, Vec<Val>, Val) {
    (f.escrow_id.clone(), topics, data)
}

fn escrow_events(f: &F) -> soroban_sdk::testutils::ContractEvents {
    f.env.events().all().filter_by_contract(&f.escrow_id)
}

fn set_time(env: &Env, timestamp: u64) {
    env.ledger().with_mut(|li| li.timestamp = timestamp);
}

fn advance_ledgers(env: &Env, ledgers: u32) {
    env.ledger().with_mut(|li| li.sequence_number += ledgers);
}

/// (payer, escrow, owner_a, owner_b)
fn balances(f: &F) -> (i128, i128, i128, i128) {
    (
        f.usdc.balance(&f.payer),
        f.usdc.balance(&f.escrow_id),
        f.usdc.balance(&f.owner_a),
        f.usdc.balance(&f.owner_b),
    )
}

fn entry_ttl(f: &F, key: &DataKey) -> u32 {
    f.env
        .as_contract(&f.escrow_id, || f.env.storage().persistent().get_ttl(key))
}

fn instance_ttl(f: &F) -> u32 {
    f.env
        .as_contract(&f.escrow_id, || f.env.storage().instance().get_ttl())
}

// ── authorize ─────────────────────────────────────────────────────────

#[test]
fn authorize_takes_custody_under_one_payer_signature() {
    let f = setup();
    let auth_id = authorize(&f);

    // The payer's single auth entry covers the root call and the nested transfer.
    assert_eq!(
        f.env.auths(),
        std::vec![(
            f.payer.clone(),
            invocation(
                &f.escrow_id,
                "authorize",
                (&f.payer, f.label.clone(), MAX, EXPIRES).into_val(&f.env),
                std::vec![invocation(
                    &f.usdc_id,
                    "transfer",
                    (&f.payer, &f.escrow_id, MAX).into_val(&f.env),
                    std::vec![],
                )],
            )
        )]
    );

    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));

    let a = f.escrow.authorization(&auth_id);
    assert_eq!(a.payer, f.payer);
    assert_eq!(a.agent_id, f.label);
    assert_eq!(a.max_amount, MAX);
    assert_eq!(a.spent, 0);
    assert_eq!(a.expires_at, EXPIRES);
    assert!(!a.revoked);
    assert!(!a.settled);
}

#[test]
fn authorize_event_is_unchanged_from_v1() {
    let f = setup();
    let auth_id = authorize(&f);
    assert_eq!(
        escrow_events(&f),
        vec![
            &f.env,
            event(
                &f,
                (symbol_short!("authd"), f.label.clone()).into_val(&f.env),
                (auth_id, f.payer.clone(), MAX).into_val(&f.env),
            ),
        ]
    );
}

#[test]
fn authorize_without_the_nested_transfer_auth_fails() {
    let f = setup();
    // The payer signs only the root call, not the custody transfer beneath it.
    f.env.mock_auths(&[MockAuth {
        address: &f.payer,
        invoke: &MockAuthInvoke {
            contract: &f.escrow_id,
            fn_name: "authorize",
            args: (&f.payer, f.label.clone(), MAX, EXPIRES).into_val(&f.env),
            sub_invokes: &[],
        },
    }]);
    assert_host_err(f.escrow.try_authorize(&f.payer, &f.label, &MAX, &EXPIRES));
    assert_eq!(balances(&f), (FUND, 0, 0, 0));
}

#[test]
fn authorize_signed_by_someone_else_fails() {
    let f = setup();
    mock_payer_authorize(&f, &f.stranger, MAX, EXPIRES);
    assert_host_err(f.escrow.try_authorize(&f.payer, &f.label, &MAX, &EXPIRES));
    assert_eq!(balances(&f), (FUND, 0, 0, 0));
}

#[test]
fn authorize_rejects_non_positive_amounts() {
    let f = setup();
    assert_eq!(
        contract_err(try_authorize(&f, 0, EXPIRES)),
        Error::BadAmount
    );
    assert_eq!(
        contract_err(try_authorize(&f, -1, EXPIRES)),
        Error::BadAmount
    );
    assert_eq!(balances(&f), (FUND, 0, 0, 0));
}

#[test]
fn authorize_rejects_an_expiry_not_in_the_future() {
    let f = setup();
    assert_eq!(contract_err(try_authorize(&f, MAX, T0)), Error::Expired);
    assert_eq!(contract_err(try_authorize(&f, MAX, T0 - 1)), Error::Expired);
    assert_eq!(balances(&f), (FUND, 0, 0, 0));
    // One second ahead is enough.
    try_authorize(&f, MAX, T0 + 1).unwrap().unwrap();
    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));
}

// ── settle ────────────────────────────────────────────────────────────

#[test]
fn settle_pays_two_owners_and_returns_the_remainder() {
    let f = setup();
    let auth_id = authorize(&f);
    let payouts = vec![
        &f.env,
        payout(&f.agent_a, PRICE_A),
        payout(&f.agent_b, PRICE_B),
    ];
    let receipts = settle(&f, &auth_id, &payouts);

    // Only the settler signed; the payouts need nobody else's signature.
    assert_eq!(
        f.env.auths(),
        std::vec![(
            f.settler.clone(),
            invocation(
                &f.escrow_id,
                "settle",
                (&f.settler, auth_id.clone(), job(&f.env), payouts.clone()).into_val(&f.env),
                std::vec![],
            )
        )]
    );

    let returned = MAX - PRICE_A - PRICE_B;
    assert_eq!(balances(&f), (FUND - MAX + returned, 0, PRICE_A, PRICE_B));

    assert_eq!(receipts.len(), 2);
    let r0 = f.escrow.receipt(&receipts.get(0).unwrap());
    assert_eq!(r0.auth_id, auth_id);
    assert_eq!(r0.agent_id, f.agent_a);
    assert_eq!(r0.amount, PRICE_A);
    assert_eq!(r0.job_id, job(&f.env));
    assert_eq!(r0.settled_at, T0);
    let r1 = f.escrow.receipt(&receipts.get(1).unwrap());
    assert_eq!(r1.auth_id, auth_id);
    assert_eq!(r1.agent_id, f.agent_b);
    assert_eq!(r1.amount, PRICE_B);

    let a = f.escrow.authorization(&auth_id);
    assert_eq!(a.spent, PRICE_A + PRICE_B);
    assert!(a.settled);
    assert!(!a.revoked);
}

#[test]
fn settle_events_keep_charged_v1_shaped() {
    let f = setup();
    let auth_id = authorize(&f);
    let payouts = vec![
        &f.env,
        payout(&f.agent_a, PRICE_A),
        payout(&f.agent_b, PRICE_B),
    ];
    let receipts = settle(&f, &auth_id, &payouts);
    let job_id = job(&f.env);
    let spent = PRICE_A + PRICE_B;
    assert_eq!(
        escrow_events(&f),
        vec![
            &f.env,
            // ("charged", agent actually paid) -> (receipt_id, auth_id, amount, job_id)
            event(
                &f,
                (symbol_short!("charged"), f.agent_a.clone()).into_val(&f.env),
                (
                    receipts.get(0).unwrap(),
                    auth_id.clone(),
                    PRICE_A,
                    job_id.clone()
                )
                    .into_val(&f.env),
            ),
            event(
                &f,
                (symbol_short!("charged"), f.agent_b.clone()).into_val(&f.env),
                (
                    receipts.get(1).unwrap(),
                    auth_id.clone(),
                    PRICE_B,
                    job_id.clone()
                )
                    .into_val(&f.env),
            ),
            // ("settled",) -> (auth_id, job_id, spent, returned)
            event(
                &f,
                (symbol_short!("settled"),).into_val(&f.env),
                (auth_id, job_id, spent, MAX - spent).into_val(&f.env),
            ),
        ]
    );
}

#[test]
fn settle_with_no_payouts_is_a_full_release() {
    let f = setup();
    let auth_id = authorize(&f);
    let receipts = settle(&f, &auth_id, &Vec::new(&f.env));
    let events = escrow_events(&f);
    assert_eq!(receipts.len(), 0);
    assert_eq!(balances(&f), (FUND, 0, 0, 0));
    assert_eq!(
        events,
        vec![
            &f.env,
            event(
                &f,
                (symbol_short!("settled"),).into_val(&f.env),
                (auth_id.clone(), job(&f.env), 0i128, MAX).into_val(&f.env),
            ),
        ]
    );

    let a = f.escrow.authorization(&auth_id);
    assert!(a.settled);
    assert_eq!(a.spent, 0);
}

#[test]
fn settle_of_exactly_max_returns_nothing() {
    let f = setup();
    let auth_id = authorize(&f);
    let payouts = vec![
        &f.env,
        payout(&f.agent_a, MAX - PRICE_B),
        payout(&f.agent_b, PRICE_B),
    ];
    let receipts = settle(&f, &auth_id, &payouts);
    // Only the two owners were paid: two SAC transfers, no remainder transfer.
    let events = escrow_events(&f);
    let sac_events = f.env.events().all().filter_by_contract(&f.usdc_id);
    assert_eq!(sac_events.events().len(), 2);
    assert_eq!(balances(&f), (FUND - MAX, 0, MAX - PRICE_B, PRICE_B));
    assert_eq!(f.escrow.authorization(&auth_id).spent, MAX);

    let job_id = job(&f.env);
    assert_eq!(
        events,
        vec![
            &f.env,
            event(
                &f,
                (symbol_short!("charged"), f.agent_a.clone()).into_val(&f.env),
                (
                    receipts.get(0).unwrap(),
                    auth_id.clone(),
                    MAX - PRICE_B,
                    job_id.clone()
                )
                    .into_val(&f.env),
            ),
            event(
                &f,
                (symbol_short!("charged"), f.agent_b.clone()).into_val(&f.env),
                (
                    receipts.get(1).unwrap(),
                    auth_id.clone(),
                    PRICE_B,
                    job_id.clone()
                )
                    .into_val(&f.env),
            ),
            event(
                &f,
                (symbol_short!("settled"),).into_val(&f.env),
                (auth_id, job_id, MAX, 0i128).into_val(&f.env),
            ),
        ]
    );
}

#[test]
fn settle_past_max_is_insufficient() {
    let f = setup();
    let auth_id = authorize(&f);
    let payouts = vec![
        &f.env,
        payout(&f.agent_a, MAX - PRICE_B),
        payout(&f.agent_b, PRICE_B + 1),
    ];
    assert_eq!(
        contract_err(try_settle(&f, &f.settler, &auth_id, &payouts)),
        Error::Insufficient
    );
    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));
    assert!(!f.escrow.authorization(&auth_id).settled);
}

#[test]
fn settle_accepts_sixteen_payouts_and_refuses_seventeen() {
    let f = setup();
    let auth_id = authorize(&f);

    let mut payouts = Vec::new(&f.env);
    for _ in 0..=MAX_PAYOUTS {
        payouts.push_back(payout(&f.agent_a, 1));
    }
    assert_eq!(payouts.len(), 17);
    assert_eq!(
        contract_err(try_settle(&f, &f.settler, &auth_id, &payouts)),
        Error::BadPayouts
    );
    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));

    payouts.pop_back();
    let receipts = settle(&f, &auth_id, &payouts);
    assert_eq!(receipts.len(), 16);
    assert_eq!(balances(&f), (FUND - 16, 0, 16, 0));
}

#[test]
fn settle_rejects_zero_and_negative_payouts() {
    let f = setup();
    let auth_id = authorize(&f);
    for bad in [0i128, -1, -PRICE_A, i128::MIN] {
        let payouts = vec![&f.env, payout(&f.agent_a, PRICE_A), payout(&f.agent_b, bad)];
        assert_eq!(
            contract_err(try_settle(&f, &f.settler, &auth_id, &payouts)),
            Error::BadAmount
        );
    }
    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));
}

#[test]
fn settle_sum_overflow_is_bad_amount() {
    let f = setup();
    let auth_id = authorize(&f);
    let payouts = vec![
        &f.env,
        payout(&f.agent_a, i128::MAX),
        payout(&f.agent_b, i128::MAX),
    ];
    assert_eq!(
        contract_err(try_settle(&f, &f.settler, &auth_id, &payouts)),
        Error::BadAmount
    );
    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));
}

#[test]
fn settle_replay_is_refused() {
    let f = setup();
    let auth_id = authorize(&f);
    let payouts = vec![&f.env, payout(&f.agent_a, PRICE_A)];
    settle(&f, &auth_id, &payouts);
    let after_first = balances(&f);
    assert_eq!(
        contract_err(try_settle(&f, &f.settler, &auth_id, &payouts)),
        Error::Replay
    );
    assert_eq!(
        contract_err(try_settle(&f, &f.settler, &auth_id, &Vec::new(&f.env))),
        Error::Replay
    );
    assert_eq!(balances(&f), after_first);
}

#[test]
fn settle_is_allowed_at_expiry_and_refused_after() {
    let f = setup();
    let late = authorize(&f);
    let on_time = authorize(&f);
    let payouts = vec![&f.env, payout(&f.agent_a, PRICE_A)];

    set_time(&f.env, EXPIRES + 1);
    assert_eq!(
        contract_err(try_settle(&f, &f.settler, &late, &payouts)),
        Error::Expired
    );

    set_time(&f.env, EXPIRES);
    settle(&f, &on_time, &payouts);
    assert_eq!(f.usdc.balance(&f.owner_a), PRICE_A);
}

#[test]
fn settle_by_a_non_settler_is_unauthorized() {
    let f = setup();
    let auth_id = authorize(&f);
    let payouts = vec![&f.env, payout(&f.agent_a, PRICE_A)];
    // The stranger signs its own call, but is not the settler.
    assert_eq!(
        contract_err(try_settle(&f, &f.stranger, &auth_id, &payouts)),
        Error::Unauthorized
    );
    // Nor is the payer.
    assert_eq!(
        contract_err(try_settle(&f, &f.payer, &auth_id, &payouts)),
        Error::Unauthorized
    );
    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));
}

#[test]
fn settle_naming_the_settler_but_signed_by_another_fails() {
    let f = setup();
    let auth_id = authorize(&f);
    let payouts = vec![&f.env, payout(&f.agent_a, PRICE_A)];
    assert_host_err(try_settle_signed(
        &f,
        &f.stranger,
        &f.settler,
        &auth_id,
        &payouts,
    ));
    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));
}

#[test]
fn settle_of_an_unknown_authorization_is_not_found() {
    let f = setup();
    let missing = BytesN::from_array(&f.env, &[9u8; 16]);
    assert_eq!(
        contract_err(try_settle(&f, &f.settler, &missing, &Vec::new(&f.env))),
        Error::NotFound
    );
}

#[test]
fn settle_to_an_unregistered_agent_fails_whole() {
    let f = setup();
    let auth_id = authorize(&f);
    let payouts = vec![
        &f.env,
        payout(&f.agent_a, PRICE_A),
        payout(&symbol_short!("ghost"), PRICE_B),
    ];
    let r = try_settle(&f, &f.settler, &auth_id, &payouts);
    assert!(r.is_err(), "{r:?}");
    // Atomic: the first payout rolled back with the rest.
    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));
    assert!(!f.escrow.authorization(&auth_id).settled);
}

// ── reclaim ───────────────────────────────────────────────────────────

#[test]
fn reclaim_before_expiry_is_locked() {
    let f = setup();
    let auth_id = authorize(&f);
    assert_eq!(contract_err(try_reclaim(&f, &auth_id)), Error::Locked);
    set_time(&f.env, EXPIRES);
    assert_eq!(contract_err(try_reclaim(&f, &auth_id)), Error::Locked);
    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));
}

#[test]
fn reclaim_after_expiry_returns_the_full_amount() {
    let f = setup();
    let auth_id = authorize(&f);
    set_time(&f.env, EXPIRES + 1);
    assert_eq!(try_reclaim(&f, &auth_id).unwrap().unwrap(), MAX);

    assert_eq!(
        f.env.auths(),
        std::vec![(
            f.payer.clone(),
            invocation(
                &f.escrow_id,
                "reclaim",
                (&f.payer, auth_id.clone()).into_val(&f.env),
                std::vec![],
            )
        )]
    );
    assert_eq!(
        escrow_events(&f),
        vec![
            &f.env,
            event(
                &f,
                (symbol_short!("reclaimd"),).into_val(&f.env),
                (auth_id.clone(), f.payer.clone(), MAX).into_val(&f.env),
            ),
        ]
    );

    assert_eq!(balances(&f), (FUND, 0, 0, 0));
    let a = f.escrow.authorization(&auth_id);
    assert!(a.revoked);
    assert!(!a.settled);
    assert_eq!(a.spent, 0);
}

#[test]
fn reclaim_by_someone_other_than_the_payer_fails() {
    let f = setup();
    let auth_id = authorize(&f);
    set_time(&f.env, EXPIRES + 1);

    // Signing as themselves: not the authorization's payer.
    assert_eq!(
        contract_err(try_reclaim_signed(&f, &f.stranger, &f.stranger, &auth_id)),
        Error::Unauthorized
    );
    // Naming the payer without the payer's signature.
    assert_host_err(try_reclaim_signed(&f, &f.stranger, &f.payer, &auth_id));
    // The settler cannot pull it back either.
    assert_host_err(try_reclaim_signed(&f, &f.settler, &f.payer, &auth_id));

    assert_eq!(balances(&f), (FUND - MAX, MAX, 0, 0));
}

#[test]
fn settle_after_reclaim_is_revoked() {
    let f = setup();
    let auth_id = authorize(&f);
    set_time(&f.env, EXPIRES + 1);
    try_reclaim(&f, &auth_id).unwrap().unwrap();
    assert_eq!(
        contract_err(try_settle(&f, &f.settler, &auth_id, &Vec::new(&f.env))),
        Error::Revoked
    );
    // And a second reclaim returns nothing more.
    assert_eq!(contract_err(try_reclaim(&f, &auth_id)), Error::Revoked);
    assert_eq!(balances(&f), (FUND, 0, 0, 0));
}

#[test]
fn reclaim_after_settle_is_replay() {
    let f = setup();
    let auth_id = authorize(&f);
    settle(&f, &auth_id, &vec![&f.env, payout(&f.agent_a, PRICE_A)]);
    set_time(&f.env, EXPIRES + 1);
    assert_eq!(contract_err(try_reclaim(&f, &auth_id)), Error::Replay);
    assert_eq!(balances(&f), (FUND - PRICE_A, 0, PRICE_A, 0));
}

// ── settler rotation ──────────────────────────────────────────────────

#[test]
fn set_settler_rotates_the_settler() {
    let f = setup();
    let new_settler = Address::generate(&f.env);
    try_set_settler_signed(&f, &f.admin, &new_settler)
        .unwrap()
        .unwrap();

    assert_eq!(
        f.env.auths(),
        std::vec![(
            f.admin.clone(),
            invocation(
                &f.escrow_id,
                "set_settler",
                (&new_settler,).into_val(&f.env),
                std::vec![],
            )
        )]
    );
    assert_eq!(
        escrow_events(&f),
        vec![
            &f.env,
            event(
                &f,
                (symbol_short!("settler"),).into_val(&f.env),
                (f.settler.clone(), new_settler.clone()).into_val(&f.env),
            ),
        ]
    );
    assert_eq!(f.escrow.settler(), new_settler);

    let auth_id = authorize(&f);
    let payouts = vec![&f.env, payout(&f.agent_a, PRICE_A)];
    assert_eq!(
        contract_err(try_settle(&f, &f.settler, &auth_id, &payouts)),
        Error::Unauthorized
    );
    try_settle(&f, &new_settler, &auth_id, &payouts)
        .unwrap()
        .unwrap();
    assert_eq!(f.usdc.balance(&f.owner_a), PRICE_A);
}

#[test]
fn set_settler_needs_the_admin() {
    let f = setup();
    for signer in [&f.stranger, &f.settler] {
        assert_host_err(try_set_settler_signed(&f, signer, &f.stranger));
    }
    assert_eq!(f.escrow.settler(), f.settler);
}

// ── views ─────────────────────────────────────────────────────────────

#[test]
fn views_report_version_admin_and_settler() {
    let f = setup();
    assert_eq!(f.escrow.version(), 2);
    assert_eq!(f.escrow.admin(), f.admin);
    assert_eq!(f.escrow.settler(), f.settler);
    let missing = BytesN::from_array(&f.env, &[9u8; 16]);
    assert_eq!(
        f.escrow.try_authorization(&missing).err().unwrap().unwrap(),
        Error::NotFound
    );
    assert_eq!(
        f.escrow.try_receipt(&missing).err().unwrap().unwrap(),
        Error::NotFound
    );
}

// ── storage TTL ───────────────────────────────────────────────────────

#[test]
fn ttl_keeps_authorizations_alive_through_their_window() {
    let f = setup();
    assert_eq!(instance_ttl(&f), INSTANCE_EXTEND_TO);

    // A short window: 30 days plus the window's ledgers (1000 s at 5 s).
    let auth_id = authorize(&f);
    let auth_key = DataKey::Auth(auth_id.clone());
    let window_ledgers = ((EXPIRES - T0) / 5) as u32;
    assert_eq!(entry_ttl(&f, &auth_key), ENTRY_EXTEND_TO + window_ledgers);

    // A 90-day window is kept alive for all 90 days, plus 30.
    let ninety_days = 90 * 24 * 3600;
    let long_id = try_authorize(&f, MAX, T0 + ninety_days).unwrap().unwrap();
    assert_eq!(
        entry_ttl(&f, &DataKey::Auth(long_id)),
        ENTRY_EXTEND_TO + 90 * DAY_IN_LEDGERS
    );

    // A far-future expiry is clamped to the network maximum, not overflowed.
    let far_id = try_authorize(&f, MAX, u64::MAX).unwrap().unwrap();
    assert_eq!(
        entry_ttl(&f, &DataKey::Auth(far_id)),
        f.env.storage().max_ttl()
    );

    // Two days on, untouched, the entry has aged; a read re-extends it.
    advance_ledgers(&f.env, 2 * DAY_IN_LEDGERS);
    assert_eq!(
        entry_ttl(&f, &auth_key),
        ENTRY_EXTEND_TO + window_ledgers - 2 * DAY_IN_LEDGERS
    );
    f.escrow.authorization(&auth_id);
    assert_eq!(entry_ttl(&f, &auth_key), ENTRY_EXTEND_TO + window_ledgers);

    // Settling bumps the instance and gives each receipt 30 days. The auth
    // was re-extended by the settle's own read; once closed, it needs only
    // 30 days, which it already has, so the write leaves it be.
    advance_ledgers(&f.env, 2 * DAY_IN_LEDGERS);
    assert_eq!(instance_ttl(&f), INSTANCE_EXTEND_TO - 4 * DAY_IN_LEDGERS);
    let receipts = settle(&f, &auth_id, &vec![&f.env, payout(&f.agent_a, PRICE_A)]);
    assert_eq!(instance_ttl(&f), INSTANCE_EXTEND_TO);
    assert_eq!(entry_ttl(&f, &auth_key), ENTRY_EXTEND_TO + window_ledgers);
    let receipt_id = receipts.get(0).unwrap();
    let receipt_key = DataKey::Receipt(receipt_id.clone());
    assert_eq!(entry_ttl(&f, &receipt_key), ENTRY_EXTEND_TO);

    // A receipt read two days on re-extends it too.
    advance_ledgers(&f.env, 2 * DAY_IN_LEDGERS);
    assert_eq!(
        entry_ttl(&f, &receipt_key),
        ENTRY_EXTEND_TO - 2 * DAY_IN_LEDGERS
    );
    f.escrow.receipt(&receipt_id);
    assert_eq!(entry_ttl(&f, &receipt_key), ENTRY_EXTEND_TO);
}
