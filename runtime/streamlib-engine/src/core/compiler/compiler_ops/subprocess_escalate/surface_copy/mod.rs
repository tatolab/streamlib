// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The surface-to-surface copy a helper process asks the engine to run.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod linux_and_macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod neither_linux_nor_macos;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use linux_and_macos::handle_copy_surface_to_surface;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) use neither_linux_nor_macos::handle_copy_surface_to_surface;
