// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Per-runtime surface-sharing service (macOS).
//!
//! Each `Runner` owns one [`MachSurfaceShareService`] registered under a
//! dynamic bootstrap name — no launchd plist, no bundle. Helper processes
//! receive the name through `STREAMLIB_SURFACE_MACH_SERVICE` and exchange
//! IOSurface Mach ports with it. The name is discoverable in the user's
//! launchd domain, so the service admits only this process and the helper
//! processes it spawned, by the audit token the kernel appends to every
//! message.

pub mod cross_process_timeline_pair;
mod gpu_pool_claimant_process;
pub mod mach_surface_share_service;
pub mod state;
mod viewer_process_admission;

pub use cross_process_timeline_pair::{
    CROSS_PROCESS_TIMELINE_WAIT_BOUND, ConsumerReleaseOutcome, CrossProcessTimelinePair,
    CrossProcessTimelinePairsBySurface,
};
pub use mach_surface_share_service::{
    MachSurfaceShareService, MachSurfaceShareServiceRendezvous, SurfaceShareHelperProcessAdmission,
};
pub use state::IOSurfaceShareState;
pub use viewer_process_admission::admit_a_viewer_process;
