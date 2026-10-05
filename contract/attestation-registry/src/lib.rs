#![no_std]
// `seal` takes eight arguments by design (one per attestation field), and the
// client `#[contractimpl]` generates beside it mirrors them.
#![allow(clippy::too_many_arguments)]

use orizon_shared::{ttl, Attestation};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    Symbol, Vec,
};

#[contracttype]
pub enum DataKey {
    Admin,
    Sealer,
    Job(BytesN<16>),
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    Unauthorized = 1,
    NotFound = 2,
    AlreadyExists = 3,
}

#[contract]
pub struct AttestationRegistry;

#[allow(deprecated)]
#[contractimpl]
impl AttestationRegistry {
    pub fn __constructor(env: Env, admin: Address, sealer: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Sealer, &sealer);
        ttl::extend_instance(&env);
    }

    /// Write-once. The caller must be the registered sealer. The seal and
    /// the contract instance are extended to the full lifetime.
    pub fn seal(
        env: Env,
        caller: Address,
        job_id: BytesN<16>,
        orchestrator: Address,
        intent_hash: BytesN<32>,
        agents: Vec<Symbol>,
        receipts: Vec<BytesN<16>>,
        total_spent: i128,
    ) -> Result<(), Error> {
        caller.require_auth();
        let sealer: Address = env
            .storage()
            .instance()
            .get(&DataKey::Sealer)
            .ok_or(Error::NotFound)?;
        if caller != sealer {
            return Err(Error::Unauthorized);
        }
        if env
            .storage()
            .persistent()
            .has(&DataKey::Job(job_id.clone()))
        {
            return Err(Error::AlreadyExists);
        }

        let attestation = Attestation {
            orchestrator: orchestrator.clone(),
            intent_hash,
            agents,
            receipts,
            total_spent,
            sealed_at: env.ledger().timestamp(),
        };
        let key = DataKey::Job(job_id.clone());
        env.storage().persistent().set(&key, &attestation);
        ttl::extend_persistent(&env, &key);
        ttl::extend_instance(&env);

        env.events().publish(
            (symbol_short!("sealed"), job_id),
            (orchestrator, total_spent),
        );
        Ok(())
    }

    /// Reads a seal. Inside a submitted transaction this also re-extends the
    /// seal and the instance (a simulated read changes nothing on chain).
    pub fn get(env: Env, job_id: BytesN<16>) -> Result<Attestation, Error> {
        let key = DataKey::Job(job_id);
        let attestation = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(Error::NotFound)?;
        ttl::extend_persistent(&env, &key);
        ttl::extend_instance(&env);
        Ok(attestation)
    }

    /// Whether a job is sealed; re-extends it as `get` does when it is.
    pub fn exists(env: Env, job_id: BytesN<16>) -> bool {
        let key = DataKey::Job(job_id);
        let sealed = env.storage().persistent().has(&key);
        if sealed {
            ttl::extend_persistent(&env, &key);
        }
        ttl::extend_instance(&env);
        sealed
    }

    pub fn set_sealer(env: Env, new_sealer: Address) -> Result<(), Error> {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotFound)?;
        admin.require_auth();
        env.storage().instance().set(&DataKey::Sealer, &new_sealer);
        ttl::extend_instance(&env);
        Ok(())
    }
}

#[cfg(test)]
mod test;
