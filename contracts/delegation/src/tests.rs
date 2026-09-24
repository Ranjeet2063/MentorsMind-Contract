extern crate std;

use super::*;
use soroban_sdk::testutils::Address as _;
use soroban_sdk::{symbol_short, Address, Env};

#[contract]
pub struct MockMntToken;

#[contractimpl]
impl MockMntToken {
    pub fn set_balance(env: Env, addr: Address, amount: i128) {
        env.storage()
            .persistent()
            .set(&(symbol_short!("BAL"), addr), &amount);
    }
    pub fn balance(env: Env, addr: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&(symbol_short!("BAL"), addr))
            .unwrap_or(0)
    }
}

#[contract]
pub struct MockSnapshotContract;

#[contractimpl]
impl MockSnapshotContract {
    pub fn set_snapshot_balance(env: Env, snapshot_id: u32, addr: Address, amount: i128) {
        env.storage()
            .persistent()
            .set(&(symbol_short!("SNAP"), snapshot_id, addr), &amount);
    }
    pub fn get_snapshot_balance(env: Env, snapshot_id: u32, addr: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&(symbol_short!("SNAP"), snapshot_id, addr))
            .unwrap_or(0)
    }
}

#[test]
fn test_delegate_and_undelegate() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    token.set_balance(&a, &100i128);

    del.delegate(&a, &b);
    let got = del.get_delegate(&a);
    assert!(got.is_some());
    assert_eq!(got.unwrap(), b.clone());

    // delegated power should include a's balance for b
    assert_eq!(del.get_delegated_power(&b), 100i128);

    del.undelegate(&a);
    assert!(del.get_delegate(&a).is_none());
    assert_eq!(del.get_delegated_power(&b), 0i128);
}

/// Acceptance Criteria 1: Verifies delegated weight is correctly transferred from delegator to delegate.
#[test]
fn test_delegated_weight_transfer_from_delegator_to_delegate() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let delegator1 = Address::generate(&env);
    let delegator2 = Address::generate(&env);
    let delegate = Address::generate(&env);

    token.set_balance(&delegator1, &100i128);
    token.set_balance(&delegator2, &200i128);
    token.set_balance(&delegate, &50i128);

    // Initial state before delegation
    assert_eq!(del.get_delegated_power(&delegate), 0i128);
    assert_eq!(del.get_effective_power(&delegate), 50i128);
    assert_eq!(del.get_effective_power(&delegator1), 100i128);
    assert_eq!(del.get_effective_power(&delegator2), 200i128);

    // Delegator 1 delegates to delegate
    del.delegate(&delegator1, &delegate);
    assert_eq!(del.get_delegate(&delegator1), Some(delegate.clone()));
    // Delegated power for delegate increases by delegator1's weight (100)
    assert_eq!(del.get_delegated_power(&delegate), 100i128);
    // Delegate's effective power is own (50) + delegated (100) = 150
    assert_eq!(del.get_effective_power(&delegate), 150i128);
    // Delegator 1's effective power becomes 0 (transferred away)
    assert_eq!(del.get_effective_power(&delegator1), 0i128);

    // Delegator 2 delegates to delegate
    del.delegate(&delegator2, &delegate);
    assert_eq!(del.get_delegate(&delegator2), Some(delegate.clone()));
    // Total delegated power is now 100 + 200 = 300
    assert_eq!(del.get_delegated_power(&delegate), 300i128);
    // Delegate's effective power is own (50) + 300 = 350
    assert_eq!(del.get_effective_power(&delegate), 350i128);
    // Delegator 2's effective power becomes 0
    assert_eq!(del.get_effective_power(&delegator2), 0i128);
}

#[test]
#[should_panic(expected = "circular delegation")]
fn test_circular_depth_2() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);

    token.set_balance(&a, &10i128);
    token.set_balance(&b, &20i128);

    del.delegate(&a, &b);
    // this should panic due to circular detection (A -> B -> A)
    del.delegate(&b, &a);
}

