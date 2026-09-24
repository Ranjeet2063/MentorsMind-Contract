#![no_std]

use shared::events::{
    emit_delegation_event, evt_del_delegated, evt_del_suspended, evt_del_undelegated,
};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, vec, Address, Env, IntoVal,
    Symbol,
};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Contract-isolated storage namespace root (#826).
    NamespaceRoot,
    Admin,
    MNTToken,
    SnapshotContract,
    Delegate(Address),            // mapping: delegator -> delegate
    Delegators,                   // Vec<Address>
    MaxDelegationDepth,           // u32: configurable max depth for cycle detection
    DelegatedPowerCache(Address), // eager cache: ultimate delegate -> sum of delegator balances
    /// Eager subtree weight: `SubtreeWeight(X)` = own balance (if X is a
    /// delegator) + the subtree weight of everyone whose delegate link
    /// points directly at X. This lets a re-delegation move an entire
    /// subtree of followers to a new ultimate target in O(1), instead of
    /// having to walk every follower.
    SubtreeWeight(Address),
    /// Historical delegation snapshot: (snapshot_id, delegator) -> delegate address at that snapshot
    DelegationAtSnapshot(u32, Address),
    /// Snapshot-time delegated power cache: (snapshot_id, delegate) -> total delegated power
    DelegationSnapshot(u32, Address),
    /// Admin-configured concentration cap. Defaults to 10_000 for backward
    /// compatibility; governance can lower it to enforce anti-capture policy.
    MaxDelegatedPowerBps,
    /// Emergency switch that forces direct voting fallback by blocking new
    /// delegation writes while preserving existing read-only snapshots.
    DelegationSuspended,
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum DelegationError {
    CircularDelegation = 1,
    DepthExceeded = 2,
    ConcentrationExceeded = 3,
    DelegationSuspended = 4,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DelegatedEventData {
    pub delegator: Address,
    pub delegate: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UndelegatedEventData {
    pub delegator: Address,
}

#[contract]
pub struct DelegationContract;

#[contractimpl]
impl DelegationContract {
    pub fn initialize(env: Env, admin: Address, mnt_token: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::MNTToken, &mnt_token);
        // Set default max delegation depth to 10
        env.storage()
            .instance()
            .set(&DataKey::MaxDelegationDepth, &10u32);
        env.storage().instance().set(
            &DataKey::MaxDelegatedPowerBps,
            &shared::DEFAULT_DELEGATION_CAP_BPS,
        );
        env.storage()
            .instance()
            .set(&DataKey::DelegationSuspended, &false);
    }

    pub fn set_max_delegation_depth(env: Env, admin: Address, depth: u32) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        admin.require_auth();
        if admin != stored_admin {
            panic!("unauthorized");
        }
        if depth < 2 || depth > 100 {
            panic!("depth must be between 2 and 100");
        }
        env.storage()
            .instance()
            .set(&DataKey::MaxDelegationDepth, &depth);
    }

    pub fn get_max_delegation_depth(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MaxDelegationDepth)
            .unwrap_or(10u32)
    }

    pub fn set_delegation_power_cap_bps(env: Env, admin: Address, cap_bps: u32) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        admin.require_auth();
        if admin != stored_admin {
            panic!("unauthorized");
        }
        if cap_bps == 0 || cap_bps > 10_000 {
            panic!("cap must be between 1 and 10000 bps");
        }
        env.storage()
            .instance()
            .set(&DataKey::MaxDelegatedPowerBps, &cap_bps);
    }

    pub fn get_delegation_power_cap_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::MaxDelegatedPowerBps)
            .unwrap_or(shared::DEFAULT_DELEGATION_CAP_BPS)
    }

    pub fn set_delegation_suspended(env: Env, admin: Address, suspended: bool) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        admin.require_auth();
        if admin != stored_admin {
            panic!("unauthorized");
        }
        env.storage()
            .instance()
            .set(&DataKey::DelegationSuspended, &suspended);
        emit_delegation_event(&env, evt_del_suspended(&env), suspended);
    }

    pub fn is_delegation_suspended(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::DelegationSuspended)
            .unwrap_or(false)
    }

    pub fn assess_delegate_concentration(
        env: Env,
        delegate: Address,
    ) -> shared::DelegationConcentrationReport {
        let total = Self::total_delegated_balance(&env);
        let delegate_power = Self::get_delegated_power(env.clone(), delegate);
        let cap = Self::get_delegation_power_cap_bps(env);
        shared::assess_delegation_concentration(total, delegate_power, cap)
    }

    /// Validate delegation chain and return its depth.
    /// Returns Ok(depth) if valid chain with no cycles.
    /// Returns Err(DelegationError::CircularDelegation) if cycle detected.
    /// Returns Err(DelegationError::DepthExceeded) if depth exceeds configured max.
    pub fn validate_delegation_chain(
        env: Env,
        delegator: Address,
        delegate: Address,
    ) -> Result<u32, DelegationError> {
        if delegator == delegate {
            return Err(DelegationError::CircularDelegation);
        }

        let max_depth = Self::get_max_delegation_depth(env.clone());
        let mut seen: soroban_sdk::Vec<Address> = soroban_sdk::Vec::new(&env);
        let mut cur = delegate.clone();
        let mut depth: u32 = 0;

        loop {
            depth += 1;

            // Check if we've exceeded max depth
            if depth > max_depth {
                return Err(DelegationError::DepthExceeded);
            }

            // Check if current address is the delegator (cycle detected)
            if cur == delegator {
                return Err(DelegationError::CircularDelegation);
            }

            // Check if we've seen this address before (cycle in chain)
            if seen.contains(&cur) {
                return Err(DelegationError::CircularDelegation);
            }

            // Add current to seen set
            seen.push_back(cur.clone());

            // Try to follow the chain
            if let Some(next) = env
                .storage()
                .persistent()
                .get::<_, Address>(&DataKey::Delegate(cur.clone()))
            {
                cur = next;
            } else {
                // End of chain reached successfully
                return Ok(depth);
            }
        }
    }

    pub fn delegate(env: Env, delegator: Address, delegate: Address) {
        delegator.require_auth();
        if Self::is_delegation_suspended(env.clone()) {
            panic!("delegation suspended");
        }
        if delegator == delegate {
            panic!("cannot delegate to self");
        }

        // Validate delegation chain at registration time
        match Self::validate_delegation_chain(env.clone(), delegator.clone(), delegate.clone()) {
            Ok(_) => {
                // Chain is valid, proceed
            }
            Err(DelegationError::CircularDelegation) => {
                panic!("circular delegation");
            }
            Err(DelegationError::DepthExceeded) => {
                panic!("delegation depth exceeded");
            }
            Err(DelegationError::ConcentrationExceeded) => {
                panic!("delegation concentration cap exceeded");
            }
            Err(DelegationError::DelegationSuspended) => {
                panic!("delegation suspended");
            }
        }

        let max_depth = Self::get_max_delegation_depth(env.clone());

        // `delegator`'s own balance plus everyone already delegating through
        // it (its subtree) is what needs to move to the new chain. Moving
        // this single stored number is O(depth), not O(followers) — a
        // re-delegation never needs to touch each individual follower.
        let weight = Self::get_subtree_weight(&env, &delegator);

        let previous_delegate: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Delegate(delegator.clone()));

        if let Some(prev) = previous_delegate {
            Self::propagate_weight_change(&env, &prev, -weight, max_depth);
        }

        // Add delegator to delegators list if not present
        let mut delegators: soroban_sdk::Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Delegators)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));

        let cap = Self::get_delegation_power_cap_bps(env.clone());
        if cap < shared::DEFAULT_DELEGATION_CAP_BPS {
            let total_after = Self::total_delegated_balance_from(&env, &delegators, &delegator);
            let current_delegate_power = Self::get_delegated_power(env.clone(), delegate.clone());
            let projected = current_delegate_power
                .checked_add(weight)
                .expect("overflow");
            let report = shared::assess_delegation_concentration(total_after, projected, cap);
            if report.cap_exceeded {
                if let Some(prev) = env
                    .storage()
                    .persistent()
                    .get::<_, Address>(&DataKey::Delegate(delegator.clone()))
                {
                    Self::propagate_weight_change(&env, &prev, weight, max_depth);
                }
                panic!("delegation concentration cap exceeded");
            }
        }

        env.storage()
            .persistent()
            .set(&DataKey::Delegate(delegator.clone()), &delegate.clone());

        if !delegators.contains(&delegator) {
            delegators.push_back(delegator.clone());
            env.storage()
                .persistent()
                .set(&DataKey::Delegators, &delegators);
        }

        Self::propagate_weight_change(&env, &delegate, weight, max_depth);

        emit_delegation_event(
            &env,
            evt_del_delegated(&env),
            DelegatedEventData {
                delegator,
                delegate,
            },
        );
    }

    pub fn undelegate(env: Env, delegator: Address) {
        delegator.require_auth();
        let existing: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Delegate(delegator.clone()));
        let delegate = match existing {
            Some(d) => d,
            None => return,
        };

        let max_depth = Self::get_max_delegation_depth(env.clone());
        let weight = Self::get_subtree_weight(&env, &delegator);

        env.storage()
            .persistent()
            .remove(&DataKey::Delegate(delegator.clone()));

        // remove from delegators list if present
        let mut delegators: soroban_sdk::Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Delegators)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));
        if let Some(index) = delegators.first_index_of(&delegator) {
            delegators.remove(index);
            env.storage()
                .persistent()
                .set(&DataKey::Delegators, &delegators);
        }

        Self::propagate_weight_change(&env, &delegate, -weight, max_depth);

        emit_delegation_event(
            &env,
            evt_del_undelegated(&env),
            UndelegatedEventData { delegator },
        );
    }

    pub fn get_delegate(env: Env, delegator: Address) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::Delegate(delegator))
    }

    /// O(1) read of cached delegated power for `delegate`.
    pub fn get_delegated_power(env: Env, delegate: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::DelegatedPowerCache(delegate))
            .unwrap_or(0)
    }

    pub fn get_effective_power(env: Env, voter: Address) -> i128 {
        // If voter delegated away, effective power is 0
        if env
            .storage()
            .persistent()
            .has(&DataKey::Delegate(voter.clone()))
        {
            return 0;
        }
        let own = Self::token_balance(&env, &voter);
        let delegated = Self::get_delegated_power(env.clone(), voter.clone());
        own.checked_add(delegated).expect("overflow")
    }

    /// Callable by the MNT token contract on balance change (transfer hook) to
    /// keep the eager delegated-power cache consistent with actual balances.
    /// `old_balance`/`new_balance` describe `delegator`'s balance before/after
    /// the change; the delta is propagated up `delegator`'s chain, updating
    /// every ancestor's subtree weight and the ultimate delegate's cache.
    pub fn invalidate_power_cache(
        env: Env,
        delegator: Address,
        old_balance: i128,
        new_balance: i128,
    ) {
        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::MNTToken)
            .expect("token not set");
        token.require_auth();

        let delta = new_balance.checked_sub(old_balance).expect("overflow");
        if delta == 0 {
            return;
        }

        // The delegator's own subtree weight includes its own balance, so it
        // must be adjusted too, not just the ancestors above it.
        Self::adjust_subtree_weight(&env, &delegator, delta);

        if let Some(delegate) = env
            .storage()
            .persistent()
            .get::<_, Address>(&DataKey::Delegate(delegator))
        {
            let max_depth = Self::get_max_delegation_depth(env.clone());
            Self::propagate_weight_change(&env, &delegate, delta, max_depth);
        }
    }

    /// Snapshot current delegation state at a specific proposal snapshot.
    /// Stores (snapshot_id, delegator) -> delegate for all current delegations.
    /// Called by snapshot contract at proposal creation time.
    /// TTL: delegation snapshot entries expire after 90 days.
    pub fn set_snapshot_contract(env: Env, admin: Address, snapshot_contract: Address) {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        admin.require_auth();
        if admin != stored_admin {
            panic!("unauthorized");
        }
        env.storage()
            .instance()
            .set(&DataKey::SnapshotContract, &snapshot_contract);
    }

    pub fn snapshot_delegations(env: Env, snapshot_id: u32) {
        let delegators: soroban_sdk::Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Delegators)
            .unwrap_or_else(|| soroban_sdk::Vec::new(&env));

        let snapshot_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::SnapshotContract)
            .expect("snapshot contract not set");

        let ninety_days_ledgers = 90 * 24 * 60 * 60 / 5; // ~5s per ledger

        for delegator in delegators.iter() {
            if let Some(delegate) = env
                .storage()
                .persistent()
                .get::<_, Address>(&DataKey::Delegate(delegator.clone()))
            {
                let key = DataKey::DelegationAtSnapshot(snapshot_id, delegator.clone());
                env.storage().persistent().set(&key, &delegate);
                // Auto-expire: extend TTL for 90 days
                env.storage().persistent().extend_ttl(
                    &key,
                    ninety_days_ledgers,
                    ninety_days_ledgers,
                );

                // Get delegator's staked balance at snapshot from snapshot contract
                let balance: i128 = env.invoke_contract(
                    &snapshot_contract,
                    &Symbol::new(&env, "get_snapshot_balance"),
                    vec![
                        &env,
                        snapshot_id.into_val(&env),
                        delegator.clone().into_val(&env),
                    ],
                );

                // Accumulate delegated power for the delegate at this snapshot
                if balance > 0 {
                    let power_key = DataKey::DelegationSnapshot(snapshot_id, delegate.clone());
                    let current: i128 = env.storage().persistent().get(&power_key).unwrap_or(0);
                    env.storage()
                        .persistent()
                        .set(&power_key, &current.checked_add(balance).expect("overflow"));
                    env.storage().persistent().extend_ttl(
                        &power_key,
                        ninety_days_ledgers,
                        ninety_days_ledgers,
                    );
                }
            }
        }
    }

    /// Get the delegate of a delegator at a specific snapshot.
    /// Returns None if delegator had no delegation at snapshot time.
    pub fn get_delegation_at_snapshot(
        env: Env,
        snapshot_id: u32,
        delegator: Address,
    ) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::DelegationAtSnapshot(snapshot_id, delegator))
    }

    /// Get the total delegated power received by `delegate` at a specific snapshot.
    /// Returns the sum of staked balances (at snapshot time) of all delegators
    /// whose delegate (at snapshot time) is the given address.
    pub fn get_delegated_power_at_snapshot(env: Env, delegate: Address, snapshot_id: u32) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::DelegationSnapshot(snapshot_id, delegate))
            .unwrap_or(0)
    }

    /// Subtree weight of `addr`: its own token balance plus the subtree
    /// weight of everyone whose delegate link points directly at it.
    fn get_subtree_weight(env: &Env, addr: &Address) -> i128 {
        let own_balance = Self::token_balance(env, addr);
        let followers: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::SubtreeWeight(addr.clone()))
            .unwrap_or(0);
        own_balance.checked_add(followers).expect("overflow")
    }

    fn adjust_subtree_weight(env: &Env, addr: &Address, delta: i128) {
        if delta == 0 {
            return;
        }
        let key = DataKey::SubtreeWeight(addr.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let updated = current.checked_add(delta).expect("overflow");
        env.storage().persistent().set(&key, &updated);
    }

    fn adjust_power_cache(env: &Env, delegate: &Address, delta: i128) {
        if delta == 0 {
            return;
        }
        let key = DataKey::DelegatedPowerCache(delegate.clone());
        let current: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        let updated = current.checked_add(delta).expect("overflow");
        env.storage().persistent().set(&key, &updated);
    }

    /// Walk the delegation chain starting at `start`, applying `delta` to
    /// each ancestor's `SubtreeWeight` (so their own subtree total stays
    /// correct if they are later re-delegated) and to the ultimate
    /// delegate's `DelegatedPowerCache`. Bounded by `max_depth` — never
    /// proportional to the number of delegators in the system.
    fn propagate_weight_change(env: &Env, start: &Address, delta: i128, max_depth: u32) {
        if delta == 0 {
            return;
        }
        let mut cur = start.clone();
        for _ in 0..max_depth {
            Self::adjust_subtree_weight(env, &cur, delta);
            match env
                .storage()
                .persistent()
                .get::<_, Address>(&DataKey::Delegate(cur.clone()))
            {
                Some(next) => cur = next,
                None => {
                    // `cur` is the ultimate delegate: also update its
                    // publicly-read cache.
                    Self::adjust_power_cache(env, &cur, delta);
                    return;
                }
            }
        }
        // Depth exhausted without finding an end (shouldn't happen given
        // validate_delegation_chain enforces max_depth at write time) —
        // treat `cur` as the ultimate as a defensive fallback.
        Self::adjust_power_cache(env, &cur, delta);
    }

    fn token_balance(env: &Env, addr: &Address) -> i128 {
        let token: Address = env
            .storage()
            .instance()
            .get(&DataKey::MNTToken)
            .expect("token not set");
        let client = soroban_sdk::token::Client::new(env, &token);
        client.balance(addr)
    }

    fn total_delegated_balance(env: &Env) -> i128 {
        let delegators: soroban_sdk::Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Delegators)
            .unwrap_or_else(|| soroban_sdk::Vec::new(env));
        let mut total = 0i128;
        for delegator in delegators.iter() {
            total = total
                .checked_add(Self::token_balance(env, &delegator))
                .expect("overflow");
        }
        total
    }

    fn total_delegated_balance_from(
        env: &Env,
        delegators: &soroban_sdk::Vec<Address>,
        pending: &Address,
    ) -> i128 {
        let mut total = 0i128;
        for delegator in delegators.iter() {
            total = total
                .checked_add(Self::token_balance(env, &delegator))
                .expect("overflow");
        }
        if !delegators.contains(pending) {
            total = total
                .checked_add(Self::token_balance(env, pending))
                .expect("overflow");
        }
        total
    }
}

// -----------------------
// Tests
// -----------------------

#[cfg(test)]
mod tests;

