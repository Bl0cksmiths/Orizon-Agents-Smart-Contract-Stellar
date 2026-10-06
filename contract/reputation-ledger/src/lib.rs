#![no_std]

//! # ReputationLedger v2
//!
//! Decayed, value-weighted, dispute-aware rating evidence store.
//!
//! Design (per Jøsang beta-reputation with a forgetting factor, and ERC-8004
//! conventions of raw evidence on-chain / complex aggregation off-chain):
//!
//! - **Value weighting** — each rating carries the job's USDC value (stroops)
//!   as its weight, so a 100-USDC job moves reputation more than a 0.01-USDC
//!   one. A per-rating weight cap stops any single job from buying dominance.
//! - **Exponential decay** — evidence loses influence at λ = 0.925 per weekly
//!   epoch (≈ 9-week half-life), applied lazily on write and on read, so an
//!   agent's score reflects recent behavior rather than ancient history.
//! - **Persistent replay guard** — v1 kept the `(agent, job)` seen-marker in
//!   TEMPORARY storage, which expires after hours and re-opened the replay
//!   window; v2 stores it in PERSISTENT storage.
//! - **Lifetime counters** — `count` and `disputed` never decay; they are raw
//!   evidence for off-chain consumers (dispute rate, volume checks).

use orizon_shared::ttl;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env, Symbol,
};

/// Length of one decay epoch in seconds (1 week).
const EPOCH_SECONDS: u64 = 604_800;
/// Forgetting factor λ = DECAY_NUM / DECAY_DEN = 0.925 per epoch,
/// giving stale evidence a ≈ 9-week half-life.
const DECAY_NUM: i128 = 925;
const DECAY_DEN: i128 = 1000;
/// 0.925^96 ≈ 0.0006 — beyond this many idle epochs the residual evidence is
/// noise, so it is fully forgotten (reset to zero) instead of looped over.
const MAX_DECAY_EPOCHS: u64 = 96;
/// Per-rating weight cap: 100 USDC in stroops (7 decimals). Caps how much
/// reputation a single job can buy.
const MAX_WEIGHT: i128 = 1_000_000_000;

/// Per-agent reputation accumulator.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepState {
    /// Σ (rating_bps × weight), decayed. rating_bps = rating_0_to_100 × 100,
    /// i.e. 0..10_000, so `sum_w / weight` is already a basis-point mean.
    pub sum_w: i128,
    /// Σ weight, decayed. Weight is the job's USDC value in stroops
    /// (7 decimals, Stellar convention).
    pub weight: i128,
    /// Lifetime rating count — never decayed.
    pub count: u32,
    /// Lifetime dispute count — never decayed.
    pub disputed: u32,
    /// Epoch of the last write (epoch = ledger timestamp / EPOCH_SECONDS).
    pub last_epoch: u64,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Scorer,
    /// agent_id → RepState (persistent).
    Rep(Symbol),
    /// (agent_id, job_id) replay marker — PERSISTENT storage so the guard
    /// never lapses (the v1 bug kept it in temporary storage).
    Rated(Symbol, BytesN<16>),
    /// (agent_id, payer) → cumulative i128 weight (persistent, never decayed).
    /// Raw per-payer stake for off-chain Sybil / self-dealing analysis.
    PayerW(Symbol, Address),
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    Unauthorized = 1,
    NotFound = 2,
    Replay = 7,
    OutOfRange = 100,
}

#[contract]
pub struct ReputationLedger;

/// Current decay epoch derived from the ledger timestamp.
fn current_epoch(env: &Env) -> u64 {
    env.ledger().timestamp() / EPOCH_SECONDS
}

/// Lazily decay `state` to `current` epoch: scale `sum_w` and `weight` by
/// λ once per elapsed epoch (both scale together, so the mean is unchanged
/// by pure decay). Beyond MAX_DECAY_EPOCHS the evidence is zeroed outright.
/// Lifetime `count` / `disputed` are never decayed.
fn decay_to(state: &mut RepState, current: u64) {
    if current <= state.last_epoch {
        return;
    }
    let delta = current - state.last_epoch;
    if delta >= MAX_DECAY_EPOCHS {
        state.sum_w = 0;
        state.weight = 0;
    } else {
        for _ in 0..delta {
            state.sum_w = state.sum_w * DECAY_NUM / DECAY_DEN;
            state.weight = state.weight * DECAY_NUM / DECAY_DEN;
        }
    }
    state.last_epoch = current;
}

/// Load the agent's RepState decayed to the current epoch — in memory only,
/// nothing is written back (views must never write). Inside a submitted
/// transaction the stored record and the instance get their lifetimes
/// extended; that touches TTL only, never the data.
fn decayed_state(env: &Env, agent_id: &Symbol) -> RepState {
    let now = current_epoch(env);
    let key = DataKey::Rep(agent_id.clone());
    let stored: Option<RepState> = env.storage().persistent().get(&key);
    if stored.is_some() {
        ttl::extend_persistent(env, &key);
    }
    ttl::extend_instance(env);
    let mut state = stored.unwrap_or(RepState {
        sum_w: 0,
        weight: 0,
        count: 0,
        disputed: 0,
        last_epoch: now,
    });
    decay_to(&mut state, now);
    state
}