/// Acceptance Criteria 2: Verifies circular delegation (A -> B -> C -> A) is rejected.
#[test]
#[should_panic(expected = "circular delegation")]
fn test_circular_delegation_3_hop_panic() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);

    token.set_balance(&a, &10i128);
    token.set_balance(&b, &20i128);
    token.set_balance(&c, &30i128);

    // Form chain: A -> B -> C
    del.delegate(&a, &b);
    del.delegate(&b, &c);

    // Closing cycle: C -> A (forms A -> B -> C -> A) must panic
    del.delegate(&c, &a);
}

/// Acceptance Criteria 2: Verifies circular delegation cycle detection (A -> B -> A, A -> B -> C -> A) via validate_delegation_chain.
#[test]
fn test_circular_delegation_cycle_detection_via_validate_chain() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);

    token.set_balance(&a, &10i128);
    token.set_balance(&b, &20i128);
    token.set_balance(&c, &30i128);

    // Self-delegation (A -> A) is rejected as circular delegation
    let res_self = del.try_validate_delegation_chain(&a, &a);
    assert_eq!(res_self, Err(Ok(DelegationError::CircularDelegation)));

    // 2-hop circular delegation: A -> B, then validating B -> A
    del.delegate(&a, &b);
    let res_2hop = del.try_validate_delegation_chain(&b, &a);
    assert_eq!(res_2hop, Err(Ok(DelegationError::CircularDelegation)));

    // 3-hop circular delegation: A -> B -> C, then validating C -> A
    del.delegate(&b, &c);
    let res_3hop = del.try_validate_delegation_chain(&c, &a);
    assert_eq!(res_3hop, Err(Ok(DelegationError::CircularDelegation)));
}

#[test]
#[should_panic(expected = "circular delegation")]
fn test_circular_depth_4() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);
    let d = Address::generate(&env);

    token.set_balance(&a, &10i128);
    token.set_balance(&b, &20i128);
    token.set_balance(&c, &15i128);
    token.set_balance(&d, &25i128);

    // Create chain: a->b->c->d
    del.delegate(&a, &b);
    del.delegate(&b, &c);
    del.delegate(&c, &d);
    // Try to close cycle: d->a (would create a->b->c->d->a)
    del.delegate(&d, &a);
}

#[test]
#[should_panic(expected = "circular delegation")]
fn test_circular_depth_5() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let _token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);
    let d = Address::generate(&env);
    let e = Address::generate(&env);

    // Setup: a->b->c->d->e
    del.delegate(&a, &b);
    del.delegate(&b, &c);
    del.delegate(&c, &d);
    del.delegate(&d, &e);
    // Try: e->a (creates cycle of length 5)
    del.delegate(&e, &a);
}

