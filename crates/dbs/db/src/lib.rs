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

//! Database-related operations.

#[macro_use]
extern crate log;

mod impls;
mod kv_store;

pub use self::{
    impls::{
        open_database, paritydb_settings, rocksdb_settings, DatabaseBackend,
        DatabaseCompactionProfile, DatabaseSettings, ParityCompression,
        ParityDbOpenConfig, SystemDB,
    },
    kv_store::{DynKeyValueStore, KeyValueStore},
};
