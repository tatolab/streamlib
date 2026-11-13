// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native evaluator of a shader node island: each kernel the split rule
//! cuts the island into, compiled from its members' Slang and run in this
//! process in one submission on each frame the island's leading Image input
//! takes, writing one Image frame from the evaluating node's own output pool.

use std::collections::HashMap;
use std::sync::Arc;

use super::shader_node_island::{
    ShaderNodeIslandHostedHereComponent, pin_value_of_a_shader_node_input,
};
use super::shader_node_island_program::{
    ShaderNodeIslandKernel, ShaderNodeIslandMemberInput, ShaderNodeIslandProgram,
    ShaderNodeKernelImageArgument,
};
use super::shader_node_program::{
    SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE, SHADER_NODE_OUTPUT_TEXTURE_FORMAT,
    ShaderNodeImageInputBacking, ShaderNodeImageInputExtent, ShaderNodeProgram,
    a_shader_node_takes_an_image_of,
};
use super::subprocess_escalate::publish_the_layouts_a_dispatch_left_its_bound_textures_in;
use crate::core::context::surface_backing_resolution::ResolvedSurfaceBacking;
use crate::core::context::{
    BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding, GpuContext,
    NativeNodeOutputPixelBufferFrame, ProcessorOutputPixelBufferPoolOfOneNativeNode,
    RuntimeContextFullAccess, RuntimeContextLimitedAccess, SurfaceBoundKernelBindingResource,
    SurfaceClaimedAgainstProducerReuse, TextureRegistration,
};
use crate::core::descriptors::ProcessorDescriptor;
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::{GraphNodeWithComponents, ProcessorNode, ShaderNodeFusedGroupOfAnIsland};
use crate::core::processors::DynGeneratedProcessor;
use crate::core::rhi::{ShaderCompilationTargetStage, StorageBuffer, TextureFormat};
use crate::iceoryx2::{
    InputMailboxes, InputMailboxesInner, MessageGroupBoundary, OutputWriter, OutputWriterInner,
};
use streamlib_ipc_types::{
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, ImageFrameDescriptorBody,
};

/// The newest Image frame one input port of the evaluator has taken.
#[derive(Clone, Copy)]
struct ImageFrameTakenAtAnInputPort {
    image: ImageFrameDescriptorBody,
    timestamp_ns: i64,
    group_boundary: MessageGroupBoundary,
}

/// One Image input resolved for an evaluation: what it is bound as, and the
/// claim that keeps its frame from its producer's reuse until the dispatch has
/// run.
struct ImageInputResolvedForOneEvaluation {
    backing: ShaderNodeImageInputBacking,
    extent: ShaderNodeImageInputExtent,
    surface_id: String,
    surface_bound_resource: SurfaceBoundKernelBindingResource,
    _claim_against_producer_reuse: SurfaceClaimedAgainstProducerReuse,
}

/// The processor a shader node island runs as in this process, on the node
/// whose Image leaves the island.
pub(crate) struct ShaderNodeNativeEvaluatorProcessor {
    node_name: String,
    descriptor: ProcessorDescriptor,
    island: ShaderNodeIslandProgram,
    fused_group: Option<Arc<ShaderNodeFusedGroupOfAnIsland>>,
    output_writer: Option<OutputWriter>,
    input_mailboxes: Option<InputMailboxes>,
    output_pixel_buffer_pool: Option<ProcessorOutputPixelBufferPoolOfOneNativeNode>,
    stored_pixel_buffer_pool_by_member:
        HashMap<usize, ProcessorOutputPixelBufferPoolOfOneNativeNode>,
    kernels_by_slang_source: HashMap<String, Arc<crate::vulkan::rhi::VulkanComputeKernel>>,
    newest_image_frame_by_input_port: HashMap<String, ImageFrameTakenAtAnInputPort>,
    value_by_float_input_port: HashMap<String, f32>,
    refusal_last_logged: Option<String>,
}

