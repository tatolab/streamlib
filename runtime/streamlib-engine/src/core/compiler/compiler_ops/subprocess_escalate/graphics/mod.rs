// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Graphics kernels a helper process registers and draws with.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod not_linux;
#[cfg(all(test, target_os = "linux"))]
mod tests;

#[cfg(target_os = "linux")]
pub(super) use linux::{handle_register_graphics_kernel, handle_run_graphics_draw};
#[cfg(not(target_os = "linux"))]
pub(super) use not_linux::{handle_register_graphics_kernel, handle_run_graphics_draw};
