// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
pub mod mach_surface_share_service;
pub mod state;

pub use cross_process_timeline_pair::{
    CROSS_PROCESS_TIMELINE_WAIT_BOUND, ConsumerReleaseOutcome, CrossProcessTimelinePair,
    CrossProcessTimelinePairsBySurface,
};
pub use mach_surface_share_service::{
    MachSurfaceShareService, MachSurfaceShareServiceRendezvous, SurfaceShareHelperProcessAdmission,
};
pub use state::{IOSurfaceShareRegistration, IOSurfaceShareState, SharedTimelineSendRights};
