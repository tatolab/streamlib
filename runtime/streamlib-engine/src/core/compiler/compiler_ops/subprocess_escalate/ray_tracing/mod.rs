// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Acceleration structures and ray-tracing kernels a helper process builds and
//! traces.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod not_linux;
#[cfg(all(test, target_os = "linux"))]
mod tests;

#[cfg(target_os = "linux")]
pub(super) use linux::{
    handle_register_acceleration_structure_blas, handle_register_acceleration_structure_tlas,
    handle_register_ray_tracing_kernel, handle_run_ray_tracing_kernel,
};
#[cfg(not(target_os = "linux"))]
pub(super) use not_linux::{
    handle_register_acceleration_structure_blas, handle_register_acceleration_structure_tlas,
    handle_register_ray_tracing_kernel, handle_run_ray_tracing_kernel,
};
