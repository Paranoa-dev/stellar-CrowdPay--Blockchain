#![cfg(test)]

use super::*;
use escrow::{EscrowContract, EscrowContractClient};
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, BytesN, Env, Vec,
};

fn create_token_contract<'a>(env: &Env, admin: &Address) -> (token::Client<'a>, token::StellarAssetClient<'a>) {
    let contract_address = env.register_stellar_asset_contract_v2(admin.clone()).address();
    (
        token::Client::new(env, &contract_address),
        token::StellarAssetClient::new(env, &contract_address),
    )
}

fn deploy_milestones(env: &Env) -> (MilestonesContractClient, Address, BytesN<32>) {
    let deployer = Address::generate(env);
    let salt = BytesN::from_array(env, &[9u8; 32]);
    let id = env
        .deployer()
        .with_address(deployer.clone(), salt.clone())
        .deployed_address();
    env.register_at(&id, MilestonesContract, ());
    (
        MilestonesContractClient::new(env, &id),
        deployer,
        salt,
    )
}

fn deploy_escrow<'a>(
    env: &'a Env,
    admin: &Address,
    target: i128,
    deadline: u64,
    asset: &Address,
) -> EscrowContractClient<'a> {
    let deployer = Address::generate(env);
    let salt = BytesN::from_array(env, &[7u8; 32]);
    let id = env
        .deployer()
        .with_address(deployer.clone(), salt.clone())
        .deployed_address();
    env.register_at(&id, EscrowContract, ());
    let client = EscrowContractClient::new(env, &id);
    client.initialize(&deployer, &salt, admin, &1u64, &target, &deadline, asset);
    client
}

fn sample_hash(env: &Env, byte: u8) -> BytesN<32> {
    BytesN::from_array(env, &[byte; 32])
}

fn plan(env: &Env, bps: &[u32]) -> Vec<Milestone> {
    let mut v = Vec::new(env);
    for (i, b) in bps.iter().enumerate() {
        v.push_back(Milestone {
            title_hash: sample_hash(env, (i + 1) as u8),
            release_bps: *b,
            status: MilestoneStatus::Pending,
            evidence_hash: None,
        });
    }
    v
}

fn try_init(bps: &[u32]) -> bool {
    let env = Env::default();
    env.mock_all_auths();
    let (client, deployer, salt) = deploy_milestones(&env);
    let (a, b, c) = (Address::generate(&env), Address::generate(&env), Address::generate(&env));
    client.try_initialize(&deployer, &salt, &a, &b, &c, &plan(&env, bps)).is_ok()
}

#[test]
fn initialize_accepts_valid_plans() {
    assert!(try_init(&[10000]));
    assert!(try_init(&[3333, 3333, 3334]));
    assert!(try_init(&[2500, 2500, 5000]));
}

#[test]
fn initialize_rejects_invalid_plans() {
    assert!(!try_init(&[]));
    assert!(!try_init(&[5000, 4000]));
    assert!(!try_init(&[6000, 6000]));
    assert!(!try_init(&[0, 10000]));
    assert!(!try_init(&[0]));
}

#[test]
fn remainder_goes_to_final_release() {
    // 3 milestones of 33.33/33.33/33.34% on 100 raised: 33 + 33 + remainder 34.
    let total: i128 = 100;
    let bps = [3333u32, 3333, 3334];
    let mut released: i128 = 0;
    for (i, b) in bps.iter().enumerate() {
        let is_last = i == bps.len() - 1;
        released += release_amount_for(total, *b, released, is_last);
    }
    assert_eq!(released, total);
}

#[test]
fn total_withdrawn_equals_total_raised_across_n_milestones() {
    for n in 1u32..=10 {
        let base = 10000 / n;
        let mut bps = [base; 10];
        bps[(n - 1) as usize] += 10000 - base * n;
        for total in [1i128, 7, 99, 1_000_003, 123_456_789_012_345] {
            let mut released: i128 = 0;
            for i in 0..n {
                released += release_amount_for(total, bps[i as usize], released, i == n - 1);
            }
            assert_eq!(released, total, "n={} total={}", n, total);
        }
    }
}

#[test]
#[should_panic(expected = "Already initialized")]
fn test_double_initialize_panics() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, deployer, salt) = deploy_milestones(&env);
    let (creator, platform, escrow) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    let p = plan(&env, &[5000, 5000]);
    client.initialize(&deployer, &salt, &creator, &platform, &escrow, &p);
    client.initialize(&deployer, &salt, &creator, &platform, &escrow, &p);
}

