// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Acceleration structures and ray-tracing kernels a helper process builds and
//! traces.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod linux_and_macos;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod neither_linux_nor_macos;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use linux_and_macos::{
    handle_register_acceleration_structure_blas, handle_register_acceleration_structure_tlas,
    handle_register_ray_tracing_kernel, handle_run_ray_tracing_kernel,
};
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) use neither_linux_nor_macos::{
    handle_register_acceleration_structure_blas, handle_register_acceleration_structure_tlas,
    handle_register_ray_tracing_kernel, handle_run_ray_tracing_kernel,
};