impl ShaderNodeNativeEvaluatorProcessor {
    /// The evaluator of the island shader node `node` hosts, or of `node`
    /// alone running `program` when it hosts none; each float input starts at
    /// its pin value, else its declared default, else zero.
    pub(crate) fn for_the_node(
        node: &ProcessorNode,
        descriptor: ProcessorDescriptor,
        program: ShaderNodeProgram,
    ) -> Self {
        let (island, fused_group) = match node.get::<ShaderNodeIslandHostedHereComponent>() {
            Some(hosted) => (hosted.island.clone(), Some(Arc::clone(&hosted.fused_group))),
            None => (
                ShaderNodeIslandProgram::of_one_node(node.display_name.clone(), program, |port| {
                    pin_value_of_a_shader_node_input(node, port)
                }),
                None,
            ),
        };
        let value_by_float_input_port = island
            .members()
            .iter()
            .flat_map(|member| &member.inputs)
            .filter_map(|input| match input {
                ShaderNodeIslandMemberInput::FloatReadAt {
                    input_port,
                    value_until_one_is_read,
                } => Some((input_port.clone(), *value_until_one_is_read)),
                _ => None,
            })
            .collect();
        Self {
            node_name: node.id.to_string(),
            descriptor,
            island,
            fused_group,
            output_writer: None,
            input_mailboxes: None,
            output_pixel_buffer_pool: None,
            stored_pixel_buffer_pool_by_member: HashMap::new(),
            kernels_by_slang_source: HashMap::new(),
            newest_image_frame_by_input_port: HashMap::new(),
            value_by_float_input_port,
            refusal_last_logged: None,
        }
    }

    /// Each Image input port the island reads, with the member and its own
    /// port reading it.
    fn image_input_ports_read(&self) -> Vec<(String, String, String)> {
        self.island
            .members()
            .iter()
            .flat_map(|member| {
                member.inputs.iter().zip(&member.program.inputs).filter_map(
                    move |(input, signature_input)| match input {
                        ShaderNodeIslandMemberInput::ImageReadAt(input_port) => Some((
                            input_port.clone(),
                            member.node_name.clone(),
                            signature_input.port.clone(),
                        )),
                        _ => None,
                    },
                )
            })
            .collect()
    }

    /// Take every message waiting at the inputs, answering whether the input
    /// port each evaluation waits on took a new frame.
    fn take_every_waiting_message(&mut self) -> Result<bool> {
        let Some(input_mailboxes) = self.input_mailboxes.as_ref() else {
            return Ok(false);
        };
        let the_input_port_each_evaluation_waits_on = self
            .island
            .the_input_port_each_evaluation_waits_on()
            .to_string();
        let mut the_waited_on_port_took_a_frame = false;
        for (input_port, _, _) in self.image_input_ports_read() {
            if !input_mailboxes.has_port(&input_port) {
                continue;
            }
            while let Some(message) = input_mailboxes.read_raw(&input_port)? {
                if message.payload_descriptor.kind
                    != DataChannelPayloadDescriptorKind::ImageFrame as u32
                {
                    continue;
                }
                self.newest_image_frame_by_input_port.insert(
                    input_port.clone(),
                    ImageFrameTakenAtAnInputPort {
                        image: ImageFrameDescriptorBody::from_descriptor_body(
                            &message.payload_descriptor.body,
                        ),
                        timestamp_ns: message.timestamp_ns,
                        group_boundary: message.group_boundary,
                    },
                );
                the_waited_on_port_took_a_frame |=
                    input_port == the_input_port_each_evaluation_waits_on;
            }
        }
        let float_input_ports: Vec<String> =
            self.value_by_float_input_port.keys().cloned().collect();
        for input_port in float_input_ports {
            if !input_mailboxes.has_port(&input_port) {
                continue;
            }
            while let Some(message) = input_mailboxes.read_raw(&input_port)? {
                if let Some(latest_value) = last_sample_of_the_first_channel(
                    message.payload_descriptor,
                    message.payload.bytes(),
                ) {
                    self.value_by_float_input_port
                        .insert(input_port.clone(), latest_value as f32);
                }
            }
        }
        Ok(the_waited_on_port_took_a_frame)
    }

