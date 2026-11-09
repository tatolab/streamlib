// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The native evaluator of a shader node: its Slang function compiled into a
//! compute kernel and run in this process on each frame its first Image input
//! takes, writing one Image frame from the node's own output pool.

use std::collections::HashMap;
use std::sync::Arc;

use super::shader_node_program::{
    SHADER_NODE_KERNEL_ENTRY_POINT, SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE,
    SHADER_NODE_OUTPUT_TEXTURE_FORMAT, ShaderNodeImageInputBacking, ShaderNodeImageInputExtent,
    ShaderNodeProgram, a_shader_node_takes_an_image_of,
};
use super::subprocess_escalate::publish_the_layouts_a_dispatch_left_its_bound_textures_in;
use crate::core::context::surface_backing_resolution::ResolvedSurfaceBacking;
use crate::core::context::{
    BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding, GpuContext,
    ProcessorOutputPixelBufferPoolOfOneNativeNode, RuntimeContextFullAccess,
    RuntimeContextLimitedAccess, SurfaceBoundKernelBindingResource,
    SurfaceClaimedAgainstProducerReuse, TextureRegistration,
};
use crate::core::descriptors::ProcessorDescriptor;
use crate::core::error::{Error, Result};
use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::ProcessorNode;
use crate::core::processors::DynGeneratedProcessor;
use crate::core::rhi::{ShaderCompilationTargetStage, StorageBuffer, TextureFormat};
use crate::iceoryx2::{
    InputMailboxes, InputMailboxesInner, MessageGroupBoundary, OutputWriter, OutputWriterInner,
};
use streamlib_ipc_types::{
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind, ImageFrameDescriptorBody,
};

/// The newest Image frame one Image input of a shader node has taken.
#[derive(Clone, Copy)]
struct ImageFrameTakenByAShaderNodeInput {
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

/// The processor a shader node runs as in this process.
pub(crate) struct ShaderNodeNativeEvaluatorProcessor {
    node_name: String,
    descriptor: ProcessorDescriptor,
    program: ShaderNodeProgram,
    output_writer: Option<OutputWriter>,
    input_mailboxes: Option<InputMailboxes>,
    output_pixel_buffer_pool: Option<ProcessorOutputPixelBufferPoolOfOneNativeNode>,
    kernels_by_image_input_backings:
        HashMap<Vec<ShaderNodeImageInputBacking>, Arc<crate::vulkan::rhi::VulkanComputeKernel>>,
    newest_image_frame_by_image_input: Vec<Option<ImageFrameTakenByAShaderNodeInput>>,
    value_by_float_input: Vec<f32>,
    refusal_last_logged: Option<String>,
}

impl ShaderNodeNativeEvaluatorProcessor {
    /// The evaluator of shader node `node`, running `program`; each float input
    /// starts at its pin value, else its declared default, else zero.
    pub(crate) fn for_the_node(
        node: &ProcessorNode,
        descriptor: ProcessorDescriptor,
        program: ShaderNodeProgram,
    ) -> Self {
        let value_by_float_input = program
            .float_input_ports()
            .map(|port| {
                node.pins
                    .get(port)
                    .or_else(|| {
                        node.ports
                            .inputs
                            .iter()
                            .find(|input| input.name == port)
                            .and_then(|input| input.pin_default.as_ref())
                    })
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0) as f32
            })
            .collect();
        Self {
            node_name: node.id.to_string(),
            newest_image_frame_by_image_input: vec![None; program.image_input_ports().count()],
            descriptor,
            program,
            output_writer: None,
            input_mailboxes: None,
            output_pixel_buffer_pool: None,
            kernels_by_image_input_backings: HashMap::new(),
            value_by_float_input,
            refusal_last_logged: None,
        }
    }

    /// Take every message waiting at the inputs, answering whether the first
    /// Image input took a new frame.
    fn take_every_waiting_message(&mut self) -> Result<bool> {
        let Some(input_mailboxes) = self.input_mailboxes.as_ref() else {
            return Ok(false);
        };
        let mut the_first_image_input_took_a_frame = false;
        let image_input_ports: Vec<String> = self
            .program
            .image_input_ports()
            .map(str::to_string)
            .collect();
        for (image_input_index, port) in image_input_ports.iter().enumerate() {
            while let Some(message) = input_mailboxes.read_raw(port)? {
                if message.payload_descriptor.kind
                    != DataChannelPayloadDescriptorKind::ImageFrame as u32
                {
                    continue;
                }
                self.newest_image_frame_by_image_input[image_input_index] =
                    Some(ImageFrameTakenByAShaderNodeInput {
                        image: ImageFrameDescriptorBody::from_descriptor_body(
                            &message.payload_descriptor.body,
                        ),
                        timestamp_ns: message.timestamp_ns,
                        group_boundary: message.group_boundary,
                    });
                the_first_image_input_took_a_frame |= image_input_index == 0;
            }
        }
        let float_input_ports: Vec<String> = self
            .program
            .float_input_ports()
            .map(str::to_string)
            .collect();
        for (float_index, port) in float_input_ports.iter().enumerate() {
            while let Some(message) = input_mailboxes.read_raw(port)? {
                if let Some(latest_value) = last_sample_of_the_first_channel(
                    message.payload_descriptor,
                    message.payload.bytes(),
                ) {
                    self.value_by_float_input[float_index] = latest_value as f32;
                }
            }
        }
        Ok(the_first_image_input_took_a_frame)
    }

