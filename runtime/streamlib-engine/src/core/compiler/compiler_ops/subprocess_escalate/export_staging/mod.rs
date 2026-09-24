// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The surface-export stagings a helper process opens, refills and copies
//! back: device-local for an external device API, host-visible for the CPU.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod not_linux;
#[cfg(all(test, target_os = "linux"))]
mod tests;

#[cfg(target_os = "linux")]
pub(super) use linux::{
    handle_copy_device_export_staging_back_to_surface, handle_open_cpu_readback_staging,
    handle_open_device_export_staging, handle_refill_device_export_staging,
    handle_run_cpu_readback_copy,
};
#[cfg(not(target_os = "linux"))]
pub(super) use not_linux::{
    handle_copy_device_export_staging_back_to_surface, handle_open_cpu_readback_staging,
    handle_open_device_export_staging, handle_refill_device_export_staging,
    handle_run_cpu_readback_copy,
};