    /// Run every kernel of the island over the newest frame at each Image
    /// input port in one submission, writing one Image frame stamped with the
    /// frame each evaluation waits on.
    fn evaluate_the_newest_frames(&mut self, gpu: &GpuContext) -> Result<()> {
        let mut resolved_inputs: HashMap<String, ImageInputResolvedForOneEvaluation> =
            HashMap::new();
        for (input_port, member_name, member_port) in self.image_input_ports_read() {
            let Some(image_frame) = self.newest_image_frame_by_input_port.get(&input_port) else {
                return Ok(());
            };
            let resolved =
                resolve_one_image_input(gpu, &member_name, &member_port, &image_frame.image)?;
            resolved_inputs.insert(input_port, resolved);
        }
        let waited_on_frame = *self
            .newest_image_frame_by_input_port
            .get(self.island.the_input_port_each_evaluation_waits_on())
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "shader node `{}` evaluated with no frame",
                    self.node_name
                ))
            })?;
        let output_extent_of_each_member = self.island.output_extent_of_each_member(|input_port| {
            resolved_inputs.get(input_port).map_or((0, 0), |resolved| {
                (resolved.extent.width, resolved.extent.height)
            })
        });
        let kernels = self
            .island
            .kernels_under_the_split_rule(&output_extent_of_each_member);
        let output_pixel_format = SHADER_NODE_OUTPUT_TEXTURE_FORMAT
            .host_view_pixel_format()
            .ok_or_else(|| {
                Error::Configuration("a shader node's output format has no pixel buffer".into())
            })?;

        let island_output_member = self.island.members().len() - 1;
        let mut written_pixel_buffer_by_member: HashMap<usize, NativeNodeOutputPixelBufferFrame> =
            HashMap::new();
        for kernel in &kernels {
            let member = kernel.output_member();
            let (width, height) = output_extent_of_each_member[member];
            let pool = if member == island_output_member {
                self.output_pixel_buffer_pool.as_mut().ok_or_else(|| {
                    Error::Runtime(format!(
                        "shader node `{}` ran before its setup",
                        self.node_name
                    ))
                })?
            } else {
                let node_name = &self.node_name;
                self.stored_pixel_buffer_pool_by_member
                    .entry(member)
                    .or_insert_with(|| {
                        gpu.create_processor_output_pixel_buffer_pool_for_a_native_node(&format!(
                            "{node_name}-stored-member-{member}"
                        ))
                    })
            };
            let pixel_buffer =
                pool.acquire_the_next_output_pixel_buffer(width, height, output_pixel_format)?;
            written_pixel_buffer_by_member.insert(member, pixel_buffer);
        }

        let mut dispatches = Vec::with_capacity(kernels.len());
        for (kernel_index, kernel) in kernels.iter().enumerate() {
            dispatches.push(self.dispatch_of_one_kernel(
                gpu,
                kernel_index,
                kernel,
                &resolved_inputs,
                &written_pixel_buffer_by_member,
                &output_extent_of_each_member,
            )?);
        }
        gpu.dispatch_compute_kernel_batch(&dispatches)?;
        if let Some(fused_group) = &self.fused_group {
            fused_group.note_an_evaluation_of_the_kernels(
                kernels
                    .iter()
                    .map(|kernel| {
                        kernel
                            .members
                            .iter()
                            .map(|&member| self.island.members()[member].node_name.clone())
                            .collect()
                    })
                    .collect(),
            );
        }
        let sampled_textures: Vec<(String, TextureRegistration)> = resolved_inputs
            .values()
            .filter_map(|input| match &input.surface_bound_resource {
                SurfaceBoundKernelBindingResource::SampledTexture(registration) => {
                    Some((input.surface_id.clone(), registration.clone()))
                }
                _ => None,
            })
            .collect();
        publish_the_layouts_a_dispatch_left_its_bound_textures_in(gpu, &sampled_textures);
        drop(resolved_inputs);

        let output_frame = written_pixel_buffer_by_member
            .remove(&island_output_member)
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "shader node `{}`: no kernel wrote the island's output",
                    self.node_name
                ))
            })?;
        let output_slot = output_frame.gpu_pool_slot_frame.ok_or_else(|| {
            Error::GpuError(format!(
                "shader node `{}`: output pixel buffer {} leases no claim-table row, so no reader \
                 could claim it",
                self.node_name, output_frame.published_frame_id
            ))
        })?;
        let (output_width, output_height) = output_extent_of_each_member[island_output_member];
        let output_writer = self.output_writer.as_ref().ok_or_else(|| {
            Error::Runtime(format!(
                "shader node `{}` has no output writer",
                self.node_name
            ))
        })?;
        output_writer.write_bytes_described_by(
            &self.island.output_member().program.output_port,
            &[],
            waited_on_frame.timestamp_ns,
            waited_on_frame.group_boundary,
            DataChannelPayloadDescriptor::image_frame(ImageFrameDescriptorBody {
                slot: output_slot,
                width: output_width,
                height: output_height,
                format: SHADER_NODE_OUTPUT_TEXTURE_FORMAT as u32,
                colour: waited_on_frame.image.colour,
            }),
        )
    }

    fn dispatch_of_one_kernel(
        &mut self,
        gpu: &GpuContext,
        kernel_index: usize,
        kernel: &ShaderNodeIslandKernel,
        resolved_inputs: &HashMap<String, ImageInputResolvedForOneEvaluation>,
        written_pixel_buffer_by_member: &HashMap<usize, NativeNodeOutputPixelBufferFrame>,
        output_extent_of_each_member: &[(u32, u32)],
    ) -> Result<BatchedComputeKernelDispatch> {
        let pixel_buffer_written_for = |member: usize| {
            written_pixel_buffer_by_member.get(&member).ok_or_else(|| {
                Error::Runtime(format!(
                    "shader node `{}`: member {member} has no pixel buffer to write",
                    self.node_name
                ))
            })
        };
        let mut backings = Vec::with_capacity(kernel.image_arguments.len());
        let mut extents = Vec::with_capacity(kernel.image_arguments.len());
        let mut bindings = vec![BatchedComputeKernelDispatchBinding {
            binding: 0,
            surface_bound_resource: SurfaceBoundKernelBindingResource::StorageBuffer(
                StorageBuffer::from_host_vulkan_buffer(Arc::clone(
                    &pixel_buffer_written_for(kernel.output_member())?
                        .pixel_buffer
                        .buffer_ref()
                        .inner,
                )),
            ),
        }];
        for (image_argument_index, image_argument) in kernel.image_arguments.iter().enumerate() {
            let (backing, extent, surface_bound_resource) = match image_argument {
                ShaderNodeKernelImageArgument::ReadAt(input_port) => {
                    let resolved = resolved_inputs.get(input_port).ok_or_else(|| {
                        Error::Runtime(format!(
                            "shader node `{}`: input port `{input_port}` has no frame",
                            self.node_name
                        ))
                    })?;
                    (
                        resolved.backing,
                        resolved.extent,
                        resolved.surface_bound_resource.clone(),
                    )
                }
                ShaderNodeKernelImageArgument::StoredOutputOf(member) => {
                    let (width, height) = output_extent_of_each_member[*member];
                    (
                        ShaderNodeImageInputBacking::Rgba8PixelBuffer,
                        ShaderNodeImageInputExtent {
                            width,
                            height,
                            row_pitch_in_pixels: width,
                        },
                        SurfaceBoundKernelBindingResource::StorageBuffer(
                            StorageBuffer::from_host_vulkan_buffer(Arc::clone(
                                &pixel_buffer_written_for(*member)?
                                    .pixel_buffer
                                    .buffer_ref()
                                    .inner,
                            )),
                        ),
                    )
                }
            };
            backings.push(backing);
            extents.push(extent);
            bindings.push(BatchedComputeKernelDispatchBinding {
                binding: image_argument_index as u32 + 1,
                surface_bound_resource,
            });
        }
        let float_values: Vec<f32> = kernel
            .float_arguments()
            .iter()
            .map(|float_argument| {
                let input_port = self.island.members()[float_argument.member]
                    .inputs
                    .iter()
                    .filter_map(|input| match input {
                        ShaderNodeIslandMemberInput::FloatReadAt { input_port, .. } => {
                            Some(input_port)
                        }
                        _ => None,
                    })
                    .nth(float_argument.float_index);
                input_port
                    .and_then(|input_port| self.value_by_float_input_port.get(input_port))
                    .copied()
                    .unwrap_or(0.0)
            })
            .collect();
        let (output_width, output_height) = output_extent_of_each_member[kernel.output_member()];
        let compiled_kernel = self.kernel_for(gpu, kernel_index, kernel, &backings)?;
        bindings.retain(|binding| {
            compiled_kernel
                .bindings()
                .iter()
                .any(|kept| kept.binding == binding.binding)
        });
        Ok(BatchedComputeKernelDispatch {
            kernel: compiled_kernel,
            bindings,
            push_constants: kernel.push_constants(
                (output_width, output_height),
                &extents,
                &float_values,
            ),
            group_count_x: output_width.div_ceil(SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE),
            group_count_y: output_height.div_ceil(SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE),
            group_count_z: 1,
        })
    }

    fn kernel_for(
        &mut self,
        gpu: &GpuContext,
        kernel_index: usize,
        kernel: &ShaderNodeIslandKernel,
        image_argument_backings: &[ShaderNodeImageInputBacking],
    ) -> Result<Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        let slang_source =
            kernel.compute_kernel_slang_source(&self.island, kernel_index, image_argument_backings);
        if let Some(compiled) = self.kernels_by_slang_source.get(&slang_source) {
            return Ok(Arc::clone(compiled));
        }
        let entry_point = ShaderNodeIslandKernel::entry_point(kernel_index);
        let member_names: Vec<&str> = kernel
            .members
            .iter()
            .map(|&member| self.island.members()[member].node_name.as_str())
            .collect();
        let spirv = gpu
            .compile_slang_shader_source_to_spirv(
                &slang_source,
                ShaderCompilationTargetStage::Compute,
                &entry_point,
                &entry_point,
            )
            .map_err(|refusal| {
                Error::GpuError(format!(
                    "shader node `{}`: the Slang source of {} does not compile: {refusal}",
                    self.node_name,
                    member_names.join(", ")
                ))
            })?;
        let (_, compiled) = gpu.create_or_reuse_compute_kernel(
            &spirv,
            kernel.push_constant_byte_len() as u32,
            &ShaderNodeIslandKernel::declared_bindings_the_compiled_kernel_kept(
                image_argument_backings,
                &crate::core::rhi::derive_bindings_from_spirv(&spirv)?.0,
            ),
            &entry_point,
        )?;
        self.kernels_by_slang_source
            .insert(slang_source, Arc::clone(&compiled));
        Ok(compiled)
    }

    fn log_a_refusal_once_until_it_changes(&mut self, refusal: &Error) {
        let refusal = refusal.to_string();
        if self.refusal_last_logged.as_deref() != Some(refusal.as_str()) {
            tracing::warn!(node = %self.node_name, %refusal, "shader node wrote no frame");
            self.refusal_last_logged = Some(refusal);
        }
    }
}