/// Acceptance Criteria 3: Verifies that revoking delegation restores the delegator's own weight.
#[test]
fn test_revoking_delegation_restores_delegator_weight() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);

    token.set_balance(&a, &100i128);
    token.set_balance(&b, &50i128);
    token.set_balance(&c, &25i128);

    // Initial weights
    assert_eq!(del.get_effective_power(&a), 100i128);
    assert_eq!(del.get_effective_power(&b), 50i128);
    assert_eq!(del.get_effective_power(&c), 25i128);

    // 1. Direct delegation & revocation: A -> B
    del.delegate(&a, &b);
    assert_eq!(del.get_delegate(&a), Some(b.clone()));
    assert_eq!(del.get_effective_power(&a), 0i128);
    assert_eq!(del.get_delegated_power(&b), 100i128);
    assert_eq!(del.get_effective_power(&b), 150i128);

    // Revoke delegation: A undelegates
    del.undelegate(&a);
    assert_eq!(del.get_delegate(&a), None);
    // Delegator's own weight is fully restored
    assert_eq!(del.get_effective_power(&a), 100i128);
    // Delegate's delegated power drops back to 0, effective power back to own balance (50)
    assert_eq!(del.get_delegated_power(&b), 0i128);
    assert_eq!(del.get_effective_power(&b), 50i128);

    // 2. Multiple delegators revocation: D1 and D2 delegate to Target
    let d1 = Address::generate(&env);
    let d2 = Address::generate(&env);
    let target = Address::generate(&env);

    token.set_balance(&d1, &100i128);
    token.set_balance(&d2, &200i128);
    token.set_balance(&target, &50i128);

    del.delegate(&d1, &target);
    del.delegate(&d2, &target);
    assert_eq!(del.get_effective_power(&d1), 0i128);
    assert_eq!(del.get_effective_power(&d2), 0i128);
    assert_eq!(del.get_delegated_power(&target), 300i128);
    assert_eq!(del.get_effective_power(&target), 350i128);

    // D1 revokes delegation:
    del.undelegate(&d1);
    // D1's weight is restored to 100
    assert_eq!(del.get_effective_power(&d1), 100i128);
    assert_eq!(del.get_delegate(&d1), None);
    // Target's delegated power drops by D1's weight (300 -> 200)
    assert_eq!(del.get_delegated_power(&target), 200i128);
    assert_eq!(del.get_effective_power(&target), 250i128);
    // D2 is still active
    assert_eq!(del.get_effective_power(&d2), 0i128);

    // D2 revokes delegation:
    del.undelegate(&d2);
    // D2's weight is restored to 200
    assert_eq!(del.get_effective_power(&d2), 200i128);
    assert_eq!(del.get_delegate(&d2), None);
    // Target's delegated power drops to 0, effective power back to own balance 50
    assert_eq!(del.get_delegated_power(&target), 0i128);
    assert_eq!(del.get_effective_power(&target), 50i128);
}

/// Acceptance Criteria 4: Integration with governance snapshot contract to verify weight attribution after delegation.
#[test]
fn test_governance_snapshot_weight_attribution() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);
    let snap_id = env.register_contract(None, MockSnapshotContract);

    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);
    let snap = MockSnapshotContractClient::new(&env, &snap_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);
    del.set_snapshot_contract(&admin, &snap_id);

    let delegator1 = Address::generate(&env);
    let delegator2 = Address::generate(&env);
    let delegate = Address::generate(&env);

    token.set_balance(&delegator1, &150i128);
    token.set_balance(&delegator2, &250i128);
    token.set_balance(&delegate, &100i128);

    // Set balances in governance snapshot contract for snapshot #1
    snap.set_snapshot_balance(&1u32, &delegator1, &150i128);
    snap.set_snapshot_balance(&1u32, &delegator2, &250i128);
    snap.set_snapshot_balance(&1u32, &delegate, &100i128);

    // Establish delegations
    del.delegate(&delegator1, &delegate);
    del.delegate(&delegator2, &delegate);

    // Create snapshot #1 in delegation contract
    del.snapshot_delegations(&1u32);

    // Verify delegation mapping at snapshot #1
    assert_eq!(
        del.get_delegation_at_snapshot(&1u32, &delegator1),
        Some(delegate.clone())
    );
    assert_eq!(
        del.get_delegation_at_snapshot(&1u32, &delegator2),
        Some(delegate.clone())
    );

    // Verify weight attribution at snapshot #1:
    // Delegated power for delegate at snapshot 1 should be 150 + 250 = 400
    assert_eq!(
        del.get_delegated_power_at_snapshot(&delegate, &1u32),
        400i128
    );

    // Test snapshot isolation:
    // Now delegator1 undelegates and delegator2 changes balance after snapshot #1
    del.undelegate(&delegator1);
    token.set_balance(&delegator2, &500i128);

    // Current state changed:
    assert_eq!(del.get_delegate(&delegator1), None);
    assert_eq!(del.get_delegated_power(&delegate), 250i128);

    // But snapshot #1 remains immutable and preserved:
    assert_eq!(
        del.get_delegation_at_snapshot(&1u32, &delegator1),
        Some(delegate.clone())
    );
    assert_eq!(
        del.get_delegated_power_at_snapshot(&delegate, &1u32),
        400i128
    );

    // Snapshot #2 with updated governance state:
    snap.set_snapshot_balance(&2u32, &delegator2, &500i128);
    del.snapshot_delegations(&2u32);

    // At snapshot #2: delegator1 has no delegation, delegator2 has 500
    assert_eq!(del.get_delegation_at_snapshot(&2u32, &delegator1), None);
    assert_eq!(
        del.get_delegation_at_snapshot(&2u32, &delegator2),
        Some(delegate.clone())
    );
    assert_eq!(
        del.get_delegated_power_at_snapshot(&delegate, &2u32),
        500i128
    );

    // Snapshot #1 still intact at 400
    assert_eq!(
        del.get_delegated_power_at_snapshot(&delegate, &1u32),
        400i128
    );
}

