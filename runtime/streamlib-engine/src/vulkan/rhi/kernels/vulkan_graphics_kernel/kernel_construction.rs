// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use vulkanalia::prelude::v1_4::*;

use crate::core::rhi::{GraphicsKernelDescriptor, GraphicsShaderStage};
use crate::core::{Error, Result};
use crate::vulkan::rhi::HostVulkanDevice;

use super::{PendingState, VulkanGraphicsKernelInner};

use super::pipeline_creation::{
    allocate_descriptor_sets, create_descriptor_pool, create_descriptor_set_layout,
    create_graphics_pipeline_with_cache, create_pipeline_layout, create_shader_modules,
    destroy_shader_modules,
};
use super::pipeline_state_conversions::shader_stage_flags_to_vk;
use super::spirv_validation::validate_against_spirv;

impl VulkanGraphicsKernelInner {
    /// Create a new graphics kernel from a multi-stage SPIR-V set + binding
    /// declaration + pipeline state.
    ///
    /// Reflects every stage's SPIR-V via `rspirv-reflect`, validates that the
    /// declared `bindings` match the merged shader declaration, and rejects
    /// any mismatch before allocating Vulkan objects.
    pub(crate) fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &GraphicsKernelDescriptor<'_>,
    ) -> Result<Self> {
        Self::new_caching_its_pipeline_in(vulkan_device, descriptor, None)
    }

    /// [`Self::new`], keeping the pipeline's on-disk cache in the stream's
    /// `pipeline_cache_directory_of_its_stream` when one is given.
    pub(crate) fn new_caching_its_pipeline_in(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &GraphicsKernelDescriptor<'_>,
        pipeline_cache_directory_of_its_stream: Option<&Path>,
    ) -> Result<Self> {
        if descriptor.descriptor_sets_in_flight == 0 {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': descriptor_sets_in_flight must be ≥ 1",
                descriptor.label
            )));
        }
        if descriptor.stages.is_empty() {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': no shader stages provided",
                descriptor.label
            )));
        }
        // Today only Vertex + Fragment are supported; reject any other stage
        // up front so the descriptor stays open-shape.
        for stage in descriptor.stages {
            match stage.stage {
                GraphicsShaderStage::Vertex | GraphicsShaderStage::Fragment => {}
            }
        }
        // Require exactly one Vertex stage.
        let vertex_count = descriptor
            .stages
            .iter()
            .filter(|s| s.stage == GraphicsShaderStage::Vertex)
            .count();
        if vertex_count != 1 {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': expected exactly 1 Vertex stage, got {vertex_count}",
                descriptor.label
            )));
        }
        // Fragment is optional in Vulkan but every consumer in tree uses one;
        // require it for the v1 surface to keep the contract simple.
        let fragment_count = descriptor
            .stages
            .iter()
            .filter(|s| s.stage == GraphicsShaderStage::Fragment)
            .count();
        if fragment_count != 1 {
            return Err(Error::GpuError(format!(
                "Graphics kernel '{}': expected exactly 1 Fragment stage, got {fragment_count}",
                descriptor.label
            )));
        }

        let reconciled_bindings =
            validate_against_spirv(descriptor, vulkan_device.subgroup_operation_support())?;

        let device = vulkan_device.device();
        let queue = vulkan_device.queue();
        let queue_family_index = vulkan_device.queue_family_index();

        // ---- Vulkan objects -----------------------------------------------------
        // Strict creation order with staged cleanup on each error edge so we
        // never leak partial state.
        let shader_modules = create_shader_modules(device, descriptor)?;

        let descriptor_set_layout = match create_descriptor_set_layout(device, descriptor.bindings)
        {
            Ok(l) => l,
            Err(e) => {
                destroy_shader_modules(device, &shader_modules);
                return Err(e);
            }
        };

        let pipeline_layout = match create_pipeline_layout(
            device,
            descriptor_set_layout,
            descriptor.push_constants,
        ) {
            Ok(l) => l,
            Err(e) => {
                unsafe { device.destroy_descriptor_set_layout(descriptor_set_layout, None) };
                destroy_shader_modules(device, &shader_modules);
                return Err(e);
            }
        };

        let pipeline = match create_graphics_pipeline_with_cache(
            device,
            &shader_modules,
            descriptor.stages,
            pipeline_layout,
            &descriptor.pipeline_state,
            descriptor.label,
            pipeline_cache_directory_of_its_stream,
        ) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                destroy_shader_modules(device, &shader_modules);
                return Err(e);
            }
        };

        // Shader modules are no longer needed after pipeline creation.
        destroy_shader_modules(device, &shader_modules);

        let descriptor_pool = match create_descriptor_pool(
            device,
            descriptor.bindings,
            descriptor.descriptor_sets_in_flight,
        ) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(e);
            }
        };

        let descriptor_sets = match allocate_descriptor_sets(
            device,
            descriptor_pool,
            descriptor_set_layout,
            descriptor.descriptor_sets_in_flight,
        ) {
            Ok(s) => s,
            Err(e) => {
                unsafe {
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline(pipeline, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(e);
            }
        };

        let pending: Vec<PendingState> = (0..descriptor.descriptor_sets_in_flight)
            .map(|_| PendingState::new())
            .collect();

        Ok(Self {
            label: descriptor.label.to_string(),
            vulkan_device: Arc::clone(vulkan_device),
            device: device.clone(),
            queue,
            queue_family_index,
            bindings: reconciled_bindings,
            push_constant_size: descriptor.push_constants.size,
            push_constant_stages: shader_stage_flags_to_vk(descriptor.push_constants.stages),
            pipeline_state: descriptor.pipeline_state.clone(),
            descriptor_sets_in_flight: descriptor.descriptor_sets_in_flight,
            pipeline,
            pipeline_layout,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_sets,
            pending: Mutex::new(pending),
            default_sampler: Mutex::new(None),
            offscreen: Mutex::new(None),
        })
    }
}
