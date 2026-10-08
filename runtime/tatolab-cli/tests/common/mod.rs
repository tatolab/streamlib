// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test support for `tatolab`'s integration tests. The unit tests reach the binary-independent
//! files through `#[path]`.

pub mod isolated_node_registry;
pub mod stub_local_api_server;
pub mod tatolab_binary_run;
