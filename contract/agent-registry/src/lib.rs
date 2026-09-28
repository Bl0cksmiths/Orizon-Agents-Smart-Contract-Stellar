#![no_std]

use orizon_shared::Agent;
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, Env, String,
    Symbol, Vec,
};

#[contracttype]
pub enum DataKey {
    Admin,
    Agent(Symbol),
    Ids,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    Unauthorized = 1,
    NotFound = 2,
    AlreadyExists = 3,
    Inactive = 8,
}

#[contract]
pub struct AgentRegistry;

#[allow(deprecated)]
#[contractimpl]
impl AgentRegistry {
    /// One-shot constructor set by Soroban at deploy time.
    pub fn __constructor(env: Env, admin: Address) {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Ids, &Vec::<Symbol>::new(&env));
    }

    /// Register a new agent. Owner-signed. Fails if id already exists.
    pub fn register(
        env: Env,
        owner: Address,
        id: Symbol,
        name: String,
        skills: Vec<Symbol>,
        price: i128,
    ) -> Result<(), Error> {
        owner.require_auth();

        if env.storage().persistent().has(&DataKey::Agent(id.clone())) {
            return Err(Error::AlreadyExists);
        }

        let agent = Agent {
            id: id.clone(),
            owner: owner.clone(),
            name,
            skills,
            price,
            active: true,
            registered_at: env.ledger().timestamp(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::Agent(id.clone()), &agent);

        let mut ids: Vec<Symbol> = env
            .storage()
            .instance()
            .get(&DataKey::Ids)
            .unwrap_or_else(|| Vec::new(&env));
        ids.push_back(id.clone());
        env.storage().instance().set(&DataKey::Ids, &ids);

        env.events()
            .publish((symbol_short!("regd"), id.clone()), owner);
        Ok(())
    }

    /// Update per-call price. Must be signed by the current owner.
    pub fn update_price(env: Env, id: Symbol, new_price: i128) -> Result<(), Error> {
        let mut agent: Agent = env
            .storage()
            .persistent()
            .get(&DataKey::Agent(id.clone()))
            .ok_or(Error::NotFound)?;
        agent.owner.require_auth();
        agent.price = new_price;
        env.storage()
            .persistent()
            .set(&DataKey::Agent(id.clone()), &agent);
        env.events()
            .publish((symbol_short!("updated"), id), symbol_short!("price"));
        Ok(())
    }

    /// Activate / deactivate. Owner-signed.
    pub fn set_active(env: Env, id: Symbol, active: bool) -> Result<(), Error> {
        let mut agent: Agent = env
            .storage()
            .persistent()
            .get(&DataKey::Agent(id.clone()))
            .ok_or(Error::NotFound)?;
        agent.owner.require_auth();
        agent.active = active;
        env.storage()
            .persistent()
            .set(&DataKey::Agent(id.clone()), &agent);
        env.events().publish((symbol_short!("active"), id), active);
        Ok(())
    }

    /// View — full agent record.
    pub fn get(env: Env, id: Symbol) -> Result<Agent, Error> {
        env.storage()
            .persistent()
            .get(&DataKey::Agent(id))
            .ok_or(Error::NotFound)
    }

    /// View — owner address (used by PaymentEscrow.charge to resolve payout).
    pub fn owner_of(env: Env, id: Symbol) -> Result<Address, Error> {
        let agent: Agent = Self::get(env, id)?;
        Ok(agent.owner)
    }

    /// View — all registered ids (capped: workspace agrees to bound registrations).
    pub fn list_ids(env: Env) -> Vec<Symbol> {
        env.storage()
            .instance()
            .get(&DataKey::Ids)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// View — admin that deployed / holds emergency control.
    pub fn admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("admin must be set")
    }
}

#[cfg(test)]
mod test;
