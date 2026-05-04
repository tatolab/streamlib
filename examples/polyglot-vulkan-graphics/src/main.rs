// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Polyglot Vulkan adapter graphics scenario (#656).
//!
//! End-to-end gate for the subprocess `VulkanContext.dispatch_graphics`
//! path: the host pre-allocates ONE render-target-capable DMA-BUF
//! surface AND an exportable `HostVulkanTimelineSemaphore`, registers
//! both with surface-share under a known UUID, and installs a
//! `GraphicsKernelBridge` wired to its `VulkanGraphicsKernel`. A
//! Python or Deno polyglot processor opens the surface through
//! `VulkanContext.acquire_write` and calls `dispatch_graphics`, which
//! routes through escalate IPC's `register_graphics_kernel` +
//! `run_graphics_draw` ops to the host's
//! `VulkanGraphicsKernel::offscreen_render`. This binary then reads
//! the surface back via Vulkan and writes a PNG; reading the PNG
//! with the Read tool is the visual gate.
//!
//! The shaders (`shaders/triangle.{vert,frag}`) are compiled to
//! SPIR-V at build time via `build.rs`, embedded as bytes here, and
//! shipped to the polyglot processor via the processor config as hex
//! strings.
//!
//! Build the Python `.slpkg` first:
//!   cargo run -p streamlib-cli -- pack examples/polyglot-vulkan-graphics/python
//!
//! Run:
//!   cargo run -p polyglot-vulkan-graphics-scenario -- \
//!       --runtime=python --output=/tmp/vulkan-triangle-py.png
//!   cargo run -p polyglot-vulkan-graphics-scenario -- \
//!       --runtime=deno   --output=/tmp/vulkan-triangle-deno.png

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use streamlib::core::context::{
    BlendFactorWire, BlendOpWire, CullModeWire, DynamicStateWire, FrontFaceWire,
    GraphicsBindingDecl, GraphicsKernelBridge, GraphicsKernelRegisterDecl,
    GraphicsKernelRunDraw, GraphicsPipelineStateWire, PolygonModeWire,
    PrimitiveTopologyWire,
};
use streamlib::core::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, DepthStencilState,
    GraphicsBindingSpec, GraphicsDynamicState, GraphicsKernelDescriptor,
    GraphicsPipelineState, GraphicsPushConstants, GraphicsShaderStage,
    GraphicsShaderStageFlags, GraphicsStage, MultisampleState, PrimitiveTopology,
    RasterizationState, StreamTexture, TextureFormat, TextureReadbackDescriptor,
    TextureSourceLayout, VertexInputState,
};
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef, StreamError};
use streamlib::host_rhi::{
    HostVulkanDevice, HostVulkanTimelineSemaphore, OffscreenColorTarget, OffscreenDraw,
    VulkanGraphicsKernel, VulkanTextureReadback,
};
use streamlib::{BgraFileSourceProcessor, ProcessorSpec, Result, StreamRuntime};

/// Compiled SPIR-V for the triangle vertex shader.
const TRIANGLE_VERT_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/triangle.vert.spv"));

/// Compiled SPIR-V for the triangle fragment shader.
const TRIANGLE_FRAG_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/triangle.frag.spv"));

/// UUID the host registers the render-target surface under.
const SCENARIO_SURFACE_UUID: &str = "00000000-0000-0000-0000-0000000006a1";

/// Side length of the surface (square keeps the reader-tool gate easy
/// to interpret; 512 is large enough to be visually obvious).
const SURFACE_SIZE: u32 = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeKind {
    Python,
    Deno,
}