#[test]
fn test_get_milestone_and_get_all_milestones() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, deployer, salt) = deploy_milestones(&env);
    let (creator, platform, escrow) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    let p = plan(&env, &[3000, 7000]);
    client.initialize(&deployer, &salt, &creator, &platform, &escrow, &p);

    let m0 = client.get_milestone(&0);
    assert_eq!(m0.release_bps, 3000);
    assert_eq!(m0.status, MilestoneStatus::Pending);
    assert_eq!(m0.evidence_hash, None);

    let m1 = client.get_milestone(&1);
    assert_eq!(m1.release_bps, 7000);
    assert_eq!(m1.status, MilestoneStatus::Pending);

    let all = client.get_all_milestones();
    assert_eq!(all.len(), 2);
}

#[test]
fn test_submit_milestone_workflow() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, deployer, salt) = deploy_milestones(&env);
    let (creator, platform, escrow) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    let p = plan(&env, &[5000, 5000]);
    client.initialize(&deployer, &salt, &creator, &platform, &escrow, &p);

    let evidence = sample_hash(&env, 99);
    client.submit_milestone(&0, &evidence);

    let m0 = client.get_milestone(&0);
    assert_eq!(m0.status, MilestoneStatus::Submitted);
    assert_eq!(m0.evidence_hash, Some(evidence));
}

#[test]
fn test_submit_milestone_auth() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, deployer, salt) = deploy_milestones(&env);
    let (creator, platform, escrow) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    let p = plan(&env, &[10000]);
    client.initialize(&deployer, &salt, &creator, &platform, &escrow, &p);

    let evidence = sample_hash(&env, 1);
    client.submit_milestone(&0, &evidence);

    assert!(env.auths().iter().any(|(addr, _)| addr == &creator));
}

#[test]
#[should_panic(expected = "Milestone already submitted or approved")]
fn test_submit_already_submitted_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, deployer, salt) = deploy_milestones(&env);
    let (creator, platform, escrow) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    client.initialize(&deployer, &salt, &creator, &platform, &escrow, &plan(&env, &[10000]));

    let evidence = sample_hash(&env, 1);
    client.submit_milestone(&0, &evidence);
    client.submit_milestone(&0, &evidence);
}

#[test]
#[should_panic(expected = "Invalid index")]
fn test_submit_invalid_index_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, deployer, salt) = deploy_milestones(&env);
    let (creator, platform, escrow) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    client.initialize(&deployer, &salt, &creator, &platform, &escrow, &plan(&env, &[10000]));

    let evidence = sample_hash(&env, 1);
    client.submit_milestone(&5, &evidence);
}

#[test]
fn test_reject_and_resubmit_milestone() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, deployer, salt) = deploy_milestones(&env);
    let (creator, platform, escrow) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    client.initialize(&deployer, &salt, &creator, &platform, &escrow, &plan(&env, &[10000]));

    // Submit milestone
    let ev1 = sample_hash(&env, 1);
    client.submit_milestone(&0, &ev1);
    assert_eq!(client.get_milestone(&0).status, MilestoneStatus::Submitted);

    // Platform rejects
    let reason = sample_hash(&env, 0xEE);
    client.reject_milestone(&0, &reason);
    assert_eq!(client.get_milestone(&0).status, MilestoneStatus::Rejected);

    // Creator can re-submit after rejection
    let ev2 = sample_hash(&env, 2);
    client.submit_milestone(&0, &ev2);
    let m = client.get_milestone(&0);
    assert_eq!(m.status, MilestoneStatus::Submitted);
    assert_eq!(m.evidence_hash, Some(ev2));
}

#[test]
#[should_panic(expected = "Milestone not submitted")]
fn test_reject_pending_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, deployer, salt) = deploy_milestones(&env);
    let (creator, platform, escrow) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    client.initialize(&deployer, &salt, &creator, &platform, &escrow, &plan(&env, &[10000]));

    let reason = sample_hash(&env, 0xEE);
    client.reject_milestone(&0, &reason);
}

#[test]
#[should_panic(expected = "Milestone not submitted")]
fn test_approve_pending_milestone_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, deployer, salt) = deploy_milestones(&env);
    let (creator, platform, escrow) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    client.initialize(&deployer, &salt, &creator, &platform, &escrow, &plan(&env, &[10000]));

    client.approve_milestone(&0);
}

