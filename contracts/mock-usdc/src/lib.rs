//! TEST-ONLY minimal NEP-141 fungible token for local sandbox tests.
//! `mint` is permissionless by design. This contract must never hold or
//! represent real value; production ZeroTick must be configured with a
//! real audited USDC token (e.g. Circle USDC on NEAR mainnet).

use near_sdk::json_types::U128;
use near_sdk::{AccountId, Gas, NearToken, Promise, PromiseOrValue, env, ext_contract, near};
use std::collections::HashMap;

#[ext_contract(ext_receiver)]
pub trait FtReceiver {
    fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128>;
}

const GAS_ON_TRANSFER: Gas = Gas::from_tgas(80);
const GAS_RESOLVE: Gas = Gas::from_tgas(20);

#[near(contract_state)]
#[derive(Default)]
pub struct MockUsdc {
    balances: HashMap<AccountId, u128>,
    total_supply: u128,
}

impl MockUsdc {
    fn internal_transfer(&mut self, from: &AccountId, to: &AccountId, amount: u128) {
        if amount == 0 {
            env::panic_str("zero amount");
        }
        let bal = self.balances.get(from).copied().unwrap_or(0);
        if bal < amount {
            env::panic_str("insufficient balance");
        }
        self.balances.insert(from.clone(), bal - amount);
        let to_bal = self.balances.get(to).copied().unwrap_or(0);
        self.balances.insert(
            to.clone(),
            to_bal
                .checked_add(amount)
                .unwrap_or_else(|| env::panic_str("balance overflow")),
        );
    }
    fn require_one_yocto() {
        if env::attached_deposit() != NearToken::from_yoctonear(1) {
            env::panic_str("requires attached deposit of exactly 1 yoctoNEAR");
        }
    }
}

#[near]
impl MockUsdc {
    #[init]
    pub fn new() -> Self {
        Self::default()
    }

    /// TEST ONLY: permissionless mint for sandbox funding.
    pub fn mint(&mut self, account_id: AccountId, amount: U128) {
        let bal = self.balances.get(&account_id).copied().unwrap_or(0);
        self.balances.insert(
            account_id,
            bal.checked_add(amount.0)
                .unwrap_or_else(|| env::panic_str("balance overflow")),
        );
        self.total_supply = self
            .total_supply
            .checked_add(amount.0)
            .unwrap_or_else(|| env::panic_str("supply overflow"));
    }

    #[payable]
    pub fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        let _ = memo;
        Self::require_one_yocto();
        let sender = env::predecessor_account_id();
        self.internal_transfer(&sender, &receiver_id, amount.0);
    }

    #[payable]
    pub fn ft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        amount: U128,
        memo: Option<String>,
        msg: String,
    ) -> Promise {
        let _ = memo;
        Self::require_one_yocto();
        let sender = env::predecessor_account_id();
        self.internal_transfer(&sender, &receiver_id, amount.0);
        ext_receiver::ext(receiver_id.clone())
            .with_static_gas(GAS_ON_TRANSFER)
            .ft_on_transfer(sender.clone(), amount, msg)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_RESOLVE)
                    .ft_resolve_transfer(sender, receiver_id, amount),
            )
    }

    /// If the receiver failed (or returned unused > 0), refund the sender.
    /// ZeroTick's receiver returns 0 on success and panics on failure, so
    /// success here means the full amount was used.
    #[private]
    pub fn ft_resolve_transfer(
        &mut self,
        sender_id: AccountId,
        receiver_id: AccountId,
        amount: U128,
    ) -> U128 {
        match env::promise_result_checked(0, 1024) {
            Ok(_) => U128(0),
            Err(_) => {
                // Receiver rejected: move funds back.
                let bal = self.balances.get(&receiver_id).copied().unwrap_or(0);
                let refund = bal.min(amount.0);
                if refund > 0 {
                    self.balances.insert(receiver_id, bal - refund);
                    let sbal = self.balances.get(&sender_id).copied().unwrap_or(0);
                    self.balances.insert(sender_id, sbal + refund);
                }
                U128(amount.0 - refund)
            }
        }
    }

    pub fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        U128(self.balances.get(&account_id).copied().unwrap_or(0))
    }

    pub fn ft_total_supply(&self) -> U128 {
        U128(self.total_supply)
    }

    pub fn ft_metadata(&self) -> String {
        serde_json::json!({"spec":"ft-1.0.0","name":"Mock USD Coin (TEST ONLY)","symbol":"mUSDC","decimals":6}).to_string()
    }

    /// Real NEP-141 tokens require storage registration; the mock auto-
    /// registers everyone, so this is a payable no-op for API compatibility.
    #[payable]
    pub fn storage_deposit(
        &mut self,
        account_id: Option<AccountId>,
        registration_only: Option<bool>,
    ) -> String {
        let _ = (account_id, registration_only);
        serde_json::json!({"total":"0","available":"0"}).to_string()
    }
}
