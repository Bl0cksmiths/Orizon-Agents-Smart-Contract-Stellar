#![no_std]

//! PaymentEscrow v2: custody at `authorize`, per-operator payouts at
//! `settle`, payer `reclaim` after expiry, admin-rotatable settler.
//!
//! The frozen interface lives in `docs/escrow-v2-interface.md`.

use orizon_shared::{Authorization, Receipt};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, BytesN, Env,
    Symbol, Vec,
};

/// Minimal import of the agent-registry's `owner_of` view so we can resolve payouts.
mod registry {
    use soroban_sdk::{contractclient, Address, Env, Symbol};
    // Only the generated `RegistryClient` is used; the trait just defines it.
    #[allow(dead_code)]
    #[contractclient(name = "RegistryClient")]
    pub trait Registry {
        fn owner_of(env: Env, id: Symbol) -> Address;
    }
}

/// Most payouts one `settle` may carry, bounding its CPU and footprint.
pub const MAX_PAYOUTS: u32 = 16;

// ── Storage TTLs ─────────────────────────────────────────────────────
// Ledgers close roughly every 5 seconds, so one day is about 17_280 ledgers.
// Every persistent extension is clamped to the network's max entry TTL.
const LEDGER_SECONDS: u64 = 5;
pub const DAY_IN_LEDGERS: u32 = 17_280;

/// The contract instance (config + nonce) is bumped to 30 days by every
/// state-changing call, but only once it has fallen below 29 days, so a
/// busy contract pays that rent about once a day.
pub const INSTANCE_EXTEND_TO: u32 = 30 * DAY_IN_LEDGERS;
pub const INSTANCE_THRESHOLD: u32 = INSTANCE_EXTEND_TO - DAY_IN_LEDGERS;

/// Auth and Receipt entries live at least 30 days past their last write or
/// read (again only re-extended once below 29 days). An open authorization
/// is additionally kept alive through its whole window (`auth_extend_to`),
/// so it cannot be archived before it is settled or reclaimed.
pub const ENTRY_EXTEND_TO: u32 = 30 * DAY_IN_LEDGERS;

#[contracttype]
pub enum DataKey {
    Admin,
    Usdc,
    Registry,
    Settler,
    Auth(BytesN<16>),
    Receipt(BytesN<16>),
    /// Used to derive unique auth / receipt ids.
    Nonce,
}

/// One operator's share of a settled authorization.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Payout {
    pub agent_id: Symbol,
    pub amount: i128,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    Unauthorized = 1,
    NotFound = 2,
    Expired = 4,
    Insufficient = 5,
    Revoked = 6,
    Replay = 7,
    Locked = 9,
    BadAmount = 101,
    BadPayouts = 102,
}

#[contract]
pub struct PaymentEscrow;

