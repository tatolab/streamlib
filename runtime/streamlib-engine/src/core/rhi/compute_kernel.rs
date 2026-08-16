// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public descriptor types for the compute-kernel RHI abstraction.
//!
//! Pattern follows production engines (Granite, Unreal RDG, bgfx): the kernel
//! author declares the binding shape once as data; the RHI reflects the SPIR-V
//! at kernel creation, validates the declaration matches, and from that point
//! on the user binds resources by slot via simple typed setters.

use std::borrow::Cow;

use rspirv_reflect::{DescriptorType as RDescriptorType, Reflection};

use crate::core::{Error, Result};

/// Kind of resource bound at a particular slot in a compute kernel's
/// descriptor set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeBindingKind {
    /// Storage buffer (SSBO) — read/write, arbitrary byte access.
    StorageBuffer,
    /// Uniform buffer (UBO) — read-only, fixed-size, fast-path.
    UniformBuffer,
    /// Sampled image with a combined sampler — read-only with filtering /
    /// addressing baked into the descriptor. GLSL `sampler2D` /
    /// `samplerExternalOES` style.
    SampledTexture,
    /// Sampled image without a combined sampler — read-only, addressed by
    /// integer coordinates via GLSL `texture2D` + `texelFetch` (no
    /// filtering). Backs Vulkan `VK_DESCRIPTOR_TYPE_SAMPLED_IMAGE`.
    SampledImage,
    /// Storage image — read/write, no filtering, exact pixel access.
    StorageImage,
}

/// One binding declaration: (binding index, resource kind, the shader's own
/// name for the binding).
///
/// Set index is implicitly 0 — multi-set kernels are not supported today.
///
/// `name` is `None` on a declaration that asserts only slot and kind, and
/// `Some` on every spec the RHI has reconciled against the shader: the
/// numeric binding is what the descriptor set is built from, the name is
/// what a by-name dispatch resolves against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputeBindingSpec {
    pub binding: u32,
    pub kind: ComputeBindingKind,
    pub name: Option<Cow<'static, str>>,
}