#[test]
fn test_chain_delegation_and_effective_power() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);

    token.set_balance(&a, &10i128);
    token.set_balance(&b, &20i128);
    token.set_balance(&c, &30i128);

    del.delegate(&a, &b);
    del.delegate(&b, &c);

    let pow_c = del.get_effective_power(&c);
    // c has own 30 + b(20) + a(10) = 60
    assert_eq!(pow_c, 60i128);

    // a delegated away -> effective power 0
    assert_eq!(del.get_effective_power(&a), 0i128);
}

#[test]
fn test_validate_chain_depth() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let _token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);

    // Chain: a->b->c (depth 2)
    del.delegate(&a, &b);
    del.delegate(&b, &c);

    // Validate chain from different starting points
    let result = del.try_validate_delegation_chain(&a, &b);
    assert!(result.is_ok());

    let result = del.try_validate_delegation_chain(&b, &c);
    assert!(result.is_ok());
}

#[test]
fn test_max_delegation_depth_configurable() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);

    let del = DelegationContractClient::new(&env, &del_id);
    let _token = MockMntTokenClient::new(&env, &token_id);

    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    // Default depth should be 10
    assert_eq!(del.get_max_delegation_depth(), 10u32);

    // Set to custom value
    del.set_max_delegation_depth(&admin, &20u32);
    assert_eq!(del.get_max_delegation_depth(), 20u32);
}

// -----------------------
// Eager cache correctness tests (#658)
// -----------------------

#[test]
fn test_get_delegated_power_is_o1_cache_read() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);
    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);
    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let target = Address::generate(&env);
    let mut expected_total: i128 = 0;
    for i in 0..20 {
        let d = Address::generate(&env);
        let bal = 10i128 + i as i128;
        token.set_balance(&d, &bal);
        del.delegate(&d, &target);
        expected_total += bal;
    }

    env.budget().reset_unlimited();
    let power = del.get_delegated_power(&target);
    assert_eq!(power, expected_total);
    // Cache read must not scale with delegator count: cheap enough to assert
    // instruction count stays well below what a linear scan of 20
    // cross-contract calls would cost.
    let cpu = env.budget().cpu_instruction_cost();
    assert!(cpu < 200_000, "cache read too expensive: {}", cpu);
}

#[test]
fn test_cache_consistent_after_redelegate() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);
    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);
    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let target1 = Address::generate(&env);
    let target2 = Address::generate(&env);
    token.set_balance(&a, &50i128);

    del.delegate(&a, &target1);
    assert_eq!(del.get_delegated_power(&target1), 50i128);
    assert_eq!(del.get_delegated_power(&target2), 0i128);

    // Re-delegate directly to a new target: cache must move, not double-count.
    del.delegate(&a, &target2);
    assert_eq!(del.get_delegated_power(&target1), 0i128);
    assert_eq!(del.get_delegated_power(&target2), 50i128);
}

#[test]
fn test_cache_consistent_after_balance_change_hook() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);
    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);
    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let target = Address::generate(&env);
    token.set_balance(&a, &50i128);
    del.delegate(&a, &target);
    assert_eq!(del.get_delegated_power(&target), 50i128);

    // Simulate a token transfer hook: balance increases from 50 to 120.
    token.set_balance(&a, &120i128);
    del.invalidate_power_cache(&a, &50i128, &120i128);
    assert_eq!(del.get_delegated_power(&target), 120i128);

    // Balance decreases from 120 to 30.
    token.set_balance(&a, &30i128);
    del.invalidate_power_cache(&a, &120i128, &30i128);
    assert_eq!(del.get_delegated_power(&target), 30i128);
}

