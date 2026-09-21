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

pub mod mach_surface_share_service;
pub mod state;

pub use mach_surface_share_service::{
    MachSurfaceShareService, MachSurfaceShareServiceRendezvous, SurfaceShareHelperProcessAdmission,
    SurfaceShareHelperProcessAdmissions,
};
pub use state::IOSurfaceShareState;
