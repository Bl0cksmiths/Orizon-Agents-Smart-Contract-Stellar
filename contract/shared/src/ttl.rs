//! Storage lifetimes (TTL) shared by the registries and the ledger.
//!
//! Soroban archives a persistent entry, the contract instance and the
//! contract's wasm once their TTL runs out. After that a read fails until
//! someone restores the entry. Every call that writes or reads one of these
//! contracts' records therefore pushes the record and the instance (which
//! carries the wasm with it) back out to `EXTEND_TO` ledgers.
//!
//! The extension is skipped while more than `EXTEND_TO - RENEW_WINDOW` ledgers
//! remain, so an entry that is touched often still pays rent at most about
//! once a day. Rent is charged for the ledgers added, not per call.
//!
//! Only calls inside a submitted transaction extend anything. A read made by
//! simulation (how the backend and the site read these contracts) changes
//! nothing on chain, so records that are only ever read need the keeper
//! script (`scripts/extend-ttl.sh`) as well.

use soroban_sdk::{Env, IntoVal, Val};

/// Ledgers close about every 5 seconds: 17,280 a day.
pub const DAY_IN_LEDGERS: u32 = 17_280;

/// How long a record or instance lives after it is touched: 180 days, which is
/// the network's maximum entry TTL on testnet and mainnet today. Always
/// clamped to the live maximum (`Storage::max_ttl`), so a lower network
/// limit can't make the call fail.
pub const EXTEND_TO: u32 = 180 * DAY_IN_LEDGERS;

/// An entry is only re-extended once it has lost this much of `EXTEND_TO`.
pub const RENEW_WINDOW: u32 = DAY_IN_LEDGERS;

/// `(threshold, extend_to)` for this ledger, clamped to the network maximum.
pub fn bounds(env: &Env) -> (u32, u32) {
    let extend_to = EXTEND_TO.min(env.storage().max_ttl());
    (extend_to.saturating_sub(RENEW_WINDOW), extend_to)
}

/// Extends the calling contract's instance and wasm.
pub fn extend_instance(env: &Env) {
    let (threshold, extend_to) = bounds(env);
    env.storage().instance().extend_ttl(threshold, extend_to);
}

/// Extends a persistent entry of the calling contract. The entry must exist:
/// call it after a successful `get`/`has`, or after `set`.
pub fn extend_persistent<K>(env: &Env, key: &K)
where
    K: IntoVal<Env, Val>,
{
    let (threshold, extend_to) = bounds(env);
    env.storage()
        .persistent()
        .extend_ttl(key, threshold, extend_to);
}