impl ComputeBindingSpec {
    pub const fn storage_buffer(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::StorageBuffer,
            name: None,
        }
    }

    pub const fn uniform_buffer(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::UniformBuffer,
            name: None,
        }
    }

    pub const fn sampled_texture(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::SampledTexture,
            name: None,
        }
    }

    pub const fn sampled_image(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::SampledImage,
            name: None,
        }
    }

    pub const fn storage_image(binding: u32) -> Self {
        Self {
            binding,
            kind: ComputeBindingKind::StorageImage,
            name: None,
        }
    }

    /// Assert the shader's spelling of this binding as well as its slot.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<Cow<'static, str>>) -> Self {
        self.name = Some(name.into());
        self
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
///
/// Every derived spec carries the shader's own name for its binding. A blob
/// whose `OpName` decorations were stripped is rejected here rather than at
/// dispatch: bindings are resolved by name in one spelling for both
/// languages, so an unnamed binding cannot be bound at all.
pub fn derive_bindings_from_spirv(spv: &[u8]) -> Result<(Vec<ComputeBindingSpec>, u32)> {
    let reflection = Reflection::new_from_spirv(spv)
        .map_err(|e| Error::GpuError(format!("Failed to reflect SPIR-V: {e:?}")))?;

    let sets = reflection.get_descriptor_sets().map_err(|e| {
        Error::GpuError(format!(
            "Failed to extract descriptor sets from SPIR-V: {e:?}"
        ))
    })?;

    if sets.len() > 1 {
        return Err(Error::GpuError(format!(
            "Only descriptor set 0 is supported; SPIR-V uses sets {:?}",
            sets.keys().collect::<Vec<_>>()
        )));
    }

    let mut bindings: Vec<ComputeBindingSpec> = Vec::new();
    if let Some(set0) = sets.get(&0) {
        let mut entries: Vec<(u32, RDescriptorType, String)> = set0
            .iter()
            .map(|(b, info)| (*b, info.ty, info.name.clone()))
            .collect();
        // Stable order — declaration-order convenience for callers.
        entries.sort_by_key(|(b, _, _)| *b);
        for (binding, ty, name) in entries {
            let kind = spirv_type_to_kind(ty).ok_or_else(|| {
                Error::GpuError(format!(
                    "SPIR-V binding {binding} has unsupported descriptor type {ty:?}"
                ))
            })?;
            if name.is_empty() {
                return Err(Error::GpuError(format!(
                    "SPIR-V binding {binding} ({kind:?}) carries no name — its OpName \
                     decorations were stripped, and bindings are resolved by name. Compile \
                     with debug info retained (`glslc -g`) so the shader's own binding names \
                     survive optimization"
                )));
            }
            bindings.push(ComputeBindingSpec {
                binding,
                kind,
                name: Some(Cow::Owned(name)),
            });
        }
    }

    let push_size = reflection
        .get_push_constant_range()
        .map_err(|e| {
            Error::GpuError(format!(
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
        RDescriptorType::COMBINED_IMAGE_SAMPLER => Some(ComputeBindingKind::SampledTexture),
        RDescriptorType::SAMPLED_IMAGE => Some(ComputeBindingKind::SampledImage),
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
            let (bindings, push_size) =
                derive_bindings_from_spirv(blend_spv(n)).expect("derive bindings");
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
    fn reflection_keeps_the_shaders_own_name_for_every_binding() {
        let (bindings, _) = derive_bindings_from_spirv(blend_spv(2)).expect("derive bindings");
        let named: Vec<(u32, &str)> = bindings
            .iter()
            .map(|spec| {
                (
                    spec.binding,
                    spec.name.as_deref().expect("every binding is named"),
                )
            })
            .collect();
        // The GLSL variable names, not the block type names — `in0` from
        // `readonly buffer In0 { … } in0;`. This is the spelling a dispatch
        // binds against, so it is a contract, not an incidental.
        assert_eq!(named, vec![(0, "in0"), (1, "in1"), (8, "out_buf")]);
    }

    #[test]
    fn reflection_keeps_image_binding_names() {
        let spv: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/test_sampled_image.spv"));
        let (bindings, _) = derive_bindings_from_spirv(spv).expect("derive bindings");
        let named: Vec<(&str, ComputeBindingKind)> = bindings
            .iter()
            .map(|spec| (spec.name.as_deref().expect("named"), spec.kind))
            .collect();
        assert_eq!(
            named,
            vec![
                ("uImage", ComputeBindingKind::SampledImage),
                ("uOut", ComputeBindingKind::StorageImage),
            ]
        );
    }

    #[test]
    fn name_stripped_spirv_is_refused_at_construction() {
        // `glslc -O` without `-g` strips every OpName. Such a blob can reach
        // the engine only through the pre-compiled-SPIR-V escape hatch, and
        // it cannot be bound by name at all — so it fails here, at
        // construction, rather than confusingly at first dispatch.
        let stripped = strip_debug_names(blend_spv(2));
        let err = derive_bindings_from_spirv(&stripped)
            .err()
            .expect("a name-stripped blob must be refused");
        let msg = format!("{err}");
        assert!(
            msg.contains("carries no name") && msg.contains("glslc -g"),
            "the refusal must name the cause and the fix, got: {msg}"
        );
    }

    /// Drop every `OpName` (opcode 5) from a SPIR-V module, reproducing what
    /// `glslc -O` emits without `-g`.
    fn strip_debug_names(spv: &[u8]) -> Vec<u8> {
        const HEADER_WORDS: usize = 5;
        const OP_NAME: u16 = 5;
        let words: Vec<u32> = spv
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let mut kept: Vec<u32> = words[..HEADER_WORDS].to_vec();
        let mut at = HEADER_WORDS;
        while at < words.len() {
            let word_count = (words[at] >> 16) as usize;
            let opcode = (words[at] & 0xffff) as u16;
            assert!(word_count > 0, "malformed SPIR-V instruction");
            if opcode != OP_NAME {
                kept.extend_from_slice(&words[at..at + word_count]);
            }
            at += word_count;
        }
        kept.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn sampled_image_spec_round_trips_kind() {
        let spec = ComputeBindingSpec::sampled_image(7);
        assert_eq!(spec.binding, 7);
        assert_eq!(spec.kind, ComputeBindingKind::SampledImage);
        // Lock the kind→SPIR-V descriptor type mapping: SAMPLED_IMAGE
        // must reflect back to `ComputeBindingKind::SampledImage` and
        // must NOT be conflated with COMBINED_IMAGE_SAMPLER / SampledTexture.
        assert_eq!(
            spirv_type_to_kind(RDescriptorType::SAMPLED_IMAGE),
            Some(ComputeBindingKind::SampledImage)
        );
        assert_ne!(
            spirv_type_to_kind(RDescriptorType::COMBINED_IMAGE_SAMPLER),
            Some(ComputeBindingKind::SampledImage),
        );
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
