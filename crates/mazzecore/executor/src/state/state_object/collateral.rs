use super::{State, Substate};
use crate::{
    executive_observer::TracerTrait, internal_contract::storage_point_prop,
    return_if, try_loaded,
};
use mazze_parameters::{
    consensus_internal::CIP107_STORAGE_POINT_PROP_INIT,
    staking::MAZZIES_PER_STORAGE_COLLATERAL_UNIT,
};
use mazze_statedb::Result as DbResult;
use mazze_types::{Address, AddressSpaceUtil, U256};
use mazze_vm_types::{self as vm};

impl State {
    pub fn collateral_for_storage(&self, address: &Address) -> DbResult<U256> {
        let acc = try_loaded!(self.read_native_account_lock(address));
        Ok(acc.collateral_for_storage())
    }

    pub fn token_collateral_for_storage(
        &self, address: &Address,
    ) -> DbResult<U256> {
        let acc = try_loaded!(self.read_native_account_lock(address));
        Ok(acc.token_collateral_for_storage())
    }

    pub fn available_storage_points_for_collateral(
        &self, address: &Address,
    ) -> DbResult<U256> {
        let acc = try_loaded!(self.read_native_account_lock(address));
        Ok(acc
            .sponsor_info()
            .storage_points
            .as_ref()
            .map(|points| points.unused)
            .unwrap_or_default())
    }

    pub fn check_storage_limit(
        &self, original_sender: &Address, storage_limit: &U256, dry_run: bool,
    ) -> DbResult<CollateralCheckResult> {
        let collateral_for_storage =
            self.collateral_for_storage(original_sender)?;
        Ok(if collateral_for_storage > *storage_limit && !dry_run {
            Err(CollateralCheckError::ExceedStorageLimit {
                limit: *storage_limit,
                required: collateral_for_storage,
            })
        } else {
            Ok(())
        })
    }

    pub fn storage_point_prop(&self) -> DbResult<U256> {
        self.get_system_storage(&storage_point_prop())
    }

    fn initialize_cip107(
        &mut self, address: &Address,
    ) -> DbResult<(U256, U256)> {
        debug!("Check initialize CIP-107");

        let prop: U256 = self.storage_point_prop()?;
        let mut account =
            self.write_account_or_new_lock(&address.with_native_space())?;
        return_if!(!account.is_contract());
        return_if!(account.is_cip_107_initialized());

        let (from_balance, from_collateral) = account.initialize_cip107(prop);
        std::mem::drop(account);

        Ok((from_balance, from_collateral))
    }
}

/// Initialize CIP-107 for the whole system.
pub fn initialize_cip107(state: &mut State) -> DbResult<()> {
    debug!(
        "set storage_point_prop to {}",
        CIP107_STORAGE_POINT_PROP_INIT
    );
    state.set_system_storage(
        storage_point_prop().to_vec(),
        CIP107_STORAGE_POINT_PROP_INIT.into(),
    )
}

pub type CollateralCheckResult = std::result::Result<(), CollateralCheckError>;

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum CollateralCheckError {
    ExceedStorageLimit { limit: U256, required: U256 },
    NotEnoughBalance { required: U256, got: U256 },
}

impl CollateralCheckError {
    pub fn into_vm_error(self) -> vm::Error {
        match self {
            CollateralCheckError::ExceedStorageLimit { .. } => {
                vm::Error::ExceedStorageLimit
            }
            CollateralCheckError::NotEnoughBalance { required, got } => {
                vm::Error::NotEnoughBalanceForStorage { required, got }
            }
        }
    }
}
