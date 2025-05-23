use keccak_hash::KECCAK_EMPTY;
use mazze_types::{Address, AddressSpaceUtil, AddressWithSpace, Space, U256};
use primitives::{Account, SponsorInfo, StorageLayout};

use super::OverlayAccount;

impl Default for OverlayAccount {
    fn default() -> Self {
        OverlayAccount {
            address: Address::zero().with_native_space(),
            balance: U256::zero(),
            nonce: U256::zero(),
            admin: Address::zero(),
            sponsor_info: Default::default(),
            storage_read_cache: Default::default(),
            storage_write_cache: Default::default(),
            transient_storage: Default::default(),
            storage_layout_change: None,
            // staking_balance: 0.into(),
            collateral_for_storage: 0.into(),
            // accumulated_interest_return: 0.into(),
            code_hash: KECCAK_EMPTY,
            code: None,
            is_newly_created_contract: false,
            pending_db_clear: false,
        }
    }
}

impl OverlayAccount {
    /// Create an OverlayAccount from loaded account.
    pub fn from_loaded(address: &AddressWithSpace, account: Account) -> Self {
        let overlay_account = OverlayAccount {
            address: address.clone(),
            balance: account.balance,
            nonce: account.nonce,
            admin: account.admin,
            sponsor_info: account.sponsor_info,
            // staking_balance: account.staking_balance,
            collateral_for_storage: account.collateral_for_storage,
            // accumulated_interest_return: account.accumulated_interest_return,
            code_hash: account.code_hash,
            ..Default::default()
        };

        overlay_account
    }

    /// Create an OverlayAccount of basic account when the account doesn't exist
    /// before.
    pub fn new_basic(address: &AddressWithSpace, balance: U256) -> Self {
        OverlayAccount {
            address: address.clone(),
            balance,
            ..Default::default()
        }
    }

    /// Create an OverlayAccount of basic account when the account doesn't exist
    /// before.
    pub fn new_removed(address: &AddressWithSpace) -> Self {
        OverlayAccount {
            address: address.clone(),
            pending_db_clear: true,
            ..Default::default()
        }
    }

    /// Create an OverlayAccount of contract account when the account doesn't
    /// exist before.
    pub fn new_contract_with_admin(
        address: &AddressWithSpace, balance: U256, admin: &Address,
        pending_db_clear: bool, storage_layout: Option<StorageLayout>,
    ) -> Self {
        let sponsor_info = if address.space == Space::Native {
            SponsorInfo {
                storage_points: Some(Default::default()),
                ..Default::default()
            }
        } else {
            Default::default()
        };
        OverlayAccount {
            address: address.clone(),
            balance,
            nonce: U256::one(),
            admin: admin.clone(),
            sponsor_info,
            storage_layout_change: storage_layout,
            is_newly_created_contract: true,
            pending_db_clear,
            ..Default::default()
        }
    }

    /// Create an OverlayAccount of contract account when the account doesn't
    /// exist before.
    #[cfg(test)]
    pub fn new_contract(
        address: &Address, balance: U256, pending_db_clear: bool,
        storage_layout: Option<StorageLayout>,
    ) -> Self {
        Self::new_contract_with_admin(
            &address.with_native_space(),
            balance,
            &Address::zero(),
            pending_db_clear,
            storage_layout,
        )
    }

    /// This function replicates the behavior of the auto-derived `Clone`
    /// implementation, but is manually implemented to explicitly invoke the
    /// `clone` method.
    ///
    /// This approach is necessary because a casual clone could lead to
    /// unintended panic: The `OverlayAccount`s in different checkpoint
    /// layers for the same address shares an `Arc` pointer to the same storage
    /// cache object. During commit, it's asserted that each storage cache
    /// object has only one pointer, meaning each address can only have one
    /// copy.
    ///
    /// Thus, this manual implementation ensures that the cloning of an account
    /// is traceable and controlled。
    pub fn clone_account(&self) -> Self {
        OverlayAccount {
            address: self.address,
            balance: self.balance,
            nonce: self.nonce,
            admin: self.admin,
            sponsor_info: self.sponsor_info.clone(),
            // staking_balance: self.staking_balance,
            collateral_for_storage: self.collateral_for_storage,
            // accumulated_interest_return: self.accumulated_interest_return,
            code_hash: self.code_hash,
            code: self.code.clone(),
            is_newly_created_contract: self.is_newly_created_contract,
            pending_db_clear: self.pending_db_clear,
            storage_write_cache: self.storage_write_cache.clone(),
            storage_read_cache: self.storage_read_cache.clone(),
            transient_storage: self.transient_storage.clone(),
            storage_layout_change: self.storage_layout_change.clone(),
        }
    }
}

impl OverlayAccount {
    pub fn as_account(&self) -> Account {
        let mut account = Account::new_empty(self.address());

        account.balance = self.balance;
        account.nonce = self.nonce;
        account.code_hash = self.code_hash;
        // account.staking_balance = self.staking_balance;
        account.collateral_for_storage = self.collateral_for_storage;
        // account.accumulated_interest_return = self.accumulated_interest_return;
        account.admin = self.admin;
        account.sponsor_info = self.sponsor_info.clone();
        account.set_address(self.address);
        account
    }
}