/// The refusal of an Image input frame of `format` a shader node does not
/// take, naming what goes in front of the input.
pub(crate) fn refusal_of_an_image_a_shader_node_does_not_take(
    node_name: &str,
    port: &str,
    format: Option<TextureFormat>,
    format_wire_value: u32,
) -> Error {
    let named_format = format.map_or_else(
        || format!("an unknown texture format ({format_wire_value})"),
        |format| format.wire_name().to_string(),
    );
    Error::PortError(format!(
        "shader node `{node_name}`: input `{port}` carries a {named_format} Image, and a shader \
         node takes RGBA Images only: rgba8_unorm, rgba8_unorm_srgb, rgba16_float or \
         rgba32_float. To fix: put a node that converts its Image to rgba8_unorm in front of \
         `{port}`"
    ))
}

fn resolve_one_image_input(
    gpu: &GpuContext,
    node_name: &str,
    port: &str,
    image: &ImageFrameDescriptorBody,
) -> Result<ImageInputResolvedForOneEvaluation> {
    let format = TextureFormat::from_repr_value(image.format);
    if !format.is_some_and(a_shader_node_takes_an_image_of) {
        return Err(refusal_of_an_image_a_shader_node_does_not_take(
            node_name,
            port,
            format,
            image.format,
        ));
    }
    let format = format.unwrap_or(TextureFormat::Rgba8Unorm);
    let surface_id = gpu.published_frame_id_of_the_gpu_pool_slot_frame(&image.slot)?;
    let claim_against_producer_reuse = gpu.claim_surface_against_producer_reuse(&surface_id)?;
    let (backing, surface_bound_resource, row_pitch_in_pixels) =
        match gpu.resolve_device_export_source(&surface_id)? {
            ResolvedSurfaceBacking::RegisteredTexture(registration) => (
                ShaderNodeImageInputBacking::SampledTexture,
                SurfaceBoundKernelBindingResource::SampledTexture(registration),
                image.width,
            ),
            ResolvedSurfaceBacking::PixelBuffer(pixel_buffer) => {
                let backing = ShaderNodeImageInputBacking::of_a_pixel_buffer_of(format)
                    .ok_or_else(|| {
                        refusal_of_an_image_a_shader_node_does_not_take(
                            node_name,
                            port,
                            Some(format),
                            image.format,
                        )
                    })?;
                let bytes_per_pixel = u64::from(format.bytes_per_pixel());
                let row_pitch_in_pixels = u32::try_from(
                    pixel_buffer.plane_size(0)
                        / (u64::from(image.height.max(1)) * bytes_per_pixel.max(1)),
                )
                .unwrap_or(image.width)
                .max(image.width);
                (
                    backing,
                    SurfaceBoundKernelBindingResource::StorageBuffer(
                        StorageBuffer::from_host_vulkan_buffer(Arc::clone(
                            &pixel_buffer.buffer_ref().inner,
                        )),
                    ),
                    row_pitch_in_pixels,
                )
            }
        };
    Ok(ImageInputResolvedForOneEvaluation {
        backing,
        extent: ShaderNodeImageInputExtent {
            width: image.width,
            height: image.height,
            row_pitch_in_pixels,
        },
        surface_id,
        surface_bound_resource,
        _claim_against_producer_reuse: claim_against_producer_reuse,
    })
}

