// Copyright 2021 Conflux Foundation. All rights reserved.
// Conflux is free software and distributed under GNU General Public License.
// See http://www.gnu.org/licenses/

#[cfg(feature = "new_state_impl")]
pub(self) mod cache_object;
#[cfg(feature = "new_state_impl")]
pub mod state;
#[cfg(feature = "new_state_impl")]
pub(self) mod state_object_cache;
#[cfg(feature = "new_state_impl")]
pub mod tracer;

/// Mode of dealing with null accounts.
#[derive(PartialEq)]
pub enum CleanupMode<'a> {
    /// Create accounts which would be null.
    ForceCreate,
    /// Don't delete null accounts upon touching, but also don't create them.
    NoEmpty,
    /// Mark all touched accounts.
    /// TODO: We have not implemented the correct behavior of TrackTouched for
    /// internal Contracts.
    TrackTouched(&'a mut HashSet<AddressWithSpace>),
}

pub fn maybe_address(address: &Address) -> Option<Address> {
    if address.is_zero() {
        None
    } else {
        Some(*address)
    }
}

// TODO: Deprecate the StateDbExt in StateDb and replace it with StateDbOps.
#[cfg(feature = "new_state_impl")]
pub trait StateDbOps {
    fn get_raw(&self, key: StorageKeyWithSpace) -> Result<Option<Arc<[u8]>>>;

    fn get<T>(&self, key: StorageKeyWithSpace) -> Result<Option<T>>
    where T: ::rlp::Decodable;

    fn set<T>(
        &mut self, key: StorageKeyWithSpace, value: &T,
        debug_record: Option<&mut ComputeEpochDebugRecord>,
    ) -> Result<()>
    where
        T: ::rlp::Encodable + IsDefault;

    fn delete(
        &mut self, key: StorageKeyWithSpace,
        debug_record: Option<&mut ComputeEpochDebugRecord>,
    ) -> Result<()>;
}
#[cfg(feature = "new_state_impl")]
impl StateDbOps for StateDbGeneric {
    fn get_raw(&self, key: StorageKeyWithSpace) -> Result<Option<Arc<[u8]>>> {
        Self::get_raw(self, key)
    }

    fn get<T>(&self, key: StorageKeyWithSpace) -> Result<Option<T>>
    where T: ::rlp::Decodable {
        <Self as StateDbExt>::get(self, key)
    }

    fn set<T>(
        &mut self, key: StorageKeyWithSpace, value: &T,
        debug_record: Option<&mut ComputeEpochDebugRecord>,
    ) -> Result<()>
    where
        T: ::rlp::Encodable + IsDefault,
    {
        <Self as StateDbExt>::set(self, key, value, debug_record)
    }

    fn delete(
        &mut self, key: StorageKeyWithSpace,
        debug_record: Option<&mut ComputeEpochDebugRecord>,
    ) -> Result<()>
    {
        Self::delete(self, key, debug_record)
    }
}
#[cfg(feature = "new_state_impl")]
use cfx_internal_common::debug::ComputeEpochDebugRecord;
#[cfg(feature = "new_state_impl")]
use cfx_statedb::{Result, StateDbExt, StateDbGeneric};
use cfx_types::{Address, AddressWithSpace};
#[cfg(feature = "new_state_impl")]
use primitives::{is_default::IsDefault, StorageKeyWithSpace};
use std::collections::HashSet;
#[cfg(feature = "new_state_impl")]
use std::sync::Arc;
