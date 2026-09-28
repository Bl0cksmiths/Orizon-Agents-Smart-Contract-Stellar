#![no_std]

use soroban_sdk::{contracttype, Address, BytesN, String, Symbol, Vec};

/// Registered agent in AgentRegistry.
#[contracttype]
#[derive(Clone)]
pub struct Agent {
    pub id: Symbol,
    pub owner: Address,
    pub name: String,
    pub skills: Vec<Symbol>,
    /// USDC price-per-call, stroops (7 decimals, Stellar convention).
    pub price: i128,
    pub active: bool,
    pub registered_at: u64,
}

/// Pre-authorization created by a payer; consumed by `charge`.
#[contracttype]
#[derive(Clone)]
pub struct Authorization {
    pub payer: Address,
    pub agent_id: Symbol,
    pub max_amount: i128,
    pub spent: i128,
    pub expires_at: u64,
    pub revoked: bool,
}

/// Payment receipt produced by a successful `charge`.
#[contracttype]
#[derive(Clone)]
pub struct Receipt {
    pub auth_id: BytesN<16>,
    pub agent_id: Symbol,
    pub amount: i128,
    pub job_id: BytesN<16>,
    pub settled_at: u64,
}

/// Write-once attestation per job in AttestationRegistry.
#[contracttype]
#[derive(Clone)]
pub struct Attestation {
    pub orchestrator: Address,
    pub intent_hash: BytesN<32>,
    pub agents: Vec<Symbol>,
    pub receipts: Vec<BytesN<16>>,
    pub total_spent: i128,
    pub sealed_at: u64,
}

/// Contract error codes reused across crates so the backend can decode them uniformly.
pub mod codes {
    pub const UNAUTHORIZED: u32 = 1;
    pub const NOT_FOUND: u32 = 2;
    pub const ALREADY_EXISTS: u32 = 3;
    pub const EXPIRED: u32 = 4;
    pub const INSUFFICIENT_AUTH: u32 = 5;
    pub const REVOKED: u32 = 6;
    pub const REPLAY: u32 = 7;
    pub const INACTIVE: u32 = 8;
    /// Escrow reclaim attempted before the authorization's window closed.
    pub const LOCKED: u32 = 9;
    /// Rating or weight outside the accepted range (reputation-ledger).
    pub const OUT_OF_RANGE: u32 = 100;
    /// Non-positive authorize/charge amount (payment-escrow).
    pub const BAD_AMOUNT: u32 = 101;
    /// More payouts in one escrow settle than the contract accepts (payment-escrow).
    pub const BAD_PAYOUTS: u32 = 102;
}
