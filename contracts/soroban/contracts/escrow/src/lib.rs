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
    Paused,
    PendingAdmin,
    UpgradeWasmHash,
}

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if *caller != admin {
            panic!("Unauthorized caller");
        }
    }

    fn assert_not_paused(env: &Env) {
        let paused: bool = env.storage().instance().get(&DataKey::Paused).unwrap_or(false);
        if paused {
            panic!("Contract is paused");
        }
    }

    pub fn initialize(env: Env, admin: Address, campaign_id: u64, target: i128, deadline: u64, asset: Address) {
        // Issue #212: Prevent front-running by requiring deployer auth
        // Only the contract deployer can initialize - we check that the contract hasn't been initialized yet
        if env.storage().instance().has(&DataKey::IsInitialized) {
            panic!("Contract is already initialized");
        }
        // Store the deployer as the initial admin
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::CampaignId, &campaign_id);
        env.storage().instance().set(&DataKey::Target, &target);
        env.storage().instance().set(&DataKey::Deadline, &deadline);
        env.storage().instance().set(&DataKey::Asset, &asset);
        env.storage().instance().set(&DataKey::TotalRaised, &0i128);
        env.storage().instance().set(&DataKey::ApprovedWithdrawal, &0i128);
        env.storage().instance().set(&DataKey::IsInitialized, &true);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
    }

    pub fn deposit(env: Env, from: Address, amount: i128) {
        from.require_auth();
        Self::assert_not_paused(&env);

        if amount <= 0 {
            panic!("Invalid amount");
        }

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
        Self::assert_not_paused(&env);

        if amount <= 0 {
            panic!("Invalid amount");
        }

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

        // CEI Fix (Issue #215): Update state BEFORE external call
        let new_total_raised = total_raised.saturating_sub(amount);
        env.storage().instance().set(&DataKey::TotalRaised, &new_total_raised);

        let asset: Address = env.storage().instance().get(&DataKey::Asset).unwrap();
        let client = token::Client::new(&env, &asset);
        client.transfer(&env.current_contract_address(), &to, &amount);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("withdraw"), to),
            amount,
        );
    }

    pub fn approve_milestone_withdrawal(env: Env, caller: Address, amount: i128) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        Self::assert_not_paused(&env);

        if amount <= 0 {
            panic!("Invalid amount");
        }

        // Issue #213: Check deadline and target before allowing approval
        let deadline: u64 = env.storage().instance().get(&DataKey::Deadline).unwrap();
        let total_raised: i128 = env.storage().instance().get(&DataKey::TotalRaised).unwrap_or(0);
        let target: i128 = env.storage().instance().get(&DataKey::Target).unwrap();

        let target_reached = total_raised >= target;
        let deadline_passed = env.ledger().timestamp() >= deadline;

        // Only allow approval if target is reached or deadline has passed (for partial withdrawals)
        // If deadline passed and target not reached, campaign failed - no withdrawals allowed
        if !target_reached && deadline_passed {
            panic!("Campaign failed, cannot withdraw funds");
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

    // Issue #213: Add revoke functionality for approved withdrawals
    pub fn revoke_withdrawal_approval(env: Env, caller: Address, amount: i128) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        Self::assert_not_paused(&env);

        if amount <= 0 {
            panic!("Invalid amount");
        }

        let current_approved: i128 = env.storage().instance().get(&DataKey::ApprovedWithdrawal).unwrap_or(0);
        if current_approved < amount {
            panic!("Insufficient approved amount");
        }

        let new_approved = current_approved - amount;
        env.storage().instance().set(&DataKey::ApprovedWithdrawal, &new_approved);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("revoke"), caller),
            amount,
        );
    }

    // Milestones contract integration: approve withdrawal amount
    // Called by milestones contract (which is admin) to approve withdrawal amount
    pub fn approve_withdrawal(env: Env, caller: Address, amount: i128) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        Self::assert_not_paused(&env);

        if amount <= 0 {
            panic!("Invalid amount");
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

    // Milestones contract integration: execute withdrawal to recipient
    // Called by milestones contract (which is admin) to execute approved withdrawal
    pub fn execute_withdrawal(env: Env, caller: Address, to: Address, amount: i128) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        Self::assert_not_paused(&env);

        if amount <= 0 {
            panic!("Invalid amount");
        }

        let current_approved: i128 = env.storage().instance().get(&DataKey::ApprovedWithdrawal).unwrap_or(0);
        if current_approved < amount {
            panic!("Insufficient approved amount");
        }

        // CEI Fix (Issue #215): Update state BEFORE external call
        let new_approved = current_approved - amount;
        env.storage().instance().set(&DataKey::ApprovedWithdrawal, &new_approved);

        let total_raised: i128 = env.storage().instance().get(&DataKey::TotalRaised).unwrap_or(0);
        let new_total_raised = total_raised.saturating_sub(amount);
        env.storage().instance().set(&DataKey::TotalRaised, &new_total_raised);

        let asset: Address = env.storage().instance().get(&DataKey::Asset).unwrap();
        let client = token::Client::new(&env, &asset);
        client.transfer(&env.current_contract_address(), &to, &amount);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("exec_wdr"), to),
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

        // CEI Fix (Issue #215): Update state BEFORE external call
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

    // Issue #214: Admin rotation
    pub fn propose_new_admin(env: Env, caller: Address, new_admin: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);
        Self::assert_not_paused(&env);

        env.storage().instance().set(&DataKey::PendingAdmin, &new_admin);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("prop_adm"), caller),
            new_admin,
        );
    }

    pub fn accept_admin(env: Env, caller: Address) {
        caller.require_auth();
        let pending_admin: Address = env.storage().instance().get(&DataKey::PendingAdmin).unwrap();
        if caller != pending_admin {
            panic!("Unauthorized caller");
        }

        env.storage().instance().set(&DataKey::Admin, &caller);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("acc_admin"), caller),
            (),
        );
    }

    // Issue #214: Pause functionality
    pub fn pause(env: Env, caller: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        env.storage().instance().set(&DataKey::Paused, &true);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("pause"), caller),
            (),
        );
    }

    pub fn unpause(env: Env, caller: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("unpause"), caller),
            (),
        );
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage().instance().get(&DataKey::Paused).unwrap_or(false)
    }

    // Issue #214: Upgrade authorization
    pub fn propose_upgrade(env: Env, caller: Address, wasm_hash: soroban_sdk::BytesN<32>) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        env.storage().instance().set(&DataKey::UpgradeWasmHash, &wasm_hash);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("prop_upgr"), caller),
            wasm_hash,
        );
    }

    pub fn execute_upgrade(env: Env, caller: Address) {
        caller.require_auth();
        Self::require_admin(&env, &caller);

        let wasm_hash: soroban_sdk::BytesN<32> = env.storage().instance().get(&DataKey::UpgradeWasmHash).unwrap();
        
        // Deploy new wasm and upgrade
        env.deployer().update_current_contract_wasm(wasm_hash.clone());
        
        env.storage().instance().remove(&DataKey::UpgradeWasmHash);
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);

        env.events().publish(
            (symbol_short!("exec_upgr"), caller),
            wasm_hash,
        );
    }

    // View functions
    pub fn get_approved_withdrawal(env: Env) -> i128 {
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
        env.storage().instance().get(&DataKey::ApprovedWithdrawal).unwrap_or(0)
    }

    pub fn get_campaign_id(env: Env) -> u64 {
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
        env.storage().instance().get(&DataKey::CampaignId).unwrap_or(0)
    }

    pub fn get_deadline(env: Env) -> u64 {
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
        env.storage().instance().get(&DataKey::Deadline).unwrap_or(0)
    }

    pub fn get_target(env: Env) -> i128 {
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
        env.storage().instance().get(&DataKey::Target).unwrap_or(0)
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
        env.storage().instance().get(&DataKey::Admin).unwrap()
    }

    pub fn get_pending_admin(env: Env) -> Option<Address> {
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
        env.storage().instance().get(&DataKey::PendingAdmin)
    }

    pub fn get_upgrade_wasm_hash(env: Env) -> Option<soroban_sdk::BytesN<32>> {
        env.storage().instance().extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_BUMP);
        env.storage().instance().get(&DataKey::UpgradeWasmHash)
    }
}