#[test]
fn test_end_to_end_milestone_release_with_escrow() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100);

    let token_admin = Address::generate(&env);
    let (token_client, stellar_client) = create_token_contract(&env, &token_admin);

    let creator = Address::generate(&env);
    let platform = Address::generate(&env);
    let contributor = Address::generate(&env);

    stellar_client.mint(&contributor, &1000);

    // Register Milestones contract and Escrow contract
    // Register Milestones contract and Escrow contract
    let (milestones_client, m_deployer, m_salt) = deploy_milestones(&env);
    let milestones_id = milestones_client.address.clone();

    let escrow_client = deploy_escrow(&env, &milestones_id, 1000, 500, &token_client.address);
    let escrow_id = escrow_client.address.clone();

    // Initialize Milestones contract with 3 milestones: 30%, 30%, 40%
    let p = plan(&env, &[3000, 3000, 4000]);
    milestones_client.initialize(&m_deployer, &m_salt, &creator, &platform, &escrow_id, &p);

    // Contributor deposits 1000 into Escrow
    escrow_client.deposit(&contributor, &1000);
    assert_eq!(escrow_client.get_total_raised(), 1000);
    assert_eq!(token_client.balance(&escrow_id), 1000);
    assert_eq!(token_client.balance(&creator), 0);

    // --- Milestone 0 (30%) ---
    let ev0 = sample_hash(&env, 0);
    milestones_client.submit_milestone(&0, &ev0);
    milestones_client.approve_milestone(&0);

    assert_eq!(milestones_client.get_milestone(&0).status, MilestoneStatus::Approved);
    assert_eq!(token_client.balance(&creator), 300);
    assert_eq!(token_client.balance(&escrow_id), 700);

    // --- Milestone 1 (30%) - Reject then re-submit and approve ---
    let ev1_draft = sample_hash(&env, 1);
    milestones_client.submit_milestone(&1, &ev1_draft);
    milestones_client.reject_milestone(&1, &sample_hash(&env, 0xAA));
    assert_eq!(milestones_client.get_milestone(&1).status, MilestoneStatus::Rejected);

    let ev1_final = sample_hash(&env, 2);
    milestones_client.submit_milestone(&1, &ev1_final);
    milestones_client.approve_milestone(&1);

    assert_eq!(milestones_client.get_milestone(&1).status, MilestoneStatus::Approved);
    assert_eq!(token_client.balance(&creator), 600);
    assert_eq!(token_client.balance(&escrow_id), 400);

    // --- Milestone 2 (40% - Final Milestone) ---
    let ev2 = sample_hash(&env, 3);
    milestones_client.submit_milestone(&2, &ev2);
    milestones_client.approve_milestone(&2);

    assert_eq!(milestones_client.get_milestone(&2).status, MilestoneStatus::Approved);
    // All 1000 tokens released to creator
    assert_eq!(token_client.balance(&creator), 1000);
    assert_eq!(token_client.balance(&escrow_id), 0);
}

#[test]
fn test_end_to_end_dust_sweeping_on_final_release() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().set_timestamp(100);

    let token_admin = Address::generate(&env);
    let (token_client, stellar_client) = create_token_contract(&env, &token_admin);

    let creator = Address::generate(&env);
    let platform = Address::generate(&env);
    let contributor = Address::generate(&env);

    // 100 tokens raised total across 3333, 3333, 3334 bps
    stellar_client.mint(&contributor, &100);

    let (milestones_client, m_deployer, m_salt) = deploy_milestones(&env);
    let milestones_id = milestones_client.address.clone();

    let escrow_client = deploy_escrow(&env, &milestones_id, 100, 500, &token_client.address);
    let escrow_id = escrow_client.address.clone();

    let p = plan(&env, &[3333, 3333, 3334]);
    milestones_client.initialize(&m_deployer, &m_salt, &creator, &platform, &escrow_id, &p);

    escrow_client.deposit(&contributor, &100);

    // Milestone 0: (100 * 3333) / 10000 = 33 tokens
    milestones_client.submit_milestone(&0, &sample_hash(&env, 0));
    milestones_client.approve_milestone(&0);
    assert_eq!(token_client.balance(&creator), 33);

    // Milestone 1: (100 * 3333) / 10000 = 33 tokens
    milestones_client.submit_milestone(&1, &sample_hash(&env, 1));
    milestones_client.approve_milestone(&1);
    assert_eq!(token_client.balance(&creator), 66);

    // Milestone 2 (Final): sweeps remaining 100 - 66 = 34 tokens
    milestones_client.submit_milestone(&2, &sample_hash(&env, 2));
    milestones_client.approve_milestone(&2);
    assert_eq!(token_client.balance(&creator), 100);
    assert_eq!(token_client.balance(&escrow_id), 0);
}

#[test]
fn initialize_rejects_non_deployer() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _deployer, salt) = deploy_milestones(&env);
    let (creator, platform, escrow) = (
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    );
    let attacker = Address::generate(&env);
    let result = client.try_initialize(
        &attacker,
        &salt,
        &creator,
        &platform,
        &escrow,
        &plan(&env, &[10000]),
    );
    assert!(result.is_err());
}