#[allow(deprecated)]
#[contractimpl]
impl PaymentEscrow {
    pub fn __constructor(
        env: Env,
        admin: Address,
        usdc: Address,
        registry: Address,
        settler: Address,
    ) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Usdc, &usdc);
        env.storage().instance().set(&DataKey::Registry, &registry);
        env.storage().instance().set(&DataKey::Settler, &settler);
        env.storage().instance().set(&DataKey::Nonce, &0u64);
        bump_instance(&env);
    }

    /// The payer pre-authorizes a spend of up to `max_amount` and hands it to
    /// this contract's custody in the same invocation, so the payer's single
    /// signature covers both the call and the nested token transfer.
    ///
    /// `agent_id` is a label for the authorization (the console sends
    /// `orizon_batch`); payouts name their own agents at `settle`.
    pub fn authorize(
        env: Env,
        payer: Address,
        agent_id: Symbol,
        max_amount: i128,
        expires_at: u64,
    ) -> Result<BytesN<16>, Error> {
        payer.require_auth();
        if max_amount <= 0 {
            return Err(Error::BadAmount);
        }
        if expires_at <= env.ledger().timestamp() {
            return Err(Error::Expired);
        }
        bump_instance(&env);

        let auth_id = next_id(&env);
        let record = Authorization {
            payer: payer.clone(),
            agent_id: agent_id.clone(),
            max_amount,
            spent: 0,
            expires_at,
            revoked: false,
            settled: false,
        };
        write_auth(&env, &auth_id, &record);

        // Custody: payer -> this contract. A sub-invocation of the
        // payer-authorized root call, so the payer's one auth entry covers it.
        usdc(&env).transfer(&payer, env.current_contract_address(), &max_amount);

        env.events().publish(
            (symbol_short!("authd"), agent_id),
            (auth_id.clone(), payer, max_amount),
        );
        Ok(auth_id)
    }

    /// Settler only. Pays each payout's registered owner from custody, writes
    /// a receipt per payout, returns the remainder to the payer and marks the
    /// authorization settled, all in one transaction. An empty `payouts` is a
    /// full release back to the payer. Returns the receipt ids in order.
    pub fn settle(
        env: Env,
        caller: Address,
        auth_id: BytesN<16>,
        job_id: BytesN<16>,
        payouts: Vec<Payout>,
    ) -> Result<Vec<BytesN<16>>, Error> {
        caller.require_auth();
        if caller != Self::settler(env.clone()) {
            return Err(Error::Unauthorized);
        }

        let mut auth = read_auth(&env, &auth_id)?;
        if auth.revoked {
            return Err(Error::Revoked);
        }
        if auth.settled {
            return Err(Error::Replay);
        }
        if env.ledger().timestamp() > auth.expires_at {
            return Err(Error::Expired);
        }

        if payouts.len() > MAX_PAYOUTS {
            return Err(Error::BadPayouts);
        }
        let mut sum: i128 = 0;
        for p in payouts.iter() {
            if p.amount <= 0 {
                return Err(Error::BadAmount);
            }
            sum = sum.checked_add(p.amount).ok_or(Error::BadAmount)?;
        }
        if sum > auth.max_amount {
            return Err(Error::Insufficient);
        }
        let returned = auth.max_amount - sum;
        bump_instance(&env);

        // Effects before interactions: a re-entrant call sees `settled`.
        auth.spent = sum;
        auth.settled = true;
        write_auth(&env, &auth_id, &auth);

        let usdc = usdc(&env);
        let reg = registry::RegistryClient::new(&env, &registry_addr(&env));
        let this = env.current_contract_address();
        let now = env.ledger().timestamp();
        let mut receipt_ids: Vec<BytesN<16>> = Vec::new(&env);

        for p in payouts.iter() {
            let owner = reg.owner_of(&p.agent_id);
            let receipt_id = next_id(&env);
            let receipt = Receipt {
                auth_id: auth_id.clone(),
                agent_id: p.agent_id.clone(),
                amount: p.amount,
                job_id: job_id.clone(),
                settled_at: now,
            };
            write_receipt(&env, &receipt_id, &receipt);

            usdc.transfer(&this, &owner, &p.amount);

            env.events().publish(
                (symbol_short!("charged"), p.agent_id),
                (
                    receipt_id.clone(),
                    auth_id.clone(),
                    p.amount,
                    job_id.clone(),
                ),
            );
            receipt_ids.push_back(receipt_id);
        }

        if returned > 0 {
            usdc.transfer(&this, &auth.payer, &returned);
        }

        env.events().publish(
            (symbol_short!("settled"),),
            (auth_id, job_id, sum, returned),
        );
        Ok(receipt_ids)
    }

    /// The payer takes custody back from an authorization that was never
    /// settled, once its window has closed. Returns the amount returned.
    pub fn reclaim(env: Env, payer: Address, auth_id: BytesN<16>) -> Result<i128, Error> {
        payer.require_auth();
        let mut auth = read_auth(&env, &auth_id)?;
        if auth.payer != payer {
            return Err(Error::Unauthorized);
        }
        if auth.settled {
            return Err(Error::Replay);
        }
        if auth.revoked {
            return Err(Error::Revoked);
        }
        if env.ledger().timestamp() <= auth.expires_at {
            return Err(Error::Locked);
        }
        bump_instance(&env);

        // Nothing was paid out, so the whole custody goes back.
        let returned = auth.max_amount;
        auth.revoked = true;
        write_auth(&env, &auth_id, &auth);

        usdc(&env).transfer(&env.current_contract_address(), &payer, &returned);

        env.events()
            .publish((symbol_short!("reclaimd"),), (auth_id, payer, returned));
        Ok(returned)
    }

    /// Admin only. Rotates the settler.
    pub fn set_settler(env: Env, new_settler: Address) -> Result<(), Error> {
        Self::admin(env.clone()).require_auth();
        let old = Self::settler(env.clone());
        env.storage()
            .instance()
            .set(&DataKey::Settler, &new_settler);
        bump_instance(&env);
        env.events()
            .publish((symbol_short!("settler"),), (old, new_settler));
        Ok(())
    }

    // ── Views ─────────────────────────────────────────────────────────
    pub fn authorization(env: Env, auth_id: BytesN<16>) -> Result<Authorization, Error> {
        read_auth(&env, &auth_id)
    }

    pub fn receipt(env: Env, receipt_id: BytesN<16>) -> Result<Receipt, Error> {
        let key = DataKey::Receipt(receipt_id);
        let receipt = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::NotFound)?;
        extend_entry(&env, &key, ENTRY_EXTEND_TO);
        Ok(receipt)
    }

    pub fn settler(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Settler)
            .expect("settler must be set")
    }

    pub fn admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("admin must be set")
    }

    pub fn version(_env: Env) -> u32 {
        2
    }
}

