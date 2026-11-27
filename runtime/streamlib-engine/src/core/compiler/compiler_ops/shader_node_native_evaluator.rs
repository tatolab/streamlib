// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The native evaluator of a shader node island: each kernel the split rule
//! cuts the island into, compiled from its members' Slang and run in this
//! process in one submission on each frame the island's leading Image input
//! takes, writing one Image frame from the evaluating node's own output pool.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::shader_node_island::{
    ShaderNodeIslandHandedToItsEvaluator, ShaderNodeIslandHostedHereComponent,
    ShaderNodeIslandWithItsFusedGroup,
    numbers_of_a_shader_node_input,
};
use super::shader_node_island_program::{
    ShaderNodeIslandKernel, ShaderNodeIslandMemberInput, ShaderNodeIslandProgram,
    ShaderNodeKernelImageArgument,
};
use super::shader_node_program::{
    SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE, SHADER_NODE_OUTPUT_TEXTURE_FORMAT,
    ShaderNodeImageInputBacking, ShaderNodeImageInputExtent, ShaderNodeInputValueType,
    ShaderNodeProgram, a_shader_node_takes_an_image_of,
};
use crate::core::context::surfaces_and_their_stores::surface_backing_resolution::ResolvedSurfaceBacking;
use crate::core::context::surfaces_and_their_stores::texture_layouts_published_to_the_surface_store::publish_the_layouts_a_dispatch_left_its_bound_textures_in;
use crate::core::context::{
    BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding, GpuContext,
    NativeNodeOutputPixelBufferFrame, NodeOutputPixelBufferPoolOfOneNativeNode,
    RuntimeContextFullAccess, RuntimeContextLimitedAccess, SurfaceBoundKernelBindingResource,
    SurfaceClaimedAgainstProducerReuse, TextureRegistration,
};
use crate::core::descriptors::NodeDescriptor;
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::{GraphNode, GraphNodeWithComponents, ShaderNodeFusedGroupOfAnIsland};
use crate::core::nodes::DynGeneratedNode;
use crate::core::rhi::{ShaderCompilationTargetStage, StorageBuffer, TextureFormat};
use crate::iceoryx2::{
    InputMailboxes, InputMailboxesInner, MessageGroupBoundary, OutputWriter, OutputWriterInner,
};
use streamlib_ipc_types::{
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, ImageFrameDescriptorBody,
};

/// The numbers one input port of the evaluator reads, a Float, a Color or a
/// Vector, and the value type it reads them as.
struct NumbersReadAtAnInputPort {
    value_type: ShaderNodeInputValueType,
    numbers: Vec<f32>,
}

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

pub(crate) struct ShaderNodeNativeEvaluator {
    node_name: String,
    descriptor: NodeDescriptor,
    island: ShaderNodeIslandProgram,
    fused_group: Option<Arc<ShaderNodeFusedGroupOfAnIsland>>,
    island_handed_over: Option<Arc<ShaderNodeIslandHandedToItsEvaluator>>,
    output_writer: Option<OutputWriter>,
    input_mailboxes: Option<InputMailboxes>,
    output_pixel_buffer_pool: Option<NodeOutputPixelBufferPoolOfOneNativeNode>,
    stored_pixel_buffer_pool_by_member: HashMap<usize, NodeOutputPixelBufferPoolOfOneNativeNode>,
    kernels_by_slang_source: HashMap<String, Arc<crate::vulkan::rhi::VulkanComputeKernel>>,
    newest_image_frame_by_input_port: HashMap<String, ImageFrameTakenAtAnInputPort>,
    numbers_by_input_port: HashMap<String, NumbersReadAtAnInputPort>,
    refusal_last_logged: Option<String>,
}