impl RuntimeKind {
    fn parse(s: &str) -> std::result::Result<Self, String> {
        match s {
            "python" => Ok(Self::Python),
            "deno" => Ok(Self::Deno),
            other => Err(format!(
                "unknown --runtime value '{other}' (expected 'python' or 'deno')"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Deno => "deno",
        }
    }

    fn processor_name(self) -> &'static str {
        match self {
            Self::Python => "com.tatolab.vulkan_graphics",
            Self::Deno => "com.tatolab.vulkan_graphics_deno",
        }
    }
}

/// Bridge between the host runtime's `set_graphics_kernel_bridge` and
/// the host's `VulkanGraphicsKernel`. Lives in this example because
/// the `GraphicsKernelBridge` trait lives in `streamlib` and the
/// `streamlib-adapter-vulkan` crate cannot depend on the full
/// `streamlib` (the consumer-rhi capability boundary forbids it).
///
/// Holds a UUID → `StreamTexture` map populated at setup time so
/// `run_graphics_draw(color_target_uuids[…], ...)` can resolve to the
/// host's `VkImage` for the offscreen color target. The kernel cache
/// is keyed by SHA-256 over a canonical byte representation of the
/// register-time descriptor.
struct TriangleKernelBridge {
    device: Arc<HostVulkanDevice>,
    surfaces: HashMap<String, StreamTexture>,
    kernels: parking_lot::Mutex<HashMap<String, Arc<VulkanGraphicsKernel>>>,
}

impl TriangleKernelBridge {
    fn new(
        device: Arc<HostVulkanDevice>,
        surfaces: Vec<(String, StreamTexture)>,
    ) -> Self {
        Self {
            device,
            surfaces: surfaces.into_iter().collect(),
            kernels: parking_lot::Mutex::new(HashMap::new()),
        }
    }

    fn canonical_kernel_id(decl: &GraphicsKernelRegisterDecl) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b"v=");
        h.update(&decl.vertex_spv);
        h.update(b"|f=");
        h.update(&decl.fragment_spv);
        h.update(b"|ve=");
        h.update(decl.vertex_entry_point.as_bytes());
        h.update(b"|fe=");
        h.update(decl.fragment_entry_point.as_bytes());
        h.update(b"|pc=");
        h.update(&decl.push_constant_size.to_le_bytes());
        h.update(b"|pcs=");
        h.update(&decl.push_constant_stages.to_le_bytes());
        h.update(b"|dsi=");
        h.update(&decl.descriptor_sets_in_flight.to_le_bytes());
        h.update(b"|nb=");
        h.update(&(decl.bindings.len() as u32).to_le_bytes());
        // Pipeline state — distil enums to enum discriminants to avoid
        // pulling in serde just for the canonicalization step.
        let p = &decl.pipeline_state;
        h.update(b"|t=");
        h.update(&(p.topology as u32).to_le_bytes());
        h.update(b"|cb=");
        h.update(&[p.color_blend_enabled as u8]);
        h.update(b"|cm=");
        h.update(&p.color_write_mask.to_le_bytes());
        h.update(b"|nf=");
        h.update(&(p.attachment_color_formats.len() as u32).to_le_bytes());
        for f in &p.attachment_color_formats {
            h.update(f.as_bytes());
            h.update(b"|");
        }
        format!("{:x}", h.finalize())
    }
}

fn map_topology(t: PrimitiveTopologyWire) -> PrimitiveTopology {
    match t {
        PrimitiveTopologyWire::PointList => PrimitiveTopology::PointList,
        PrimitiveTopologyWire::LineList => PrimitiveTopology::LineList,
        PrimitiveTopologyWire::LineStrip => PrimitiveTopology::LineStrip,
        PrimitiveTopologyWire::TriangleList => PrimitiveTopology::TriangleList,
        PrimitiveTopologyWire::TriangleStrip => PrimitiveTopology::TriangleStrip,
        PrimitiveTopologyWire::TriangleFan => PrimitiveTopology::TriangleFan,
    }
}

fn map_polygon_mode(m: PolygonModeWire) -> streamlib::core::rhi::PolygonMode {
    use streamlib::core::rhi::PolygonMode;
    match m {
        PolygonModeWire::Fill => PolygonMode::Fill,
        PolygonModeWire::Line => PolygonMode::Line,
        PolygonModeWire::Point => PolygonMode::Point,
    }
}

fn map_cull(m: CullModeWire) -> streamlib::core::rhi::CullMode {
    use streamlib::core::rhi::CullMode;
    match m {
        CullModeWire::None => CullMode::None,
        CullModeWire::Front => CullMode::Front,
        CullModeWire::Back => CullMode::Back,
        CullModeWire::FrontAndBack => CullMode::FrontAndBack,
    }
}

