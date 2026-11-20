// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::ffi::CStr;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::rhi::{
    ComputeBindingKind, ComputeBindingSpec, ShaderSpecializationConstant, ShaderSpecializationData,
    Texture,
};
use crate::core::{Error, Result};
use crate::vulkan::rhi::kernels::vulkan_pipeline_cache_on_disk::{
    PipelineCacheKernelKind, create_pipeline_cache_handle, persist_pipeline_cache,
    pipeline_cache_file_path_keyed_by, read_cache_blob,
};

fn descriptor_kind_to_vk(kind: ComputeBindingKind) -> vk::DescriptorType {
    match kind {
        ComputeBindingKind::StorageBuffer => vk::DescriptorType::STORAGE_BUFFER,
        ComputeBindingKind::UniformBuffer => vk::DescriptorType::UNIFORM_BUFFER,
        ComputeBindingKind::SampledTexture => vk::DescriptorType::COMBINED_IMAGE_SAMPLER,
        ComputeBindingKind::SampledImage => vk::DescriptorType::SAMPLED_IMAGE,
        ComputeBindingKind::StorageImage => vk::DescriptorType::STORAGE_IMAGE,
    }
}

pub(super) fn create_shader_module(
    device: &vulkanalia::Device,
    spirv: &[u32],
    label: &str,
) -> Result<vk::ShaderModule> {
    let info = vk::ShaderModuleCreateInfo::builder().code(spirv).build();
    unsafe { device.create_shader_module(&info, None) }.map_err(|e| {
        Error::GpuError(format!(
            "Compute kernel '{label}': failed to create shader module: {e}"
        ))
    })
}