impl ShaderNodeNativeEvaluator {
    /// The evaluator of the island shader node `node` hosts, or of `node`
    /// alone running `program` when it hosts none; each input of numbers starts
    /// at its pin value, else its declared default, else zero.
    pub(crate) fn for_the_node(
        node: &GraphNode,
        descriptor: NodeDescriptor,
        program: ShaderNodeProgram,
    ) -> Self {
        let (island, fused_group, island_handed_over) =
            match node.get::<ShaderNodeIslandHostedHereComponent>() {
                Some(hosted) => (
                    hosted.island.clone(),
                    Some(Arc::clone(&hosted.fused_group)),
                    Some(Arc::clone(&hosted.handed_to_its_evaluator)),
                ),
                None => (
                    ShaderNodeIslandProgram::of_one_node(
                        node.display_name.clone(),
                        program,
                        |port, value_type| numbers_of_a_shader_node_input(node, port, value_type),
                    ),
                    None,
                    None,
                ),
            };
        let numbers_by_input_port =
            the_numbers_each_input_port_of_an_island_reads(&island, |_| None);
        Self {
            node_name: node.id.to_string(),
            descriptor,
            island,
            fused_group,
            island_handed_over,
            output_writer: None,
            input_mailboxes: None,
            output_pixel_buffer_pool: None,
            stored_pixel_buffer_pool_by_member: HashMap::new(),
            kernels_by_slang_source: HashMap::new(),
            newest_image_frame_by_input_port: HashMap::new(),
            numbers_by_input_port,
            refusal_last_logged: None,
        }
    }

    /// Take the island last handed to this evaluator, if one was, in place of the one it
    /// evaluates: each input of numbers both read goes on reading the value it reads now, a new
    /// one starts at the value the island was placed with, and the newest frame of an Image
    /// input the new island no longer reads is let go.
    fn take_the_island_handed_over(&mut self) {
        let Some(ShaderNodeIslandWithItsFusedGroup {
            island,
            fused_group,
        }) = self
            .island_handed_over
            .as_ref()
            .and_then(|handed_over| handed_over.take())
        else {
            return;
        };
        let numbers_read_before = std::mem::take(&mut self.numbers_by_input_port);
        self.numbers_by_input_port =
            the_numbers_each_input_port_of_an_island_reads(&island, |input_port| {
                numbers_read_before
                    .get(input_port)
                    .map(|read| read.numbers.clone())
            });
        self.island = island;
        let image_input_ports_read: HashSet<String> = self
            .image_input_ports_read()
            .into_iter()
            .map(|(input_port, _, _)| input_port)
            .collect();
        self.newest_image_frame_by_input_port
            .retain(|input_port, _| image_input_ports_read.contains(input_port));
        self.fused_group = Some(fused_group);
        self.refusal_last_logged = None;
    }

    /// The members of the island it evaluates now, by name.
    #[cfg(test)]
    pub(crate) fn the_members_it_evaluates(&self) -> Vec<String> {
        self.island.member_names()
    }

