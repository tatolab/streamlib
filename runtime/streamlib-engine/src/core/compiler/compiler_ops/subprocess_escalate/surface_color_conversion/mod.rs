// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The colour conversion a helper process asks the engine to run.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod linux_and_macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod neither_linux_nor_macos;
#[cfg(all(test, target_os = "linux"))]
mod tests;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use linux_and_macos::handle_convert_surface_into;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) use neither_linux_nor_macos::handle_convert_surface_into;