#[allow(deprecated)]
#[contractimpl]
impl ReputationLedger {
    pub fn __constructor(env: Env, admin: Address, scorer: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Scorer, &scorer);
        ttl::extend_instance(&env);
    }

    /// Record a rating for a completed job.
    ///
    /// - `caller` must be the registered scorer (the backend JobManager).
    /// - `rating_0_to_100` is stored as basis points (× 100 → 0..10_000).
    /// - `weight` is the job's USDC value in stroops, 0 < weight ≤ MAX_WEIGHT.
    /// - `(agent_id, job_id)` can only ever be rated once (persistent guard).
    /// - `payer` accrues cumulative stake for off-chain Sybil analysis.
    /// - `kind == "dispute"` additionally bumps the lifetime dispute counter.
    #[allow(clippy::too_many_arguments)] // mirrors the fixed v2 ABI
    pub fn submit(
        env: Env,
        caller: Address,
        agent_id: Symbol,
        job_id: BytesN<16>,
        rating_0_to_100: u32,
        weight: i128,
        payer: Address,
        kind: Symbol,
    ) -> Result<(), Error> {
        caller.require_auth();

        let scorer: Address = env
            .storage()
            .instance()
            .get(&DataKey::Scorer)
            .ok_or(Error::NotFound)?;
        if caller != scorer {
            return Err(Error::Unauthorized);
        }
        if rating_0_to_100 > 100 {
            return Err(Error::OutOfRange);
        }
        if weight <= 0 || weight > MAX_WEIGHT {
            return Err(Error::OutOfRange);
        }

        let seen_key = DataKey::Rated(agent_id.clone(), job_id.clone());
        if env.storage().persistent().has(&seen_key) {
            return Err(Error::Replay);
        }

        let mut state = decayed_state(&env, &agent_id);
        state.sum_w += (rating_0_to_100 as i128) * 100 * weight;
        state.weight += weight;
        state.count += 1;
        if kind == symbol_short!("dispute") {
            state.disputed += 1;
        }

        let rep_key = DataKey::Rep(agent_id.clone());
        env.storage().persistent().set(&rep_key, &state);
        ttl::extend_persistent(&env, &rep_key);
        env.storage().persistent().set(&seen_key, &true);
        ttl::extend_persistent(&env, &seen_key);

        let payer_key = DataKey::PayerW(agent_id.clone(), payer);
        let paid: i128 = env.storage().persistent().get(&payer_key).unwrap_or(0);
        env.storage().persistent().set(&payer_key, &(paid + weight));
        ttl::extend_persistent(&env, &payer_key);
        ttl::extend_instance(&env);

        env.events().publish(
            (symbol_short!("rated"), agent_id),
            (rating_0_to_100, weight, job_id, kind),
        );
        Ok(())
    }

    /// View — the agent's RepState decayed to now (nothing is written).
    pub fn rep_state(env: Env, agent_id: Symbol) -> RepState {
        decayed_state(&env, &agent_id)
    }

    /// View — decayed weighted mean in basis points (0..10_000, since
    /// `sum_w` already carries the × 100 scale). 0 when weight == 0.
    pub fn avg_bps(env: Env, agent_id: Symbol) -> u32 {
        let s = decayed_state(&env, &agent_id);
        if s.weight == 0 {
            0
        } else {
            (s.sum_w / s.weight).clamp(0, 10_000) as u32
        }
    }

    /// View — Bayesian-smoothed mean:
    /// `(prior_weight × prior_bps + sum_w) / (prior_weight + weight)`.
    /// The caller-supplied prior anchors low-evidence agents; as decayed
    /// evidence weight grows the result converges to the raw mean.
    /// Returns `prior_bps` when both weights are 0. Clamped to 0..10_000.
    pub fn rep_bps(env: Env, agent_id: Symbol, prior_bps: u32, prior_weight: i128) -> u32 {
        let s = decayed_state(&env, &agent_id);
        let total_weight = prior_weight + s.weight;
        if total_weight <= 0 {
            return prior_bps;
        }
        let blended = (prior_weight * (prior_bps as i128) + s.sum_w) / total_weight;
        blended.clamp(0, 10_000) as u32
    }

    /// View — lifetime dispute rate in basis points:
    /// `disputed × 10_000 / count`. 0 when count == 0. Never decayed.
    pub fn dispute_rate_bps(env: Env, agent_id: Symbol) -> u32 {
        let s = decayed_state(&env, &agent_id);
        if s.count == 0 {
            0
        } else {
            ((s.disputed as u64) * 10_000 / (s.count as u64)) as u32
        }
    }

    /// View — cumulative (never decayed) weight this payer has contributed
    /// to this agent's reputation. 0 by default. Raw evidence for off-chain
    /// Sybil / self-dealing analysis.
    pub fn payer_weight(env: Env, agent_id: Symbol, payer: Address) -> i128 {
        let key = DataKey::PayerW(agent_id, payer);
        let paid: Option<i128> = env.storage().persistent().get(&key);
        if paid.is_some() {
            ttl::extend_persistent(&env, &key);
        }
        ttl::extend_instance(&env);
        paid.unwrap_or(0)
    }

    /// Admin-only: swap the scorer address.
    pub fn set_scorer(env: Env, new_scorer: Address) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotFound)?;
        admin.require_auth();
        env.storage().instance().set(&DataKey::Scorer, &new_scorer);
        ttl::extend_instance(&env);
        Ok(())
    }
}

#[cfg(test)]
mod test;
