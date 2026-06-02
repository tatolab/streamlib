// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public descriptor types for the compute-kernel RHI abstraction.
//!
//! Pure-data twins of the engine's `core::rhi` descriptor shapes (the
//! engine's `compute_kernel.rs` additionally hosts the
//! `rspirv-reflect`-driven `derive_bindings_from_spirv` helper, which is
//! host-only; the SDK carries only the byte-shaped declaration types a
//! plugin hands to `create_compute_kernel`). The kernel author declares
//! the binding shape once as data; the host reflects the SPIR-V at kernel
//! creation and validates the declaration matches.

/// Kind of resource bound at a particular slot in a compute kernel's
/// descriptor set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeBindingKind {
    /// Storage buffer (SSBO) — read/write, arbitrary byte access.
    StorageBuffer,
    /// Uniform buffer (UBO) — read-only, fixed-size, fast-path.
    UniformBuffer,
    /// Sampled image with a combined sampler — read-only with filtering.
    SampledTexture,
    /// Sampled image without a combined sampler — read-only, addressed
    /// by integer coordinates via GLSL `texelFetch`.
    SampledImage,
    /// Storage image — read/write, no filtering, exact pixel access.
    StorageImage,
}

/// One binding declaration: (binding index, resource kind).
///
/// Set index is implicitly 0 — multi-set kernels are not supported today.
#[derive(Debug, Clone, Copy)]
pub struct ComputeBindingSpec {
    pub binding: u32,
    pub kind: ComputeBindingKind,
}

impl ComputeBindingSpec {
    pub const fn storage_buffer(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::StorageBuffer,
        }
    }

    pub const fn uniform_buffer(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::UniformBuffer,
        }
    }

    pub const fn sampled_texture(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::SampledTexture,
        }
    }

    pub const fn sampled_image(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::SampledImage,
        }
    }

    pub const fn storage_image(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::StorageImage,
        }
    }
}

/// Compute-kernel descriptor: SPIR-V bytecode + binding layout +
/// push-constant size.
///
/// Pass to [`crate::context::GpuContextFullAccess::create_compute_kernel`].
/// The host reflects the SPIR-V on creation, validates that `bindings`
/// matches the shader's declared descriptor set, and rejects mismatches
/// loudly.
#[derive(Debug, Clone)]
pub struct ComputeKernelDescriptor<'a> {
    /// Human-readable label used in error messages and tracing.
    pub label: &'a str,
    /// Compiled SPIR-V bytecode for the compute shader.
    pub spv: &'a [u8],
    /// Binding declarations for descriptor set 0.
    pub bindings: &'a [ComputeBindingSpec],
    /// Push-constant range size in bytes; 0 if the shader uses none.
    pub push_constant_size: u32,
}

// =============================================================================
// Plugin-ABI repr conversions
// =============================================================================

use streamlib_plugin_abi::{ComputeBindingKindRepr, ComputeBindingSpecRepr, ComputeKernelDescriptorRepr};

impl From<ComputeBindingKind> for ComputeBindingKindRepr {
    fn from(value: ComputeBindingKind) -> Self {
        match value {
            ComputeBindingKind::StorageBuffer => Self::StorageBuffer,
            ComputeBindingKind::UniformBuffer => Self::UniformBuffer,
            ComputeBindingKind::SampledTexture => Self::SampledTexture,
            ComputeBindingKind::StorageImage => Self::StorageImage,
            ComputeBindingKind::SampledImage => Self::SampledImage,
        }
    }
}

impl From<&ComputeBindingSpec> for ComputeBindingSpecRepr {
    fn from(value: &ComputeBindingSpec) -> Self {
        Self {
            binding: value.binding,
            kind: ComputeBindingKindRepr::from(value.kind) as u32,
        }
    }
}

/// Stage a [`ComputeKernelDescriptor`] to its `#[repr(C)]` mirror plus a
/// backing buffer of repr bindings, ready for the FullAccess
/// `create_compute_kernel` vtable call.
///
/// Returns `(repr, bindings_buf)`. The caller MUST keep `bindings_buf`
/// alive for the lifetime of `repr` (the repr's `bindings_ptr` points
/// into `bindings_buf`).
pub(crate) fn stage_compute_kernel_descriptor(
    desc: &ComputeKernelDescriptor<'_>,
) -> (ComputeKernelDescriptorRepr, Vec<ComputeBindingSpecRepr>) {
    let bindings_buf: Vec<ComputeBindingSpecRepr> =
        desc.bindings.iter().map(ComputeBindingSpecRepr::from).collect();
    let repr = ComputeKernelDescriptorRepr {
        label_ptr: desc.label.as_ptr(),
        label_len: desc.label.len(),
        spv_ptr: desc.spv.as_ptr(),
        spv_len: desc.spv.len(),
        bindings_ptr: bindings_buf.as_ptr(),
        bindings_len: bindings_buf.len(),
        push_constant_size: desc.push_constant_size,
        _reserved_padding: 0,
    };
    (repr, bindings_buf)
}