fn registry_addr(env: &Env) -> Address {
    env.storage()
        .instance()
        .get(&DataKey::Registry)
        .expect("registry must be set")
}

fn usdc(env: &Env) -> token::TokenClient<'_> {
    let addr: Address = env
        .storage()
        .instance()
        .get(&DataKey::Usdc)
        .expect("usdc must be set");
    token::TokenClient::new(env, &addr)
}

fn bump_instance(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_THRESHOLD, INSTANCE_EXTEND_TO);
}

/// Extends a persistent entry to live `extend_to` ledgers from now, when it
/// has less than `extend_to - 1 day` left. Clamped to the network maximum.
fn extend_entry(env: &Env, key: &DataKey, extend_to: u32) {
    let extend_to = extend_to.min(env.storage().max_ttl());
    let threshold = extend_to.saturating_sub(DAY_IN_LEDGERS);
    env.storage()
        .persistent()
        .extend_ttl(key, threshold, extend_to);
}

/// An open authorization must outlive its window, plus the usual 30 days so
/// the payer can still reclaim afterwards. The window is converted from
/// seconds to ledgers at 5 s per ledger; slower ledgers only over-extend.
fn auth_extend_to(env: &Env, auth: &Authorization) -> u32 {
    if auth.settled || auth.revoked {
        return ENTRY_EXTEND_TO;
    }
    let window = auth.expires_at.saturating_sub(env.ledger().timestamp());
    let window_ledgers = u32::try_from(window.div_ceil(LEDGER_SECONDS)).unwrap_or(u32::MAX);
    ENTRY_EXTEND_TO.saturating_add(window_ledgers)
}

fn read_auth(env: &Env, auth_id: &BytesN<16>) -> Result<Authorization, Error> {
    let key = DataKey::Auth(auth_id.clone());
    let auth: Authorization = env
        .storage()
        .persistent()
        .get(&key)
        .ok_or(Error::NotFound)?;
    extend_entry(env, &key, auth_extend_to(env, &auth));
    Ok(auth)
}

fn write_auth(env: &Env, auth_id: &BytesN<16>, auth: &Authorization) {
    let key = DataKey::Auth(auth_id.clone());
    env.storage().persistent().set(&key, auth);
    extend_entry(env, &key, auth_extend_to(env, auth));
}

fn write_receipt(env: &Env, receipt_id: &BytesN<16>, receipt: &Receipt) {
    let key = DataKey::Receipt(receipt_id.clone());
    env.storage().persistent().set(&key, receipt);
    extend_entry(env, &key, ENTRY_EXTEND_TO);
}

fn next_id(env: &Env) -> BytesN<16> {
    let n: u64 = env
        .storage()
        .instance()
        .get(&DataKey::Nonce)
        .unwrap_or(0u64);
    env.storage().instance().set(&DataKey::Nonce, &(n + 1));

    // Deterministic from `n` alone. Using ledger state (timestamp / sequence)
    // would drift between simulation and execution, leaving the dynamically
    // keyed storage entry outside the transaction footprint and causing
    // host_fn_failed: Error(Storage, ExceededLimit).
    let mut arr = [0u8; 16];
    arr[8..].copy_from_slice(&n.to_be_bytes());
    BytesN::from_array(env, &arr)
}

#[cfg(test)]
mod test;
