// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public descriptor types for the compute-kernel RHI abstraction.
//!
//! Pattern follows production engines (Granite, Unreal RDG, bgfx): the kernel
//! author declares the binding shape once as data; the RHI reflects the SPIR-V
//! at kernel creation, validates the declaration matches, and from that point
//! on the user binds resources by slot via simple typed setters.

use rspirv_reflect::{DescriptorType as RDescriptorType, Reflection};

use crate::core::{Result, StreamError};

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

/// Derive a binding declaration and push-constant size directly from a SPIR-V
/// blob, with no caller-provided descriptor.
///
/// Used by the escalate-IPC `RegisterComputeKernel` path: a subprocess sends
/// only the SPIR-V (plus push-constant bytes at dispatch time) and the host
/// derives the descriptor shape from reflection alone. Keeps the wire format
/// minimal and the binding-shape source-of-truth in the shader.
///
/// Rejects multi-set kernels — only descriptor set 0 is supported, matching
/// `VulkanComputeKernel`'s contract.
pub fn derive_bindings_from_spirv(
    spv: &[u8],
) -> Result<(Vec<ComputeBindingSpec>, u32)> {
    let reflection = Reflection::new_from_spirv(spv).map_err(|e| {
        StreamError::GpuError(format!("Failed to reflect SPIR-V: {e:?}"))
    })?;

    let sets = reflection.get_descriptor_sets().map_err(|e| {
        StreamError::GpuError(format!(
            "Failed to extract descriptor sets from SPIR-V: {e:?}"
        ))
    })?;

    if sets.len() > 1 {
        return Err(StreamError::GpuError(format!(
            "Only descriptor set 0 is supported; SPIR-V uses sets {:?}",
            sets.keys().collect::<Vec<_>>()
        )));
    }

    let mut bindings: Vec<ComputeBindingSpec> = Vec::new();
    if let Some(set0) = sets.get(&0) {
        let mut entries: Vec<(u32, RDescriptorType)> = set0
            .iter()
            .map(|(b, info)| (*b, info.ty))
            .collect();
        // Stable order — declaration-order convenience for callers.
        entries.sort_by_key(|(b, _)| *b);
        for (binding, ty) in entries {
            let kind = spirv_type_to_kind(ty).ok_or_else(|| {
                StreamError::GpuError(format!(
                    "SPIR-V binding {binding} has unsupported descriptor type {ty:?}"
                ))
            })?;
            bindings.push(ComputeBindingSpec { binding, kind });
        }
    }

    let push_size = reflection
        .get_push_constant_range()
        .map_err(|e| {
            StreamError::GpuError(format!(
                "Failed to read push-constant range from SPIR-V: {e:?}"
            ))
        })?
        .map(|info| info.size)
        .unwrap_or(0);

    Ok((bindings, push_size))
}

fn spirv_type_to_kind(ty: RDescriptorType) -> Option<ComputeBindingKind> {
    match ty {
        RDescriptorType::STORAGE_BUFFER => Some(ComputeBindingKind::StorageBuffer),
        RDescriptorType::UNIFORM_BUFFER => Some(ComputeBindingKind::UniformBuffer),
        RDescriptorType::COMBINED_IMAGE_SAMPLER => {
            Some(ComputeBindingKind::SampledTexture)
        }
        RDescriptorType::STORAGE_IMAGE => Some(ComputeBindingKind::StorageImage),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // SPIR-V test fixtures live next to `vulkan_compute_kernel.rs` and are
    // built by `libs/streamlib/build.rs`. Reflection is a host-architecture
    // operation (no GPU required), so these tests run anywhere `streamlib`
    // builds.
    fn blend_spv(input_count: u32) -> &'static [u8] {
        match input_count {
            1 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_1.spv")),
            2 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_2.spv")),
            4 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_4.spv")),
            8 => include_bytes!(concat!(env!("OUT_DIR"), "/test_blend_8.spv")),
            _ => panic!("test_blend SPIR-V variants are 1/2/4/8 only"),
        }
    }

    #[test]
    fn derives_storage_buffers_for_blend_shader() {
        for &n in &[1u32, 2, 4, 8] {
            let (bindings, push_size) = derive_bindings_from_spirv(blend_spv(n))
                .expect("derive bindings");
            assert_eq!(bindings.len(), n as usize + 1);
            for spec in &bindings {
                assert_eq!(spec.kind, ComputeBindingKind::StorageBuffer);
            }
            // Output sits at binding 8 in every variant.
            assert!(bindings.iter().any(|s| s.binding == 8));
            assert_eq!(push_size, 4);
        }
    }

    #[test]
    fn rejects_truncated_spirv() {
        let err = derive_bindings_from_spirv(&[0u8; 7])
            .err()
            .expect("expected failure");
        let msg = format!("{err}");
        assert!(
            msg.contains("Failed to reflect SPIR-V"),
            "expected reflect error, got: {msg}"
        );
    }
}
