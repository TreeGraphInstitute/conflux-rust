// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

pub mod constants;
pub mod events;
pub mod resources;

pub use constants::*;
pub use events::*;
pub use resources::*;

use move_core_types::account_address::AccountAddress;

pub fn pivot_chain_select_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1D9")
        .expect("Parsing valid hex literal should always succeed")
}

pub fn election_select_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1DA")
        .expect("Parsing valid hex literal should always succeed")
}

pub fn retire_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1DB")
        .expect("Parsing valid hex literal should always succeed")
}

pub fn unlock_address() -> AccountAddress {
    AccountAddress::from_hex_literal("0x1DC")
        .expect("Parsing valid hex literal should always succeed")
}
