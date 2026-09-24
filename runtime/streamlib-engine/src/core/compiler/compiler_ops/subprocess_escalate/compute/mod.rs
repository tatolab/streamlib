// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Compute kernels a helper process registers and dispatches.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod linux_and_macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod neither_linux_nor_macos;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use linux_and_macos::{
    handle_register_compute_kernel, handle_run_compute_kernel, handle_run_compute_kernel_batch,
};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) use neither_linux_nor_macos::{
    handle_register_compute_kernel, handle_run_compute_kernel, handle_run_compute_kernel_batch,
};