pub(super) fn create_descriptor_set_layout(
    device: &vulkanalia::Device,
    bindings: &[ComputeBindingSpec],
    immutable_samplers: &[(u32, vk::Sampler)],
) -> Result<vk::DescriptorSetLayout> {
    // Builder borrows `pImmutableSamplers` from the slice provided to
    // `.immutable_samplers(...)`. Hold each [vk::Sampler; 1] in a stable
    // slot for the duration of the `info.build()` call so the pointers
    // baked into `layout_bindings` are valid through layout creation.
    let immutable_slots: Vec<[vk::Sampler; 1]> = immutable_samplers
        .iter()
        .map(|(_, sampler)| [*sampler])
        .collect();

    let layout_bindings: Vec<vk::DescriptorSetLayoutBinding> = bindings
        .iter()
        .map(|spec| {
            let mut b = vk::DescriptorSetLayoutBinding::builder()
                .binding(spec.binding)
                .descriptor_type(descriptor_kind_to_vk(spec.kind))
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE);
            if let Some(idx) = immutable_samplers
                .iter()
                .position(|(b, _)| *b == spec.binding)
            {
                b = b.immutable_samplers(&immutable_slots[idx]);
            }
            b.build()
        })
        .collect();

    let info = vk::DescriptorSetLayoutCreateInfo::builder()
        .bindings(&layout_bindings)
        .build();
    let layout = unsafe { device.create_descriptor_set_layout(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create descriptor set layout: {e}")))?;
    // Keep `immutable_slots` alive until `create_descriptor_set_layout`
    // has consumed the pointers. (The driver copies the samplers into
    // its internal layout state; the slice can be freed afterward.)
    drop(immutable_slots);
    Ok(layout)
}

pub(super) fn create_pipeline_layout(
    device: &vulkanalia::Device,
    set_layout: vk::DescriptorSetLayout,
    push_constant_size: u32,
) -> Result<vk::PipelineLayout> {
    let set_layouts = [set_layout];
    let push_constant_ranges: Vec<vk::PushConstantRange> = if push_constant_size > 0 {
        vec![
            vk::PushConstantRange::builder()
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
                .offset(0)
                .size(push_constant_size)
                .build(),
        ]
    } else {
        Vec::new()
    };

    let info = vk::PipelineLayoutCreateInfo::builder()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_constant_ranges)
        .build();

    unsafe { device.create_pipeline_layout(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create pipeline layout: {e}")))
}

/// Build a compute pipeline, transparently using an on-disk pipeline cache
/// keyed by the SPIR-V SHA-256.
///
/// The driver validates the cache blob's `VkPipelineCacheHeaderVersionOne`
/// (vendor_id + device_id + cache_uuid) and silently rejects mismatches —
/// fallback-to-recompile is automatic. All cache I/O failures are non-fatal:
/// we warn and fall through to a null cache so kernel construction never
/// fails on a stale, corrupt, or unwritable cache file.
pub(super) fn create_compute_pipeline_with_cache(
    device: &vulkanalia::Device,
    shader_module: vk::ShaderModule,
    pipeline_layout: vk::PipelineLayout,
    spv: &[u8],
    entry_point: &std::ffi::CStr,
    specialization_constants: &[ShaderSpecializationConstant],
    label: &str,
    pipeline_cache_directory_of_its_stream: Option<&Path>,
) -> Result<vk::Pipeline> {
    let specialization_data = ShaderSpecializationData::of_the_constants(specialization_constants);
    let specialization_map_entries = specialization_data
        .map_entries
        .iter()
        .map(|entry| {
            let offset = u32::try_from(entry.offset).map_err(|_| {
                Error::GpuError(format!(
                    "Compute kernel '{label}': specialization constant {} sits past the 4 GiB \
                     a specialization's data can span",
                    entry.constant_id
                ))
            })?;
            Ok(vk::SpecializationMapEntry {
                constant_id: entry.constant_id,
                offset,
                size: entry.size,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let cache_path = pipeline_cache_file_path(
        spv,
        entry_point,
        &specialization_data,
        pipeline_cache_directory_of_its_stream,
    );
    let initial_data = read_cache_blob(PipelineCacheKernelKind::Compute, &cache_path);
    tracing::debug!(
        label,
        cache_path = ?cache_path,
        cache_hit = initial_data.is_some(),
        blob_bytes = initial_data.as_ref().map_or(0, |d| d.len()),
        "compute pipeline cache lookup",
    );

    // `Some(cache_handle)` if we successfully created a `VkPipelineCache` —
    // we need to destroy it before returning regardless of success/failure
    // of the pipeline build.
    let pipeline_cache = create_pipeline_cache_handle(
        PipelineCacheKernelKind::Compute,
        device,
        initial_data.as_deref(),
        label,
    );

    let cache_handle = pipeline_cache.unwrap_or(vk::PipelineCache::null());

    let specialization_info = vk::SpecializationInfo::builder()
        .map_entries(&specialization_map_entries)
        .data(&specialization_data.bytes);
    let mut stage = vk::PipelineShaderStageCreateInfo::builder()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(shader_module)
        .name(entry_point.to_bytes_with_nul());
    if !specialization_map_entries.is_empty() {
        stage = stage.specialization_info(&specialization_info);
    }
    let stage = stage.build();
    let info = vk::ComputePipelineCreateInfo::builder()
        .stage(stage)
        .layout(pipeline_layout)
        .build();

    let pipelines_result = unsafe { device.create_compute_pipelines(cache_handle, &[info], None) };

    // Persist whatever the driver populated, even if one of the pipelines in
    // a hypothetical multi-pipeline batch failed (we only build one here).
    if pipeline_cache.is_some() {
        persist_pipeline_cache(
            PipelineCacheKernelKind::Compute,
            device,
            cache_handle,
            &cache_path,
            label,
        );
        unsafe { device.destroy_pipeline_cache(cache_handle, None) };
    }

    let pipelines = pipelines_result.map_err(|e| {
        Error::GpuError(format!(
            "Compute kernel '{label}': failed to create compute pipeline: {e}"
        ))
    })?;
    Ok(pipelines.0[0])
}

/// The cache file path for a given SPIR-V blob, entry point and
/// specialization.
///
/// File name is the SHA-256 of the SPIR-V bytes, the entry point and the
/// specialization in lowercase hex with a `.bin` suffix. Two SPIR-V blobs that differ by any byte produce
/// distinct file paths; identical blobs hit the same cache file across
/// process restarts.
pub(super) fn pipeline_cache_file_path(
    spv: &[u8],
    entry_point: &CStr,
    specialization_data: &ShaderSpecializationData,
    pipeline_cache_directory_of_its_stream: Option<&Path>,
) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(spv);
    // One module can back several pipelines, one per entry point, and they are
    // not interchangeable. Without this they would share a file and overwrite
    // each other's blob — self-healing, since the driver validates its own
    // header and recompiles on a miss, but a cold recompile every alternate
    // run is not what a cache is for.
    hasher.update(entry_point.to_bytes());
    // Each specialization is a pipeline of its own. An unspecialized kernel
    // hashes only its SPIR-V and entry point.
    for entry in &specialization_data.map_entries {
        hasher.update(entry.constant_id.to_le_bytes());
        hasher.update((entry.offset as u64).to_le_bytes());
        hasher.update((entry.size as u64).to_le_bytes());
    }
    hasher.update(&specialization_data.bytes);
    pipeline_cache_file_path_keyed_by(
        PipelineCacheKernelKind::Compute,
        &format!("{:x}", hasher.finalize()),
        pipeline_cache_directory_of_its_stream,
    )
}

pub(super) fn create_descriptor_pool(
    device: &vulkanalia::Device,
    bindings: &[ComputeBindingSpec],
) -> Result<vk::DescriptorPool> {
    let mut counts: HashMap<vk::DescriptorType, u32> = HashMap::new();
    for spec in bindings {
        *counts.entry(descriptor_kind_to_vk(spec.kind)).or_insert(0) += 1;
    }
    let pool_sizes: Vec<vk::DescriptorPoolSize> = counts
        .into_iter()
        .map(|(ty, count)| {
            vk::DescriptorPoolSize::builder()
                .type_(ty)
                .descriptor_count(count)
                .build()
        })
        .collect();

    let info = vk::DescriptorPoolCreateInfo::builder()
        .max_sets(1)
        .pool_sizes(&pool_sizes)
        .build();
    unsafe { device.create_descriptor_pool(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create descriptor pool: {e}")))
}

pub(super) fn allocate_descriptor_set(
    device: &vulkanalia::Device,
    pool: vk::DescriptorPool,
    layout: vk::DescriptorSetLayout,
) -> Result<vk::DescriptorSet> {
    let set_layouts = [layout];
    let info = vk::DescriptorSetAllocateInfo::builder()
        .descriptor_pool(pool)
        .set_layouts(&set_layouts)
        .build();
    let sets = unsafe { device.allocate_descriptor_sets(&info) }
        .map_err(|e| Error::GpuError(format!("Failed to allocate descriptor set: {e}")))?;
    Ok(sets[0])
}

pub(super) fn create_command_pool(
    device: &vulkanalia::Device,
    queue_family_index: u32,
) -> Result<vk::CommandPool> {
    let info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
        .build();
    unsafe { device.create_command_pool(&info, None) }
        .map_err(|e| Error::GpuError(format!("Failed to create command pool: {e}")))
}

pub(super) fn allocate_command_buffer(
    device: &vulkanalia::Device,
    pool: vk::CommandPool,
) -> Result<vk::CommandBuffer> {
    let info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let buffers = unsafe { device.allocate_command_buffers(&info) }
        .map_err(|e| Error::GpuError(format!("Failed to allocate command buffer: {e}")))?;
    Ok(buffers[0])
}

pub(super) fn vk_image_view_for(texture: &Texture) -> Result<vk::ImageView> {
    use crate::host_rhi::HostTextureExt;
    texture.vulkan_inner().image_view()
}
