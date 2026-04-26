// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public descriptor types for the compute-kernel RHI abstraction.
//!
//! Pattern follows production engines (Granite, Unreal RDG, bgfx): the kernel
//! author declares the binding shape once as data; the RHI reflects the SPIR-V
//! at kernel creation, validates the declaration matches, and from that point
//! on the user binds resources by slot via simple typed setters.

/// Kind of resource bound at a particular slot in a compute kernel's
/// descriptor set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeBindingKind {
    /// Storage buffer (SSBO) — read/write, arbitrary byte access.
    StorageBuffer,
    /// Uniform buffer (UBO) — read-only, fixed-size, fast-path.
    UniformBuffer,
    /// Sampled image — read-only with a sampler (filtering, addressing).
    SampledTexture,
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

    pub const fn storage_image(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::StorageImage,
        }
    }
}

/// Compute-kernel descriptor: SPIR-V bytecode + binding layout + push-constant size.
///
/// Pass to `GpuContext::create_compute_kernel` (or `VulkanComputeKernel::new`).
/// The RHI reflects the SPIR-V on creation, validates that `bindings` matches
/// the shader's declared descriptor set, and rejects mismatches loudly.
#[derive(Debug, Clone)]
pub struct ComputeKernelDescriptor<'a> {
    /// Human-readable label used in error messages and tracing.
    pub label: &'a str,
    /// Compiled SPIR-V bytecode for the compute shader.
    pub spv: &'a [u8],
    /// Binding declarations for descriptor set 0.
    pub bindings: &'a [ComputeBindingSpec],
    /// Push-constant range size in bytes; 0 if the shader uses no push constants.
    pub push_constant_size: u32,
}