#[test]
fn test_property_cache_equals_sum_of_delegator_balances() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);
    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);
    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let target = Address::generate(&env);
    let mut delegators: std::vec::Vec<Address> = std::vec::Vec::new();
    let mut expected: i128 = 0;

    // delegate a set of accounts to target
    for i in 0..10 {
        let d = Address::generate(&env);
        let bal = (i as i128 + 1) * 7;
        token.set_balance(&d, &bal);
        del.delegate(&d, &target);
        expected += bal;
        delegators.push(d);
    }
    assert_eq!(del.get_delegated_power(&target), expected);

    // undelegate a couple, changing the sum
    del.undelegate(&delegators[2]);
    expected -= 3 * 7; // i=2 -> bal = 21
    assert_eq!(del.get_delegated_power(&target), expected);

    del.undelegate(&delegators[5]);
    expected -= 6 * 7; // i=5 -> bal = 42
    assert_eq!(del.get_delegated_power(&target), expected);

    // balance change via hook for a still-delegating account
    let d0 = delegators[0].clone();
    let old_bal = 7i128;
    let new_bal = 100i128;
    token.set_balance(&d0, &new_bal);
    del.invalidate_power_cache(&d0, &old_bal, &new_bal);
    expected += new_bal - old_bal;
    assert_eq!(del.get_delegated_power(&target), expected);
}

#[test]
fn test_intermediate_redelegation_moves_entire_subtree() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);
    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);
    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);
    let d = Address::generate(&env);

    token.set_balance(&a, &10i128);
    token.set_balance(&b, &20i128);

    del.delegate(&a, &b);
    del.delegate(&b, &c);
    assert_eq!(del.get_delegated_power(&c), 30i128); // a(10) + b(20)

    // b re-delegates to d; a's 10 must follow, not stay stuck on c.
    del.delegate(&b, &d);
    assert_eq!(del.get_delegated_power(&c), 0i128);
    assert_eq!(del.get_delegated_power(&d), 30i128);
}

#[test]
fn test_deep_chain_redelegation_moves_whole_subtree() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);
    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);
    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let a = Address::generate(&env);
    let b = Address::generate(&env);
    let c = Address::generate(&env);
    let e = Address::generate(&env);
    let f = Address::generate(&env);

    token.set_balance(&a, &5i128);
    token.set_balance(&b, &7i128);
    token.set_balance(&c, &11i128);

    del.delegate(&a, &b);
    del.delegate(&b, &c);
    del.delegate(&c, &e);
    assert_eq!(del.get_delegated_power(&e), 23i128); // 5+7+11

    del.delegate(&c, &f);
    assert_eq!(del.get_delegated_power(&e), 0i128);
    assert_eq!(del.get_delegated_power(&f), 23i128);

    // a and b's individual undelegate/re-delegate still work correctly
    // after the subtree moved.
    del.undelegate(&a);
    assert_eq!(del.get_delegated_power(&f), 18i128); // 23 - 5
}

#[test]
fn test_get_delegated_power_o1_with_500_delegators() {
    let env = Env::default();
    env.mock_all_auths();

    let del_id = env.register_contract(None, DelegationContract);
    let token_id = env.register_contract(None, MockMntToken);
    let del = DelegationContractClient::new(&env, &del_id);
    let token = MockMntTokenClient::new(&env, &token_id);
    let admin = Address::generate(&env);
    del.initialize(&admin, &token_id);

    let target = Address::generate(&env);
    let mut expected: i128 = 0;
    for _ in 0..500 {
        let d = Address::generate(&env);
        token.set_balance(&d, &1i128);
        del.delegate(&d, &target);
        expected += 1;
    }

    env.budget().reset_unlimited();
    let power = del.get_delegated_power(&target);
    let cpu = env.budget().cpu_instruction_cost();
    assert_eq!(power, expected);
    assert!(
        cpu < 500_000,
        "get_delegated_power cost scaled with delegator count: {}",
        cpu
    );
}