fn map_front_face(m: FrontFaceWire) -> streamlib::core::rhi::FrontFace {
    use streamlib::core::rhi::FrontFace;
    match m {
        FrontFaceWire::CounterClockwise => FrontFace::CounterClockwise,
        FrontFaceWire::Clockwise => FrontFace::Clockwise,
    }
}

fn map_blend_factor(f: BlendFactorWire) -> streamlib::core::rhi::BlendFactor {
    use streamlib::core::rhi::BlendFactor;
    match f {
        BlendFactorWire::Zero => BlendFactor::Zero,
        BlendFactorWire::One => BlendFactor::One,
        BlendFactorWire::SrcColor => BlendFactor::SrcColor,
        BlendFactorWire::OneMinusSrcColor => BlendFactor::OneMinusSrcColor,
        BlendFactorWire::DstColor => BlendFactor::DstColor,
        BlendFactorWire::OneMinusDstColor => BlendFactor::OneMinusDstColor,
        BlendFactorWire::SrcAlpha => BlendFactor::SrcAlpha,
        BlendFactorWire::OneMinusSrcAlpha => BlendFactor::OneMinusSrcAlpha,
        BlendFactorWire::DstAlpha => BlendFactor::DstAlpha,
        BlendFactorWire::OneMinusDstAlpha => BlendFactor::OneMinusDstAlpha,
        BlendFactorWire::ConstantColor => BlendFactor::ConstantColor,
        BlendFactorWire::OneMinusConstantColor => BlendFactor::OneMinusConstantColor,
        BlendFactorWire::ConstantAlpha => BlendFactor::ConstantAlpha,
        BlendFactorWire::OneMinusConstantAlpha => BlendFactor::OneMinusConstantAlpha,
        BlendFactorWire::SrcAlphaSaturate => BlendFactor::SrcAlphaSaturate,
    }
}

fn map_blend_op(o: BlendOpWire) -> streamlib::core::rhi::BlendOp {
    use streamlib::core::rhi::BlendOp;
    match o {
        BlendOpWire::Add => BlendOp::Add,
        BlendOpWire::Subtract => BlendOp::Subtract,
        BlendOpWire::ReverseSubtract => BlendOp::ReverseSubtract,
        BlendOpWire::Min => BlendOp::Min,
        BlendOpWire::Max => BlendOp::Max,
    }
}

fn parse_texture_format(s: &str) -> std::result::Result<TextureFormat, String> {
    match s {
        "rgba8_unorm" => Ok(TextureFormat::Rgba8Unorm),
        "rgba8_unorm_srgb" => Ok(TextureFormat::Rgba8UnormSrgb),
        "bgra8_unorm" => Ok(TextureFormat::Bgra8Unorm),
        "bgra8_unorm_srgb" => Ok(TextureFormat::Bgra8UnormSrgb),
        other => Err(format!("unknown attachment color format '{other}'")),
    }
}

