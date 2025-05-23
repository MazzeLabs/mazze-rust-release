// Copyright 2015-2018 Parity Technologies (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

// Copyright 2024 Mazze Foundation. All rights reserved.
// Mazze is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

//! Evm factory.
use super::{interpreter::SharedCache, vmtype::VMType};
use mazze_types::U256;
#[cfg(test)]
use mazze_vm_types::CallType;
use mazze_vm_types::{ActionParams, Exec, Spec};
use std::sync::Arc;

/// Evm factory. Creates appropriate Evm.
#[derive(Clone)]
pub struct Factory {
    evm: VMType,
    evm_cache: Arc<SharedCache<false>>,
    evm_cache_cancun: Arc<SharedCache<true>>,
}

impl Factory {
    /// Create fresh instance of VM
    /// Might choose implementation depending on supplied gas.
    pub fn create(
        &self, params: ActionParams, spec: &Spec, depth: usize,
    ) -> Box<dyn Exec> {
        use super::interpreter::Interpreter;
        // Assert there is only one type. Parity Ethereum is dead and no more
        // types will be added.
        match self.evm {
            VMType::Interpreter => {}
        };

        match (Self::can_fit_in_usize(&params.gas), spec.cancun_opcodes) {
            (true, true) => Box::new(Interpreter::<usize, true>::new(
                params,
                self.evm_cache_cancun.clone(),
                spec,
                depth,
            )),
            (true, false) => Box::new(Interpreter::<usize, false>::new(
                params,
                self.evm_cache.clone(),
                spec,
                depth,
            )),
            (false, true) => Box::new(Interpreter::<U256, true>::new(
                params,
                self.evm_cache_cancun.clone(),
                spec,
                depth,
            )),
            (false, false) => Box::new(Interpreter::<U256, false>::new(
                params,
                self.evm_cache.clone(),
                spec,
                depth,
            )),
        }
    }

    /// Create new instance of specific `VMType` factory, with a size in bytes
    /// for caching jump destinations.
    pub fn new(evm: VMType, cache_size: usize) -> Self {
        Factory {
            evm,
            evm_cache: Arc::new(SharedCache::new(cache_size)),
            evm_cache_cancun: Arc::new(SharedCache::new(cache_size)),
        }
    }

    fn can_fit_in_usize(gas: &U256) -> bool {
        gas == &U256::from(gas.low_u64() as usize)
    }
}

impl Default for Factory {
    /// Returns native rust evm factory
    fn default() -> Factory {
        Factory {
            evm: VMType::Interpreter,
            evm_cache: Arc::new(SharedCache::default()),
            evm_cache_cancun: Arc::new(SharedCache::default()),
        }
    }
}

#[test]
fn test_create_vm() {
    use mazze_bytes::Bytes;
    use mazze_vm_types::{tests::MockContext, Context};

    let mut params = ActionParams::default();
    params.call_type = CallType::None;
    params.code = Some(Arc::new(Bytes::default()));
    let context = MockContext::new();
    let _vm =
        Factory::default().create(params, context.spec(), context.depth());
}

/// Create tests by injecting different VM factories
#[macro_export]
macro_rules! evm_test(
	($name_test: ident: $name_int: ident) => {
		#[test]
		fn $name_int() {
			$name_test(Factory::new(VMType::Interpreter, 1024 * 32));
		}
	}
);

/// Create ignored tests by injecting different VM factories
#[macro_export]
macro_rules! evm_test_ignore(
	($name_test: ident: $name_int: ident) => {
		#[test]
		#[ignore]
		#[cfg(feature = "ignored-tests")]
		fn $name_int() {
			$name_test(Factory::new(VMType::Interpreter, 1024 * 32));
		}
	}
);
