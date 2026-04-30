// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CUDA surface adapter — imports a host-allocated, OPAQUE_FD-exportable
//! Vulkan resource into a CUDA context via `VK_KHR_external_memory_fd` ↔
//! `cudaImportExternalMemory`, and a Vulkan timeline semaphore via
//! `VK_KHR_external_semaphore_fd` ↔ `cudaImportExternalSemaphore`.
//!
//! Lets subprocess customers (Python / Deno) run GPU-resident AI
//! inference (PyTorch / TensorRT / ONNX-Runtime CUDA backend) on captured
//! frames without the CPU readback round-trip the
//! `streamlib-adapter-cpu-readback` adapter incurs. On-path consumer is
//! the Anduril AI Grand Prix drone-racing pipeline (NVIDIA Jetson Orin /
//! x86 + dGPU).
//!
//! This crate (#587) ships the *host-flavor scaffold* — the
//! `SurfaceAdapter` shape, the registry-of-state machinery, and the
//! host-side OPAQUE_FD export entry points the carve-out test in
//! `streamlib-adapter-cuda-helpers` exercises. Subprocess FFI, DLPack
//! capsule construction, and polyglot E2E ship in dependent issues
//! (#589 Python, #590 Deno).

#![cfg(target_os = "linux")]

mod adapter;
mod context;
mod state;
mod view;

pub use adapter::CudaSurfaceAdapter;
pub use context::CudaContext;
pub use state::HostSurfaceRegistration;
pub use streamlib_consumer_rhi::VulkanLayout;
pub use view::{CudaReadView, CudaWriteView};