fn build_pipeline_state(
    p: &GraphicsPipelineStateWire,
) -> std::result::Result<GraphicsPipelineState, String> {
    use streamlib::core::rhi::ColorBlendAttachment;

    if p.multisample_samples != 1 {
        return Err(format!(
            "MSAA samples != 1 not supported (got {})",
            p.multisample_samples
        ));
    }
    if p.attachment_color_formats.len() != 1 {
        return Err(format!(
            "expected exactly one attachment color format (got {})",
            p.attachment_color_formats.len()
        ));
    }
    if p.attachment_depth_format.is_some() || p.depth_stencil_enabled {
        return Err(
            "depth attachments are not supported in v1 of the bridge".into(),
        );
    }
    if !p.vertex_input_bindings.is_empty() || !p.vertex_input_attributes.is_empty() {
        // The triangle example uses gl_VertexIndex (no vertex buffers); a
        // production bridge can fold these in via VertexInputState::Buffers.
        return Err(
            "v1 example bridge supports only vertex-fabricating shaders \
             (gl_VertexIndex pattern) — empty vertex_input_bindings"
                .into(),
        );
    }

    let color_blend = if p.color_blend_enabled {
        let mask = ColorWriteMask::R
            | ColorWriteMask::G
            | ColorWriteMask::B
            | ColorWriteMask::A;
        ColorBlendState::Enabled(ColorBlendAttachment {
            src_color_blend_factor: map_blend_factor(p.color_blend_src_color_factor),
            dst_color_blend_factor: map_blend_factor(p.color_blend_dst_color_factor),
            color_blend_op: map_blend_op(p.color_blend_color_op),
            src_alpha_blend_factor: map_blend_factor(p.color_blend_src_alpha_factor),
            dst_alpha_blend_factor: map_blend_factor(p.color_blend_dst_alpha_factor),
            alpha_blend_op: map_blend_op(p.color_blend_alpha_op),
            // Wire mask is a flat u32; the host `ColorWriteMask` accepts
            // RGBA as a preset and any subset here would degrade visibility
            // for the triangle anyway.
            color_write_mask: if (p.color_write_mask & 0b1111) == 0b1111 {
                ColorWriteMask::RGBA
            } else {
                mask
            },
        })
    } else {
        ColorBlendState::Disabled {
            color_write_mask: ColorWriteMask::RGBA,
        }
    };

    Ok(GraphicsPipelineState {
        topology: map_topology(p.topology),
        vertex_input: VertexInputState::None,
        rasterization: RasterizationState {
            polygon_mode: map_polygon_mode(p.rasterization_polygon_mode),
            cull_mode: map_cull(p.rasterization_cull_mode),
            front_face: map_front_face(p.rasterization_front_face),
            line_width: p.rasterization_line_width,
        },
        multisample: MultisampleState { samples: 1 },
        depth_stencil: DepthStencilState::Disabled,
        color_blend,
        attachment_formats: AttachmentFormats {
            color: vec![parse_texture_format(&p.attachment_color_formats[0])?],
            depth: None,
        },
        dynamic_state: match p.dynamic_state {
            DynamicStateWire::None => GraphicsDynamicState::None,
            DynamicStateWire::ViewportScissor => GraphicsDynamicState::ViewportScissor,
        },
    })
}

fn build_bindings(decls: &[GraphicsBindingDecl]) -> Vec<GraphicsBindingSpec> {
    use streamlib::core::context::GraphicsBindingKindWire as W;
    use streamlib::core::rhi::GraphicsBindingKind;
    decls
        .iter()
        .map(|d| GraphicsBindingSpec {
            binding: d.binding,
            kind: match d.kind {
                W::SampledTexture => GraphicsBindingKind::SampledTexture,
                W::StorageBuffer => GraphicsBindingKind::StorageBuffer,
                W::UniformBuffer => GraphicsBindingKind::UniformBuffer,
                W::StorageImage => GraphicsBindingKind::StorageImage,
            },
            stages: GraphicsShaderStageFlags::from_bits_or_zero(d.stages),
        })
        .collect()
}

trait FlagsFromBits {
    fn from_bits_or_zero(bits: u32) -> GraphicsShaderStageFlags;
}

impl FlagsFromBits for GraphicsShaderStageFlags {
    fn from_bits_or_zero(bits: u32) -> GraphicsShaderStageFlags {
        // Hand-roll a flags constructor — `GraphicsShaderStageFlags` is a
        // newtype around u32 with no public from_bits, so reconstruct via
        // the public consts.
        let mut out = GraphicsShaderStageFlags::NONE;
        if bits & 0b01 != 0 {
            out |= GraphicsShaderStageFlags::VERTEX;
        }
        if bits & 0b10 != 0 {
            out |= GraphicsShaderStageFlags::FRAGMENT;
        }
        out
    }
}