/// The last sample of the first channel of the Channels block a message
/// carries; `None` for any other message, or a block of no samples.
fn last_sample_of_the_first_channel(
    payload_descriptor: DataChannelPayloadDescriptor,
    payload: &[u8],
) -> Option<f64> {
    if payload_descriptor.kind != DataChannelPayloadDescriptorKind::ChannelsBlock as u32 {
        return None;
    }
    let body = ChannelsBlockDescriptorBody::from_descriptor_body(&payload_descriptor.body);
    let sample_type = ChannelsBlockSampleType::from_wire(body.sample_type)?;
    let sample_byte_len = sample_type.sample_byte_len();
    let samples_per_channel = body.samples_per_channel as usize;
    if samples_per_channel == 0 || body.channel_count == 0 {
        return None;
    }
    let last_sample_start = (samples_per_channel - 1) * sample_byte_len;
    let last_sample = payload.get(last_sample_start..last_sample_start + sample_byte_len)?;
    match sample_type {
        ChannelsBlockSampleType::F32 => {
            Some(f64::from(f32::from_le_bytes(last_sample.try_into().ok()?)))
        }
        ChannelsBlockSampleType::F64 => Some(f64::from_le_bytes(last_sample.try_into().ok()?)),
    }
}

impl DynGeneratedProcessor for ShaderNodeNativeEvaluatorProcessor {
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.output_pixel_buffer_pool = Some(
            ctx.gpu_full_access()
                .create_processor_output_pixel_buffer_pool_for_a_native_node(&self.node_name),
        );
        Ok(())
    }

    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.output_pixel_buffer_pool = None;
        self.stored_pixel_buffer_pool_by_member.clear();
        self.kernels_by_slang_source.clear();
        Ok(())
    }

    fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.take_every_waiting_message()? {
            return Ok(());
        }
        match self.evaluate_the_newest_frames(ctx.gpu_limited_access().host_inner()) {
            Ok(()) => {
                self.refusal_last_logged = None;
                Ok(())
            }
            Err(refusal) => {
                self.log_a_refusal_once_until_it_changes(&refusal);
                Ok(())
            }
        }
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(Error::Runtime(
            "a shader node is reactive, so it is never started by hand".into(),
        ))
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(Error::Runtime(
            "a shader node is reactive, so it is never stopped by hand".into(),
        ))
    }

    fn name(&self) -> &str {
        &self.node_name
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        Some(self.descriptor.clone())
    }

    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::new(ProcessExecution::Reactive)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        true
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        true
    }

    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<OutputWriter>,
        input_mailboxes: Option<InputMailboxes>,
    ) -> Result<()> {
        if output_writer.is_some() {
            self.output_writer = output_writer;
        }
        if input_mailboxes.is_some() {
            self.input_mailboxes = input_mailboxes;
        }
        Ok(())
    }

    fn iceoryx2_output_writer_inner(&self) -> Option<Arc<OutputWriterInner>> {
        self.output_writer
            .as_ref()
            .and_then(OutputWriter::inner_arc)
    }

    fn iceoryx2_input_mailboxes_inner(&self) -> Option<Arc<InputMailboxesInner>> {
        self.input_mailboxes
            .as_ref()
            .and_then(InputMailboxes::inner_arc)
    }

    fn apply_config_json(&mut self, _config_json: &serde_json::Value) -> Result<()> {
        Err(Error::Configuration(format!(
            "shader node `{}` declares no configuration; its settings are its float inputs",
            self.node_name
        )))
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn config_json(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_rgba_image_is_refused_naming_its_format_the_port_and_the_conversion() {
        for format in [
            TextureFormat::Bgra8Unorm,
            TextureFormat::Bgra8UnormSrgb,
            TextureFormat::Nv12,
        ] {
            let refusal = refusal_of_an_image_a_shader_node_does_not_take(
                "tint",
                "image",
                Some(format),
                format as u32,
            )
            .to_string();
            assert!(refusal.contains(format.wire_name()), "{refusal}");
            assert!(refusal.contains("input `image`"), "{refusal}");
            assert!(
                refusal.contains("converts its Image to rgba8_unorm"),
                "{refusal}"
            );
        }
    }

    #[test]
    fn a_float_input_reads_the_last_sample_of_its_first_channel() {
        let samples: Vec<u8> = [0.25f32, 0.75f32]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let descriptor =
            DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
                start_sample: 0,
                sample_rate: 100,
                channel_count: 1,
                samples_per_channel: 2,
                sample_type: ChannelsBlockSampleType::F32 as u32,
            });
        assert_eq!(
            last_sample_of_the_first_channel(descriptor, &samples),
            Some(0.75)
        );
        assert_eq!(
            last_sample_of_the_first_channel(DataChannelPayloadDescriptor::bytes(), &samples),
            None
        );
    }
}
