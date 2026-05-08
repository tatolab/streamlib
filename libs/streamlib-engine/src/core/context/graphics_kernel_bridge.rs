// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host-side dispatch trait the escalate handler uses to drive graphics
//! kernel registration and per-draw invocation on behalf of subprocess
//! customers.
//!
//! Mirrors the [`super::compute_kernel_bridge::ComputeKernelBridge`]
//! shape (#550): the subprocess sends a typed IPC, the host runs
//! privileged Vulkan work via its [`crate::core::context::GpuContextFullAccess`],
//! and the bridge keeps the FullAccess capability boundary on the host
//! side of the IPC seam.
//!
//! Graphics is register-once-draw-many: the subprocess sends the
//! vertex + fragment SPIR-V plus the full pipeline state once; the
//! host reflects bindings + builds the
//! [`crate::vulkan::rhi::VulkanGraphicsKernel`] (with on-disk pipeline
//! cache persistence — same `STREAMLIB_PIPELINE_CACHE_DIR` knob as
//! compute), and caches it keyed by SHA-256 of a canonical
//! description blob. Subsequent `run_draw` calls reference the cached
//! kernel by handle.
//!
//! Subprocesses cannot pass `vk::CommandBuffer` across IPC, so the
//! bridge's `run_draw` always renders one offscreen pass into the
//! caller-provided color targets and submits + waits on the kernel's
//! own command buffer + fence. This matches
//! [`crate::vulkan::rhi::VulkanGraphicsKernel::offscreen_render`].
//!
//! The trait lives here (in `streamlib`) because the escalate IPC
//! handler is here. Implementations live in application setup glue
//! (or in `streamlib-adapter-vulkan` test utilities) — those can
//! depend on `streamlib`; the reverse cannot. Register an impl via
//! [`crate::core::context::GpuContext::set_graphics_kernel_bridge`]
//! before spawning subprocesses that issue
//! `register_graphics_kernel` / `run_graphics_draw`.

#![cfg(target_os = "linux")]

/// Resource kind for a binding slot in the graphics kernel's
/// descriptor set 0. Wire-format mirror of
/// [`crate::core::rhi::GraphicsBindingKind`] decoupled from the
/// generated JTD types so the bridge surface is stable across schema
/// regenerations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsBindingKindWire {
    SampledTexture,
    StorageBuffer,
    UniformBuffer,
    StorageImage,
}

/// One binding declaration for register-time validation against
/// SPIR-V reflection.
#[derive(Debug, Clone, Copy)]
pub struct GraphicsBindingDecl {
    pub binding: u32,
    pub kind: GraphicsBindingKindWire,
    /// Stage-visibility bitmask: `1 = VERTEX`, `2 = FRAGMENT`.
    pub stages: u32,
}

/// Per-vertex attribute format. Mirrors
/// [`crate::core::rhi::VertexAttributeFormat`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexAttributeFormatWire {
    R32Float,
    Rg32Float,
    Rgb32Float,
    Rgba32Float,
    R32Uint,
    Rg32Uint,
    Rgb32Uint,
    Rgba32Uint,
    R32Sint,
    Rg32Sint,
    Rgb32Sint,
    Rgba32Sint,
    Rgba8Unorm,
    Rgba8Snorm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexInputRateWire {
    Vertex,
    Instance,
}

#[derive(Debug, Clone, Copy)]
pub struct VertexInputBindingDecl {
    pub binding: u32,
    pub stride: u32,
    pub input_rate: VertexInputRateWire,
}