impl GraphicsKernelBridge for TriangleKernelBridge {
    fn register(
        &self,
        decl: &GraphicsKernelRegisterDecl,
    ) -> std::result::Result<String, String> {
        let kernel_id = Self::canonical_kernel_id(decl);
        let mut kernels = self.kernels.lock();
        if !kernels.contains_key(&kernel_id) {
            let pipeline_state = build_pipeline_state(&decl.pipeline_state)?;
            let bindings = build_bindings(&decl.bindings);
            let stages = [
                GraphicsStage {
                    stage: GraphicsShaderStage::Vertex,
                    spv: &decl.vertex_spv,
                    entry_point: if decl.vertex_entry_point.is_empty() {
                        "main"
                    } else {
                        decl.vertex_entry_point.as_str()
                    },
                },
                GraphicsStage {
                    stage: GraphicsShaderStage::Fragment,
                    spv: &decl.fragment_spv,
                    entry_point: if decl.fragment_entry_point.is_empty() {
                        "main"
                    } else {
                        decl.fragment_entry_point.as_str()
                    },
                },
            ];
            let push_constants = if decl.push_constant_size == 0 {
                GraphicsPushConstants::NONE
            } else {
                GraphicsPushConstants {
                    size: decl.push_constant_size,
                    stages: GraphicsShaderStageFlags::from_bits_or_zero(
                        decl.push_constant_stages,
                    ),
                }
            };
            let descriptor = GraphicsKernelDescriptor {
                label: if decl.label.is_empty() {
                    "polyglot-triangle"
                } else {
                    decl.label.as_str()
                },
                stages: &stages,
                bindings: &bindings,
                push_constants,
                pipeline_state,
                descriptor_sets_in_flight: decl.descriptor_sets_in_flight,
            };
            let kernel = VulkanGraphicsKernel::new(&self.device, &descriptor)
                .map_err(|e| format!("VulkanGraphicsKernel::new: {e}"))?;
            kernels.insert(kernel_id.clone(), Arc::new(kernel));
        }
        Ok(kernel_id)
    }

