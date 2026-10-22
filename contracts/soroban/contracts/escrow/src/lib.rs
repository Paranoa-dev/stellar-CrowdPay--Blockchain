#![no_std]
use soroban_sdk::{contract, contractimpl, contracttype, symbol_short, Address, Env, token};

// ~1 year in ledgers (5s per ledger): 6_312_000. Use a safe ceiling.
const INSTANCE_TTL_BUMP: u32 = 6_312_000;
const INSTANCE_TTL_THRESHOLD: u32 = 100_000;
const PERSISTENT_TTL_BUMP: u32 = 6_312_000;
const PERSISTENT_TTL_THRESHOLD: u32 = 100_000;

#[cfg(test)]
mod test;

#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    Admin,
    CampaignId,
    Target,
    Deadline,
    Asset,
    Balances(Address),
    TotalRaised,
    ApprovedWithdrawal,
    IsInitialized,
}

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    pub fn initialize(env: Env, admin: Address, campaign_id: u64, target: i128, deadline: u64, asset: Address) {
        if env.storage().instance().has(&DataKey::IsInitialized) {
            panic!("Contract is already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::CampaignId, &campaign_id);
        env.storage().instance().set(&DataKey::Target, &target);
        env.storage().instance().set(&DataKey::Deadline, &deadline);
        env.storage().instance().set(&DataKey::Asset, &asset);
        env.storage().instance().set(&DataKey::TotalRaised, &0i128);
        env.storage().instance().set(&DataKey::ApprovedWithdrawal, &0i128);
        env.storage().instance().set(&DataKey::IsInitialized, &true);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
    }

    pub fn deposit(env: Env, from: Address, amount: i128) {
        from.require_auth();

        let deadline: u64 = env.storage().instance().get(&DataKey::Deadline).unwrap();
        if env.ledger().timestamp() >= deadline {
            panic!("Deadline has passed");
        }

        let asset: Address = env.storage().instance().get(&DataKey::Asset).unwrap();
        let client = token::Client::new(&env, &asset);
        client.transfer(&from, &env.current_contract_address(), &amount);

        let balance_key = DataKey::Balances(from.clone());
        let current_balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);
        let new_balance = current_balance + amount;
        env.storage().persistent().set(&balance_key, &new_balance);
        env.storage().persistent().extend_ttl(&balance_key, PERSISTENT_TTL_THRESHOLD, PERSISTENT_TTL_BUMP);

        let total_raised: i128 = env.storage().instance().get(&DataKey::TotalRaised).unwrap_or(0);
        env.storage().instance().set(&DataKey::TotalRaised, &(total_raised + amount));
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("deposit"), from),
            amount,
        );
    }

    pub fn withdraw(env: Env, to: Address, amount: i128) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        admin.require_auth();

        let deadline: u64 = env.storage().instance().get(&DataKey::Deadline).unwrap();
        let total_raised: i128 = env.storage().instance().get(&DataKey::TotalRaised).unwrap_or(0);
        let target: i128 = env.storage().instance().get(&DataKey::Target).unwrap();

        let target_reached = total_raised >= target;
        let deadline_passed = env.ledger().timestamp() >= deadline;

        if !target_reached && !deadline_passed {
            panic!("Cannot withdraw: target not reached and deadline has not passed");
        }

        if !target_reached && deadline_passed {
            panic!("Campaign failed, cannot withdraw funds");
        }

        let asset: Address = env.storage().instance().get(&DataKey::Asset).unwrap();
        let client = token::Client::new(&env, &asset);
        client.transfer(&env.current_contract_address(), &to, &amount);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("withdraw"), to),
            amount,
        );
    }

    /// Approve a withdrawal. Only allowed once the campaign has succeeded
    /// (target reached); failed campaigns must remain refundable.
    pub fn approve_withdrawal(env: Env, release_amount: i128) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        admin.require_auth();

        let total_raised: i128 = env.storage().instance().get(&DataKey::TotalRaised).unwrap_or(0);
        let target: i128 = env.storage().instance().get(&DataKey::Target).unwrap();
        if total_raised < target {
            panic!("Campaign has not succeeded");
        }

        let current_approved: i128 = env.storage().instance().get(&DataKey::ApprovedWithdrawal).unwrap_or(0);
        let approved = current_approved + release_amount;
        env.storage().instance().set(&DataKey::ApprovedWithdrawal, &approved);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("approve"), admin),
            release_amount,
        );
    }

    /// Revoke a previously granted approval. Admin-only and bounded by the
    /// currently approved amount.
    pub fn revoke_withdrawal(env: Env, release_amount: i128) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        admin.require_auth();

        if release_amount <= 0 {
            panic!("Amount must be positive");
        }
        let approved: i128 = env.storage().instance().get(&DataKey::ApprovedWithdrawal).unwrap_or(0);
        if release_amount > approved {
            panic!("Cannot revoke more than approved");
        }

        env.storage().instance().set(&DataKey::ApprovedWithdrawal, &(approved - release_amount));
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("revoke"), admin),
            release_amount,
        );
    }

    pub fn execute_withdrawal(env: Env, to: Address, release_amount: i128) {
        let approved: i128 = env.storage().instance().get(&DataKey::ApprovedWithdrawal).unwrap_or(0);
        if approved < release_amount {
            panic!("Insufficient approved amount");
        }

        let asset: Address = env.storage().instance().get(&DataKey::Asset).unwrap();
        let client = token::Client::new(&env, &asset);
        client.transfer(&env.current_contract_address(), &to, &release_amount);

        env.storage().instance().set(&DataKey::ApprovedWithdrawal, &(approved - release_amount));
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("withdraw"), to),
            release_amount,
        );
    }

    pub fn approve_milestone_withdrawal(env: Env, caller: Address, amount: i128) {
        caller.require_auth();
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if caller != admin {
            panic!("Unauthorized caller");
        }

        let current_approved: i128 = env.storage().instance().get(&DataKey::ApprovedWithdrawal).unwrap_or(0);
        let approved = current_approved + amount;
        env.storage().instance().set(&DataKey::ApprovedWithdrawal, &approved);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("approve"), caller),
            amount,
        );
    }

    pub fn refund(env: Env, contributor: Address) {
        let deadline: u64 = env.storage().instance().get(&DataKey::Deadline).unwrap();
        if env.ledger().timestamp() < deadline {
            panic!("Deadline has not passed");
        }

        let total_raised: i128 = env.storage().instance().get(&DataKey::TotalRaised).unwrap_or(0);
        let target: i128 = env.storage().instance().get(&DataKey::Target).unwrap();

        if total_raised >= target {
            panic!("Campaign succeeded, refunds unavailable");
        }

        let balance_key = DataKey::Balances(contributor.clone());
        let amount: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);
        if amount <= 0 {
            panic!("No contribution to refund");
        }

        // Checks-effects-interactions: clear the contributor's balance and
        // reduce the total before handing control to the token contract.
        let new_total = total_raised.saturating_sub(amount);
        env.storage().instance().set(&DataKey::TotalRaised, &new_total);
        env.storage().persistent().set(&balance_key, &0i128);

        let asset: Address = env.storage().instance().get(&DataKey::Asset).unwrap();
        let client = token::Client::new(&env, &asset);
        client.transfer(&env.current_contract_address(), &contributor, &amount);

        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("refund"), contributor),
            amount,
        );
    }

    pub fn get_total_raised(env: Env) -> i128 {
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
        env.storage().instance().get(&DataKey::TotalRaised).unwrap_or(0)
    }

    pub fn get_asset(env: Env) -> Address {
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
        env.storage().instance().get(&DataKey::Asset).unwrap()
    }
}