    /// Run the kernel over the newest frame of every Image input, writing one
    /// Image frame stamped with the first input's frame.
    fn evaluate_the_newest_frames(&mut self, gpu: &GpuContext) -> Result<()> {
        let image_frames: Vec<ImageFrameTakenByAShaderNodeInput> = match self
            .newest_image_frame_by_image_input
            .iter()
            .copied()
            .collect::<Option<Vec<_>>>()
        {
            Some(image_frames) => image_frames,
            None => return Ok(()),
        };
        let image_input_ports: Vec<String> = self
            .program
            .image_input_ports()
            .map(str::to_string)
            .collect();
        let resolved_inputs = image_frames
            .iter()
            .zip(&image_input_ports)
            .map(|(image_frame, port)| {
                resolve_one_image_input(gpu, &self.node_name, port, &image_frame.image)
            })
            .collect::<Result<Vec<_>>>()?;
        let image_input_backings: Vec<ShaderNodeImageInputBacking> =
            resolved_inputs.iter().map(|input| input.backing).collect();
        let kernel = self.kernel_for(gpu, &image_input_backings)?;

        let first_image = image_frames[0].image;
        let (output_width, output_height) = (first_image.width, first_image.height);
        let output_pixel_buffer_pool = self.output_pixel_buffer_pool.as_mut().ok_or_else(|| {
            Error::Runtime(format!(
                "shader node `{}` ran before its setup",
                self.node_name
            ))
        })?;
        let output_pixel_format = SHADER_NODE_OUTPUT_TEXTURE_FORMAT
            .host_view_pixel_format()
            .ok_or_else(|| {
                Error::Configuration("a shader node's output format has no pixel buffer".into())
            })?;
        let output_frame = output_pixel_buffer_pool.acquire_the_next_output_pixel_buffer(
            output_width,
            output_height,
            output_pixel_format,
        )?;
        let output_slot = output_frame.gpu_pool_slot_frame.ok_or_else(|| {
            Error::GpuError(format!(
                "shader node `{}`: output pixel buffer {} leases no claim-table row, so no reader \
                 could claim it",
                self.node_name, output_frame.published_frame_id
            ))
        })?;

        let mut bindings = vec![BatchedComputeKernelDispatchBinding {
            binding: 0,
            surface_bound_resource: SurfaceBoundKernelBindingResource::StorageBuffer(
                StorageBuffer::from_host_vulkan_buffer(Arc::clone(
                    &output_frame.pixel_buffer.buffer_ref().inner,
                )),
            ),
        }];
        bindings.extend(
            resolved_inputs
                .iter()
                .enumerate()
                .map(
                    |(image_input_index, input)| BatchedComputeKernelDispatchBinding {
                        binding: image_input_index as u32 + 1,
                        surface_bound_resource: input.surface_bound_resource.clone(),
                    },
                ),
        );
        let extents: Vec<ShaderNodeImageInputExtent> =
            resolved_inputs.iter().map(|input| input.extent).collect();
        let dispatch = BatchedComputeKernelDispatch {
            kernel,
            bindings,
            push_constants: self.program.push_constants(
                (output_width, output_height),
                &extents,
                &self.value_by_float_input,
            ),
            group_count_x: output_width.div_ceil(SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE),
            group_count_y: output_height.div_ceil(SHADER_NODE_KERNEL_WORKGROUP_TILE_SIZE),
            group_count_z: 1,
        };
        gpu.dispatch_compute_kernel_batch(std::slice::from_ref(&dispatch))?;
        let sampled_textures: Vec<(String, TextureRegistration)> = resolved_inputs
            .iter()
            .filter_map(|input| match &input.surface_bound_resource {
                SurfaceBoundKernelBindingResource::SampledTexture(registration) => {
                    Some((input.surface_id.clone(), registration.clone()))
                }
                _ => None,
            })
            .collect();
        publish_the_layouts_a_dispatch_left_its_bound_textures_in(gpu, &sampled_textures);
        drop(resolved_inputs);

        let output_writer = self.output_writer.as_ref().ok_or_else(|| {
            Error::Runtime(format!(
                "shader node `{}` has no output writer",
                self.node_name
            ))
        })?;
        output_writer.write_bytes_described_by(
            &self.program.output_port,
            &[],
            image_frames[0].timestamp_ns,
            image_frames[0].group_boundary,
            DataChannelPayloadDescriptor::image_frame(ImageFrameDescriptorBody {
                slot: output_slot,
                width: output_width,
                height: output_height,
                format: SHADER_NODE_OUTPUT_TEXTURE_FORMAT as u32,
                colour: first_image.colour,
            }),
        )
    }

    fn kernel_for(
        &mut self,
        gpu: &GpuContext,
        image_input_backings: &[ShaderNodeImageInputBacking],
    ) -> Result<Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        if let Some(kernel) = self
            .kernels_by_image_input_backings
            .get(image_input_backings)
        {
            return Ok(Arc::clone(kernel));
        }
        let spirv = gpu
            .compile_slang_shader_source_to_spirv(
                &self
                    .program
                    .compute_kernel_slang_source(image_input_backings),
                ShaderCompilationTargetStage::Compute,
                SHADER_NODE_KERNEL_ENTRY_POINT,
                &self.program.function_name,
            )
            .map_err(|refusal| {
                Error::GpuError(format!(
                    "shader node `{}`: the Slang source of `{}` does not compile: {refusal}",
                    self.node_name, self.program.function_name
                ))
            })?;
        let (_, kernel) = gpu.create_or_reuse_compute_kernel(
            &spirv,
            self.program.push_constant_byte_len() as u32,
            &ShaderNodeProgram::declared_bindings(image_input_backings),
            SHADER_NODE_KERNEL_ENTRY_POINT,
        )?;
        self.kernels_by_image_input_backings
            .insert(image_input_backings.to_vec(), Arc::clone(&kernel));
        Ok(kernel)
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
        self.kernels_by_image_input_backings.clear();
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