    fn run_draw(
        &self,
        draw: &GraphicsKernelRunDraw,
    ) -> std::result::Result<(), String> {
        let kernel = self
            .kernels
            .lock()
            .get(&draw.kernel_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "kernel_id '{}' not registered with this bridge",
                    draw.kernel_id
                )
            })?;
        if draw.color_target_uuids.len() != 1 {
            return Err(format!(
                "v1 bridge requires exactly one color target (got {})",
                draw.color_target_uuids.len()
            ));
        }
        let target_uuid = &draw.color_target_uuids[0];
        let texture = self.surfaces.get(target_uuid).ok_or_else(|| {
            format!(
                "color_target_uuid '{target_uuid}' not registered with this bridge"
            )
        })?;
        if !draw.push_constants.is_empty() {
            kernel
                .set_push_constants(draw.frame_index, &draw.push_constants)
                .map_err(|e| format!("set_push_constants: {e}"))?;
        }
        let _ = (&draw.bindings, &draw.vertex_buffers, &draw.index_buffer);
        // The triangle scenario has no descriptor bindings, no vertex
        // buffers, and is non-indexed — so we don't need to forward any
        // of those paths to the kernel. A production bridge would loop
        // over `draw.bindings` and call `set_sampled_texture` /
        // `set_storage_buffer` etc., loop over `draw.vertex_buffers`
        // and call `set_vertex_buffer`, and forward `draw.index_buffer`
        // to `set_index_buffer` for indexed draws.
        let offscreen_draw = match draw.draw {
            streamlib::core::context::GraphicsDrawSpec::Draw {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
            } => OffscreenDraw::Draw(streamlib::core::rhi::DrawCall {
                vertex_count,
                instance_count,
                first_vertex,
                first_instance,
                viewport: None,
                scissor: None,
            }),
            streamlib::core::context::GraphicsDrawSpec::DrawIndexed {
                index_count,
                instance_count,
                first_index,
                vertex_offset,
                first_instance,
            } => OffscreenDraw::DrawIndexed(
                streamlib::core::rhi::DrawIndexedCall {
                    index_count,
                    instance_count,
                    first_index,
                    vertex_offset,
                    first_instance,
                    viewport: None,
                    scissor: None,
                },
            ),
        };
        kernel
            .offscreen_render(
                draw.frame_index,
                &[OffscreenColorTarget {
                    texture,
                    clear_color: Some([0.05, 0.05, 0.05, 1.0]),
                }],
                draw.extent,
                offscreen_draw,
            )
            .map_err(|e| format!("offscreen_render: {e}"))?;
        Ok(())
    }
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1);
    let mut runtime_kind = RuntimeKind::Python;
    let mut output_png = PathBuf::from("/tmp/vulkan-triangle.png");
    for a in args {
        if let Some(value) = a.strip_prefix("--runtime=") {
            runtime_kind = RuntimeKind::parse(value).map_err(StreamError::Configuration)?;
        } else if let Some(value) = a.strip_prefix("--output=") {
            output_png = PathBuf::from(value);
        }
    }

    println!("=== Polyglot Vulkan adapter graphics scenario (#656) ===");
    println!("Runtime:     {}", runtime_kind.as_str());
    println!(
        "Surface:     {SURFACE_SIZE}x{SURFACE_SIZE} RGBA8 (uuid {SCENARIO_SURFACE_UUID})"
    );
    println!(
        "SPIR-V:      vert={} bytes, frag={} bytes",
        TRIANGLE_VERT_SPV.len(),
        TRIANGLE_FRAG_SPV.len()
    );
    println!("Output PNG:  {}", output_png.display());
    println!();

    let runtime = StreamRuntime::new()?;

    let texture_slot: Arc<Mutex<Option<StreamTexture>>> = Arc::new(Mutex::new(None));
    let timeline_slot: Arc<Mutex<Option<Arc<HostVulkanTimelineSemaphore>>>> =
        Arc::new(Mutex::new(None));
    let readback_slot: Arc<Mutex<Option<Arc<VulkanTextureReadback>>>> =
        Arc::new(Mutex::new(None));

    {
        let texture_slot = Arc::clone(&texture_slot);
        let timeline_slot = Arc::clone(&timeline_slot);
        let readback_slot = Arc::clone(&readback_slot);
        runtime.install_setup_hook(move |gpu| {
            let texture = gpu.acquire_render_target_dma_buf_image(
                SURFACE_SIZE,
                SURFACE_SIZE,
                TextureFormat::Rgba8Unorm,
            )?;
            let host_device = Arc::clone(gpu.device().vulkan_device());
            let timeline = Arc::new(
                HostVulkanTimelineSemaphore::new_exportable(host_device.device(), 0)
                    .map_err(|e| {
                        StreamError::Configuration(format!(
                            "HostVulkanTimelineSemaphore::new_exportable: {e}"
                        ))
                    })?,
            );
            let store = gpu.surface_store().ok_or_else(|| {
                StreamError::Configuration(
                    "surface_store unavailable — host runtime built without \
                     a surface-share service (Linux subprocess flow requires it)"
                        .into(),
                )
            })?;
            // The graphics kernel's offscreen_render leaves the image in
            // COLOR_ATTACHMENT_OPTIMAL after the render pass. Declare
            // that as the post-release layout so any future Path 2
            // consumer's acquire_from_foreign sees a matching source
            // layout.
            store
                .register_texture(
                    SCENARIO_SURFACE_UUID,
                    &texture,
                    Some(timeline.as_ref()),
                    streamlib::core::rhi::VulkanLayout::COLOR_ATTACHMENT_OPTIMAL,
                )
                .map_err(|e| {
                    StreamError::Configuration(format!("register_texture: {e}"))
                })?;

            let bridge = Arc::new(TriangleKernelBridge::new(
                Arc::clone(&host_device),
                vec![(SCENARIO_SURFACE_UUID.to_string(), texture.clone())],
            ));
            gpu.set_graphics_kernel_bridge(bridge);

            let readback = gpu.create_texture_readback(&TextureReadbackDescriptor {
                label: "polyglot-vulkan-graphics/readback",
                format: TextureFormat::Rgba8Unorm,
                width: SURFACE_SIZE,
                height: SURFACE_SIZE,
            })?;

            *texture_slot.lock().unwrap() = Some(texture);
            *timeline_slot.lock().unwrap() = Some(timeline);
            *readback_slot.lock().unwrap() = Some(readback);
            println!(
                "✓ render-target DMA-BUF + timeline registered as '{}'",
                SCENARIO_SURFACE_UUID
            );
            Ok(())
        });
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    match runtime_kind {
        RuntimeKind::Python => {
            let slpkg_path = manifest_dir
                .join("python/polyglot-vulkan-graphics-0.1.0.slpkg");
            if !slpkg_path.exists() {
                return Err(StreamError::Configuration(format!(
                    "Package not found: {}\nRun: cargo run -p streamlib-cli -- pack examples/polyglot-vulkan-graphics/python",
                    slpkg_path.display()
                )));
            }
            runtime.load_package(&slpkg_path)?;
        }
        RuntimeKind::Deno => {
            let project_path = manifest_dir.join("deno");
            if !project_path.join("streamlib.yaml").exists() {
                return Err(StreamError::Configuration(format!(
                    "Deno project not found: {}",
                    project_path.display()
                )));
            }
            runtime.load_project(&project_path)?;
        }
    }

    let fixture_path = write_trigger_fixture()
        .map_err(StreamError::Configuration)?;
    let source = runtime.add_processor(BgraFileSourceProcessor::Processor::node(
        BgraFileSourceProcessor::Config {
            file_path: fixture_path
                .to_str()
                .ok_or_else(|| {
                    StreamError::Configuration(
                        "fixture path has non-utf8 component".into(),
                    )
                })?
                .to_string(),
            width: 4,
            height: 4,
            fps: 5,
            frame_count: 3,
        },
    ))?;
    println!("+ BgraFileSource: {source}");

    let variant: u32 = match runtime_kind {
        RuntimeKind::Python => 0,
        RuntimeKind::Deno => 1,
    };
    let graphics_config = serde_json::json!({
        "vulkan_surface_uuid": SCENARIO_SURFACE_UUID,
        "width": SURFACE_SIZE,
        "height": SURFACE_SIZE,
        "variant": variant,
        "vertex_spv_hex": bytes_to_hex(TRIANGLE_VERT_SPV),
        "fragment_spv_hex": bytes_to_hex(TRIANGLE_FRAG_SPV),
    });
    let graphics = runtime.add_processor(ProcessorSpec::new(
        runtime_kind.processor_name(),
        graphics_config,
    ))?;
    println!("+ Vulkan graphics processor: {graphics}");

    runtime.connect(
        OutputLinkPortRef::new(&source, "video"),
        InputLinkPortRef::new(&graphics, "video_in"),
    )?;
    println!(
        "\nPipeline: BgraFileSource → {} vulkan-graphics\n",
        runtime_kind.as_str()
    );

    println!("Starting pipeline...");
    runtime.start()?;
    std::thread::sleep(Duration::from_secs(4));
    println!("Stopping pipeline...");
    runtime.stop()?;

    println!("\nReading host surface back via Vulkan...");
    let texture = texture_slot
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| {
            StreamError::Runtime(
                "host texture slot is empty — setup hook never ran".into(),
            )
        })?;
    let readback = readback_slot
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| StreamError::Runtime("readback slot is empty".into()))?;
    let ticket = readback
        .submit(&texture, TextureSourceLayout::ColorAttachment)
        .map_err(|e| StreamError::Runtime(format!("readback submit: {e}")))?;
    let rgba = readback
        .wait_and_read(ticket, u64::MAX)
        .map_err(|e| StreamError::Runtime(format!("readback wait: {e}")))?
        .to_vec();
    write_png(&rgba, SURFACE_SIZE, SURFACE_SIZE, &output_png)?;
    println!("✓ Output PNG written: {}", output_png.display());

    Ok(())
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn write_trigger_fixture() -> std::result::Result<PathBuf, String> {
    use std::fs::File;
    use std::io::Write;

    let path = std::env::temp_dir().join("vulkan-graphics-trigger.bgra");
    let mut f = File::create(&path)
        .map_err(|e| format!("create {}: {e}", path.display()))?;
    f.write_all(&[0u8; 4 * 4 * 4 * 3])
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

fn write_png(
    rgba: &[u8],
    width: u32,
    height: u32,
    output: &std::path::Path,
) -> Result<()> {
    use std::fs::File;
    use std::io::BufWriter;

    let file = File::create(output).map_err(|e| {
        StreamError::Configuration(format!("create output PNG {}: {e}", output.display()))
    })?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| StreamError::Configuration(format!("PNG header: {e}")))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| StreamError::Configuration(format!("PNG body: {e}")))?;
    Ok(())
}
