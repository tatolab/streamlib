// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public test helpers shared by every adapter (in-tree and 3rd-party).
//!
//! - [`run_conformance`] runs the parameterized fixture against any
//!   adapter and panics on the first violation.
//! - [`MockAdapter`] is the reference implementation used by both this
//!   crate's tests and 3rd-party adapter authors who want to copy it.
//! - [`SubprocessCrashHarness`] (Linux-only) spawns a polyglot
//!   subprocess that holds an FD-bound surface, runs a closure, then
//!   SIGKILLs the subprocess at a configurable point so adapters can
//!   verify host-side cleanup against the surface-share watchdog.

pub use crate::conformance::{
    empty_surface, run_conformance, ConformanceSurfaceFactory,
};
pub use crate::mock::{AdapterCountersSnapshot, MockAdapter, MockView};

#[cfg(target_os = "linux")]
pub use crate::subprocess_crash::{
    CrashTiming, SubprocessCrashHarness, SubprocessCrashOutcome,
};
