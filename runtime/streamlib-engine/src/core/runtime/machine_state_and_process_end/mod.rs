// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the runtime records about the machine and how the process ends: state directory, crashes, shutdown and teardown.

pub(super) mod end_the_process_at_once;
pub(super) mod engine_teardown_watchdog;
pub(super) mod machine_name;
pub(super) mod machine_state_directory;
pub(super) mod runtime_crash_pinned_on_a_stream;
pub(super) mod runtime_shutdown_request;
pub(super) mod runtime_unique_id;
pub(super) mod status;
