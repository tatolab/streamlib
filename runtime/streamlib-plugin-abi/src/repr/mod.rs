// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wire-protocol `#[repr(C)]` mirrors of the Rust descriptor / helper types
//! crossed by the GPU FFI surface. Built by the cdylib at the FullAccess
//! vtable call site; decoded by the host into the canonical Rust types and
//! dispatched to `GpuContextFullAccess` / `RhiCommandRecorder` /
//! `RhiColorConverter` methods.
//!
//! **Enum discriminant convention.** Variant enums are mirrored as
//! `#[repr(u32)]` so the FFI value is the discriminant only. The
//! payload-carrying enums (`VertexInputState`, `DepthStencilState`,
//! `ColorBlendState`, `RayTracingShaderGroup`) are flattened into
//! `(kind: u32, ...flat payload fields)` structs — every payload field
//! is always present in the wire format, irrelevant fields are zero or
//! `u32::MAX` (the canonical "absent" sentinel for `Option<u32>`
//! stage-index references) and ignored on the host side. This matches
//! the C1 vtable pattern (`acquire_texture` decodes `format_raw: u32`
//! into the appropriate enum on the host) and avoids relying on
//! `#[repr(C, u32)]` data-carrying-enum semantics.
//!
//! **Pointer-shaped slices.** Every `&[T]` in the Rust descriptor is
//! mirrored as `(ptr: *const TRepr, len: usize)`. The pointed-at array
//! must live for the duration of the vtable call; the host
//! `slice::from_raw_parts` lift is bounded by the call. Borrow-shaped
//! reprs match the C1 method-dispatch pattern (`id_ptr` / `id_len`
//! pairs throughout).
//!
//! **`Option<u32>` sentinel.** Ray-tracing shader-group references use
//! `u32::MAX` to encode `None` (no shader index). `u32::MAX` is
//! reserved at the spec level (`VK_SHADER_UNUSED_KHR == ~0u`).

pub mod command_recorder;
pub mod compute;
pub mod graphics;
pub mod opaque_fd;
pub mod present;
pub mod ray_tracing;
pub mod video;

pub use command_recorder::*;
pub use compute::*;
pub use graphics::*;
pub use opaque_fd::*;
pub use present::*;
pub use ray_tracing::*;
pub use video::*;
