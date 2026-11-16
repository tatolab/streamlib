// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Test support for `tatolab`'s integration tests. The unit tests mount every file here but
//! `tatolab_binary_run`, `isolated_machine_directories` and `running_tatolab` through `#[path]`, as
//! siblings, so each reaches the others as `super::`.

pub(crate) mod isolated_machine_directories;
pub(crate) mod running_tatolab;
pub(crate) mod runtime_log_line_fixtures;
pub(crate) mod stub_local_api_server;
pub(crate) mod tatolab_binary_run;
