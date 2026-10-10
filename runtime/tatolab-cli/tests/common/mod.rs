// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Test support for `tatolab`'s integration tests. The unit tests mount every file here but
//! `tatolab_binary_run`, `isolated_machine_directories` and `running_tatolab` through `#[path]`, as
//! siblings, so each reaches the others as `super::`.

pub mod isolated_machine_directories;
pub mod running_tatolab;
pub mod runtime_log_line_fixtures;
pub mod stub_local_api_server;
pub mod tapped_channel_bag_fixtures;
pub mod tatolab_binary_run;
