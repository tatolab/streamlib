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
//! Post-#588 the crate is **ready for the cdylib runtimes** (#589
//! Python, #590 Deno). The pieces in place:
//!
//! - Host-flavor scaffold from #587: [`CudaSurfaceAdapter`],
//!   [`HostSurfaceRegistration`], [`CudaContext`], the registry-of-state
//!   machinery, and the host-side OPAQUE_FD export entry points the
//!   carve-out test in `streamlib-adapter-cuda-helpers` exercises.
//! - OPAQUE_FD plumbing chain from #588: the adapter is generic over
//!   `D: VulkanRhiDevice` and instantiates against either flavor of
//!   device — `HostVulkanDevice` host-side, `ConsumerVulkanDevice`
//!   inside the cdylib once `streamlib-consumer-rhi`'s
//!   `import_opaque_fd_memory` + `ConsumerVulkanPixelBuffer::from_opaque_fd`
//!   land the surface-share-passed FD. Same trait surface, same
//!   acquire/release semantics on either side.
//! - DLPack capsule shape: [`crate::dlpack`] re-exports the v0.8
//!   `#[repr(C)]` ABI mirrors via `dlpark::ffi` and provides
//!   [`crate::dlpack::build_managed_tensor`] /
//!   [`crate::dlpack::build_byte_buffer_managed_tensor`]. Views grow
//!   `dlpack_managed_tensor(device_ptr, device, owner)` accessors
//!   that the cdylib calls after `cudaExternalMemoryGetMappedBuffer`
//!   yields the device pointer. The capsule manager-ctx + deleter
//!   plumbing is owned here (not in `dlpark`'s pyo3-flavored safe
//!   wrappers) so the cdylib can supply any `Arc`-flavored owner via
//!   `Box<dyn Any + Send + 'static>`.
//!
//! What still ships in #589/#590 (out of scope here): the cdylib's
//! `cudarc` integration that pulls the `CUdeviceptr` from
//! `cudaExternalMemoryGetMappedBuffer`, the Python `PyCapsule` /
//! Deno FFI wrapping of the [`dlpack::ManagedTensor`], the
//! `cudaPointerGetAttributes`-driven `kDLCUDA` vs `kDLCUDAHost`
//! decision (#588 Stage 8 ships the assertion; the result calibrates
//! the cdylib default), and the polyglot E2E.

#![cfg(target_os = "linux")]

mod adapter;
mod context;
pub mod dlpack;
mod state;
mod view;

pub use adapter::CudaSurfaceAdapter;
pub use context::CudaContext;
pub use state::HostSurfaceRegistration;
pub use streamlib_consumer_rhi::VulkanLayout;
pub use view::{CudaReadView, CudaWriteView};