#[derive(Debug, Clone, Copy)]
pub struct VertexInputAttributeDecl {
    pub location: u32,
    pub binding: u32,
    pub format: VertexAttributeFormatWire,
    pub offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveTopologyWire {
    PointList,
    LineList,
    LineStrip,
    TriangleList,
    TriangleStrip,
    TriangleFan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolygonModeWire {
    Fill,
    Line,
    Point,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CullModeWire {
    None,
    Front,
    Back,
    FrontAndBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontFaceWire {
    CounterClockwise,
    Clockwise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthCompareOpWire {
    Never,
    Less,
    Equal,
    LessOrEqual,
    Greater,
    NotEqual,
    GreaterOrEqual,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendFactorWire {
    Zero,
    One,
    SrcColor,
    OneMinusSrcColor,
    DstColor,
    OneMinusDstColor,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstAlpha,
    OneMinusDstAlpha,
    ConstantColor,
    OneMinusConstantColor,
    ConstantAlpha,
    OneMinusConstantAlpha,
    SrcAlphaSaturate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendOpWire {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthFormatWire {
    D16Unorm,
    D32Sfloat,
    D24UnormS8Uint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DynamicStateWire {
    None,
    ViewportScissor,
}

/// Pipeline-state mirror used by the bridge `register` call. Owned (no
/// borrows) so the bridge can stash it in its kernel cache.
#[derive(Debug, Clone)]
pub struct GraphicsPipelineStateWire {
    pub topology: PrimitiveTopologyWire,
    pub vertex_input_bindings: Vec<VertexInputBindingDecl>,
    pub vertex_input_attributes: Vec<VertexInputAttributeDecl>,
    pub rasterization_polygon_mode: PolygonModeWire,
    pub rasterization_cull_mode: CullModeWire,
    pub rasterization_front_face: FrontFaceWire,
    pub rasterization_line_width: f32,
    pub multisample_samples: u32,
    pub depth_stencil_enabled: bool,
    pub depth_compare_op: DepthCompareOpWire,
    pub depth_write: bool,
    pub color_blend_enabled: bool,
    /// Color write-mask bits — `1=R`, `2=G`, `4=B`, `8=A`.
    pub color_write_mask: u32,
    pub color_blend_src_color_factor: BlendFactorWire,
    pub color_blend_dst_color_factor: BlendFactorWire,
    pub color_blend_color_op: BlendOpWire,
    pub color_blend_src_alpha_factor: BlendFactorWire,
    pub color_blend_dst_alpha_factor: BlendFactorWire,
    pub color_blend_alpha_op: BlendOpWire,
    /// Color attachment formats — wire-format strings (`"bgra8_unorm"`,
    /// `"rgba8_unorm"`, …). The bridge translates these to
    /// [`crate::core::rhi::TextureFormat`] before construction.
    pub attachment_color_formats: Vec<String>,
    pub attachment_depth_format: Option<DepthFormatWire>,
    pub dynamic_state: DynamicStateWire,
}

/// Full register-time descriptor passed to
/// [`GraphicsKernelBridge::register`]. Owned mirror of the wire shape.
#[derive(Debug, Clone)]
pub struct GraphicsKernelRegisterDecl {
    pub label: String,
    pub vertex_spv: Vec<u8>,
    pub fragment_spv: Vec<u8>,
    pub vertex_entry_point: String,
    pub fragment_entry_point: String,
    pub bindings: Vec<GraphicsBindingDecl>,
    pub push_constant_size: u32,
    pub push_constant_stages: u32,
    pub descriptor_sets_in_flight: u32,
    pub pipeline_state: GraphicsPipelineStateWire,
}

/// Per-draw binding value.
#[derive(Debug, Clone)]
pub struct GraphicsBindingValue {
    pub binding: u32,
    pub kind: GraphicsBindingKindWire,
    pub surface_uuid: String,
}

#[derive(Debug, Clone)]
pub struct GraphicsVertexBufferBinding {
    pub binding: u32,
    pub surface_uuid: String,
    pub offset: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexTypeWire {
    Uint16,
    Uint32,
}

#[derive(Debug, Clone)]
pub struct GraphicsIndexBufferBinding {
    pub surface_uuid: String,
    pub offset: u64,
    pub index_type: IndexTypeWire,
}

#[derive(Debug, Clone, Copy)]
pub struct ViewportWire {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct ScissorRectWire {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Draw-call discriminator + parameters. Indexed and non-indexed share
/// the wire shape; the host bridge dispatches the right
/// `vkCmdDraw` / `vkCmdDrawIndexed` based on `kind`.
#[derive(Debug, Clone, Copy)]
pub enum GraphicsDrawSpec {
    Draw {
        vertex_count: u32,
        instance_count: u32,
        first_vertex: u32,
        first_instance: u32,
    },
    DrawIndexed {
        index_count: u32,
        instance_count: u32,
        first_index: u32,
        vertex_offset: i32,
        first_instance: u32,
    },
}

/// Full per-draw input passed to [`GraphicsKernelBridge::run_draw`].
#[derive(Debug, Clone)]
pub struct GraphicsKernelRunDraw {
    pub kernel_id: String,
    pub frame_index: u32,
    pub bindings: Vec<GraphicsBindingValue>,
    pub vertex_buffers: Vec<GraphicsVertexBufferBinding>,
    pub index_buffer: Option<GraphicsIndexBufferBinding>,
    pub color_target_uuids: Vec<String>,
    pub depth_target_uuid: Option<String>,
    pub extent: (u32, u32),
    pub push_constants: Vec<u8>,
    pub viewport: Option<ViewportWire>,
    pub scissor: Option<ScissorRectWire>,
    pub draw: GraphicsDrawSpec,
}

/// Dispatch trait the host runtime uses to drive graphics kernel
/// registration and per-draw invocation for subprocess customers.
///
/// Graphics dispatch on the host is synchronous: the bridge's
/// `run_draw` blocks on the kernel's fence inside
/// [`crate::vulkan::rhi::VulkanGraphicsKernel::offscreen_render`]
/// before returning, so by the time this returns, the GPU work has
/// retired and the host's writes to the color attachments are visible
/// to any subsequent submission against the same VkDevice. The
/// subprocess can safely advance its surface-share timeline on
/// receipt of the `ok` response.
pub trait GraphicsKernelBridge: Send + Sync {
    /// Register a graphics kernel. Returns a stable `kernel_id` —
    /// re-registering an identical descriptor (same SPIR-V, same
    /// pipeline state, same bindings) hits the host-side cache and
    /// returns the same id without re-reflecting or rebuilding the
    /// pipeline.
    ///
    /// The `kernel_id` shape is **implementation-defined** — the
    /// escalate handler treats it as an opaque string and the
    /// subprocess uses it only as a `run_draw` reference, so
    /// implementations are free to canonicalize whichever subset of
    /// `decl` makes sense for their cache shape. The recommended
    /// pattern is SHA-256 hex over a canonical byte representation
    /// of the inputs that *materially* determine the host-side
    /// `VulkanGraphicsKernel` (mirroring compute's SHA-256(spv)
    /// approach but extended to stage SPIR-V + pipeline state).
    /// Identical descriptors must collide on `kernel_id` for the
    /// register-cache to work; differing descriptors should not
    /// collide, but the bridge — not the trait — owns that contract.
    fn register(
        &self,
        decl: &GraphicsKernelRegisterDecl,
    ) -> Result<String, String>;

    /// Run one draw against a previously-registered kernel.
    ///
    /// Resolves binding `surface_uuid`s through the application-
    /// provided UUID → resource map, then submits + waits on the
    /// kernel's own command buffer + fence (offscreen-render shape).
    /// Errors include unrecognized `kernel_id`, surface lookup
    /// failure, push-constant size mismatch, and Vulkan submit
    /// failure.
    fn run_draw(
        &self,
        draw: &GraphicsKernelRunDraw,
    ) -> Result<(), String>;
}