    /// The numbers input `input_port` of the island reads now.
    #[cfg(test)]
    pub(crate) fn the_numbers_an_input_reads(&self, input_port: &str) -> Option<&[f32]> {
        self.numbers_by_input_port
            .get(input_port)
            .map(|read| read.numbers.as_slice())
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
        for (input_port, read) in &mut self.numbers_by_input_port {
            if !input_mailboxes.has_port(input_port) {
                continue;
            }
            while let Some(message) = input_mailboxes.read_raw(input_port)? {
                if let Some(numbers) =
                    last_sample_of_each_channel(message.payload_descriptor, message.payload.bytes())
                        .and_then(|channel_values| {
                            read.value_type
                                .numbers_a_control_message_carries(&channel_values)
                        })
                {
                    read.numbers = numbers;
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
                        gpu.create_node_output_pixel_buffer_pool_for_a_native_node(&format!(
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
        let numbers: Vec<f32> = kernel
            .number_arguments()
            .iter()
            .flat_map(|argument| {
                let numbers_read =
                    match &self.island.members()[argument.member].inputs[argument.input_index] {
                        ShaderNodeIslandMemberInput::NumbersReadAt { input_port, .. } => self
                            .numbers_by_input_port
                            .get(input_port)
                            .map(|read| read.numbers.as_slice()),
                        _ => None,
                    }
                    .unwrap_or_default();
                (0..argument.number_count)
                    .map(move |part| numbers_read.get(part).copied().unwrap_or_default())
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
                &numbers,
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
        // Its registration is never released, so the island's kernel is held
        // for the context's life.
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

/// The numbers each input port of `island` that takes numbers reads: what `reading_now` gives
/// for its port, else the value the island starts it at.
fn the_numbers_each_input_port_of_an_island_reads(
    island: &ShaderNodeIslandProgram,
    reading_now: impl Fn(&str) -> Option<Vec<f32>>,
) -> HashMap<String, NumbersReadAtAnInputPort> {
    island
        .members()
        .iter()
        .flat_map(|member| member.inputs.iter().zip(&member.program.inputs))
        .filter_map(|(input, signature_input)| match input {
            ShaderNodeIslandMemberInput::NumbersReadAt {
                input_port,
                numbers_until_one_is_read,
            } => Some((
                input_port.clone(),
                NumbersReadAtAnInputPort {
                    value_type: signature_input.value_type.clone(),
                    numbers: reading_now(input_port)
                        .unwrap_or_else(|| numbers_until_one_is_read.clone()),
                },
            )),
            _ => None,
        })
        .collect()
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

/// The last sample of each channel of the Channels block a message carries,
/// channel after channel; `None` for any other message, or a block of no
/// samples.
fn last_sample_of_each_channel(
    payload_descriptor: DataChannelPayloadDescriptor,
    payload: &[u8],
) -> Option<Vec<f64>> {
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
    (0..body.channel_count as usize)
        .map(|channel| {
            let last_sample_start =
                (channel * samples_per_channel + samples_per_channel - 1) * sample_byte_len;
            let last_sample =
                payload.get(last_sample_start..last_sample_start + sample_byte_len)?;
            match sample_type {
                ChannelsBlockSampleType::F32 => {
                    Some(f64::from(f32::from_le_bytes(last_sample.try_into().ok()?)))
                }
                ChannelsBlockSampleType::F64 => {
                    Some(f64::from_le_bytes(last_sample.try_into().ok()?))
                }
            }
        })
        .collect()
}

impl DynGeneratedNode for ShaderNodeNativeEvaluator {
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.output_pixel_buffer_pool = Some(
            ctx.gpu_full_access()
                .create_node_output_pixel_buffer_pool_for_a_native_node(&self.node_name),
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
        self.take_the_island_handed_over();
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

    fn descriptor(&self) -> Option<NodeDescriptor> {
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

    /// Read each input of numbers of the island no link feeds as its value in `input_values`,
    /// keyed by the input port it is read at, and one `input_values` holds no value of its type
    /// for as 0, as a freshly built island reads it; a linked input goes on reading its link. An
    /// island handed over and not yet taken is taken first, so the values reach the island it
    /// evaluates from then on.
    fn set_unlinked_input_values(
        &mut self,
        input_values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        self.take_the_island_handed_over();
        let input_mailboxes = self.input_mailboxes.as_ref();
        for (input_port, read) in &mut self.numbers_by_input_port {
            if input_mailboxes
                .is_some_and(|input_mailboxes| input_mailboxes.is_fed_by_a_link(input_port))
            {
                continue;
            }
            read.numbers = read
                .value_type
                .numbers_a_stored_value_holds(input_values.get(input_port));
        }
        Ok(())
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        serde_json::Value::Null
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
    fn an_input_of_numbers_reads_the_last_sample_of_each_channel() {
        let samples: Vec<u8> = [0.25f32, 0.75, 0.5, 1.0]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let block_of = |channel_count: u32, samples_per_channel: u32| {
            DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
                start_sample: 0,
                sample_rate: 100,
                channel_count,
                samples_per_channel,
                sample_type: ChannelsBlockSampleType::F32 as u32,
            })
        };
        assert_eq!(
            last_sample_of_each_channel(block_of(1, 4), &samples),
            Some(vec![1.0])
        );
        assert_eq!(
            last_sample_of_each_channel(block_of(2, 2), &samples),
            Some(vec![0.75, 1.0])
        );
        assert_eq!(
            last_sample_of_each_channel(block_of(4, 1), &samples),
            Some(vec![0.25, 0.75, 0.5, 1.0])
        );
        assert_eq!(
            last_sample_of_each_channel(DataChannelPayloadDescriptor::bytes(), &samples),
            None
        );
    }

    #[test]
    fn a_color_input_reads_a_vector_or_one_number_with_the_free_conversion() {
        let color = ShaderNodeInputValueType::Numbers(crate::core::descriptors::PinType::Color {});
        assert_eq!(
            color.numbers_a_control_message_carries(&[0.25, 0.5, 0.75, 1.0]),
            Some(vec![0.25, 0.5, 0.75, 1.0])
        );
        assert_eq!(
            color.numbers_a_control_message_carries(&[0.5]),
            Some(vec![0.5, 0.5, 0.5, 1.0])
        );
        assert_eq!(
            color.numbers_a_stored_value_holds(Some(&serde_json::json!({
                "red": 1.0, "green": 0.5, "blue": 0.25, "alpha": 0.75,
            }))),
            vec![1.0, 0.5, 0.25, 0.75]
        );
        assert_eq!(
            color.numbers_a_stored_value_holds(None),
            vec![0.0, 0.0, 0.0, 1.0]
        );
    }
}
