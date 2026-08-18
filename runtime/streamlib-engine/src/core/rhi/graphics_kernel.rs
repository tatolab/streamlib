// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Public descriptor types for the graphics-kernel RHI abstraction.
//!
//! Mirrors [`super::compute_kernel`] for graphics-pipeline work — vertex and
//! fragment stages today, with the descriptor shape open to optional
//! geometry / tessellation / mesh / task stages without breaking changes.
//!
//! Pattern matches `VulkanComputeKernel`: declare the binding shape and
//! pipeline state once as data; the RHI reflects the SPIR-V at kernel
//! creation, validates the declaration matches, and from that point on the
//! caller binds resources by slot via simple typed setters.

use std::borrow::Cow;

use rspirv_reflect::DescriptorType as RDescriptorType;

use crate::core::Result;

use super::TextureFormat;
use super::kernel_binding_names::{
    KernelBindingUnderReconciliation, KernelShaderStageMask, KernelShaderStageSpirvModule,
    derive_staged_kernel_bindings_from_shader_reflection,
    reconcile_staged_kernel_binding_declarations,
};

/// Shader stages that contribute to a graphics pipeline.
///
/// Today the RHI ships vertex + fragment. Optional stages (geometry,
/// tessellation control / evaluation, mesh, task) are deliberately not
/// yet supported — the enum is open so adding them later does not break
/// the public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GraphicsShaderStage {
    Vertex,
    Fragment,
}

/// Bitflags for which shader stages a binding or push-constant range is
/// visible to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphicsShaderStageFlags(u32);

impl GraphicsShaderStageFlags {
    pub const NONE: Self = Self(0);
    pub const VERTEX: Self = Self(0b01);
    pub const FRAGMENT: Self = Self(0b10);
    pub const VERTEX_FRAGMENT: Self = Self(0b11);

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    /// The mask a raw bitmask names, or `None` if it names a bit no graphics
    /// stage owns.
    ///
    /// An unknown bit is refused rather than masked off: a caller that set it
    /// meant a stage, and dropping it silently would make the declaration
    /// assert less than it said.
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !Self::VERTEX_FRAGMENT.0 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }
}

impl std::ops::BitOr for GraphicsShaderStageFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for GraphicsShaderStageFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// One stage of a graphics pipeline: SPIR-V blob plus stage classification.
#[derive(Debug, Clone, Copy)]
pub struct GraphicsStage<'a> {
    pub stage: GraphicsShaderStage,
    pub spv: &'a [u8],
    /// Entry point name. Defaults to `"main"`.
    pub entry_point: &'a str,
}

impl<'a> GraphicsStage<'a> {
    pub const fn vertex(spv: &'a [u8]) -> Self {
        Self {
            stage: GraphicsShaderStage::Vertex,
            spv,
            entry_point: "main",
        }
    }

    pub const fn fragment(spv: &'a [u8]) -> Self {
        Self {
            stage: GraphicsShaderStage::Fragment,
            spv,
            entry_point: "main",
        }
    }
}

/// Resource kind bound at a particular slot in a graphics kernel's
/// descriptor set 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsBindingKind {
    SampledTexture,
    StorageBuffer,
    UniformBuffer,
    StorageImage,
}

/// One binding declaration: (binding index, resource kind, visible stages, the
/// shader's own name for the binding).
///
/// `name` is `None` on a declaration that asserts only slot, kind and stages,
/// and `Some` on every spec the RHI has reconciled against the shader: the
/// numeric binding is what the descriptor set is built from, the name is what a
/// by-name draw resolves against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsBindingSpec {
    pub binding: u32,
    pub kind: GraphicsBindingKind,
    pub stages: GraphicsShaderStageFlags,
    pub name: Option<Cow<'static, str>>,
}

/// What a caller asserts about one graphics binding before reflection has
/// assigned it a slot: the shader's name for it, the kind expected there, and
/// the stages the caller believes read it.
///
/// Deliberately not a [`GraphicsBindingSpec`]: a declaration has no slot, and
/// carrying a placeholder slot in a spec would put a meaningless number in a
/// field every resolved path treats as load-bearing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsBindingDeclaration {
    pub name: String,
    pub kind: GraphicsBindingKind,
    pub stages: GraphicsShaderStageFlags,
}

impl KernelShaderStageMask for GraphicsShaderStageFlags {
    fn mask_naming_no_stages() -> Self {
        Self::NONE
    }

    fn named_stages(self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.contains(Self::VERTEX) {
            names.push("vertex");
        }
        if self.contains(Self::FRAGMENT) {
            names.push("fragment");
        }
        names
    }

    fn names_no_stage(self) -> bool {
        self == Self::NONE
    }

    fn stages_missing_from(self, available: Self) -> Vec<&'static str> {
        Self(self.0 & !available.0).named_stages()
    }

    fn contains_every_stage_in(self, other: Self) -> bool {
        self.contains(other)
    }
}

/// Check a caller's graphics binding declarations against what the shaders'
/// reflection actually found, by name.
///
/// Runs at kernel construction, where the multi-stage declaration is built —
/// which is the only place a stage mismatch can be caught, because a draw
/// never revisits which stage reads what.
pub(crate) fn reconcile_graphics_binding_declarations(
    declared: &[GraphicsBindingDeclaration],
    reflected: &[GraphicsBindingSpec],
    stages_the_kernel_was_built_from: GraphicsShaderStageFlags,
) -> Result<()> {
    let declared_view: Vec<_> = declared
        .iter()
        .map(|declaration| KernelBindingUnderReconciliation {
            name: declaration.name.as_str(),
            kind: declaration.kind,
            stages: declaration.stages,
        })
        .collect();
    let reflected_view: Vec<_> = reflected
        .iter()
        .filter_map(|spec| {
            Some(KernelBindingUnderReconciliation {
                name: spec.name.as_deref()?,
                kind: spec.kind,
                stages: spec.stages,
            })
        })
        .collect();
    reconcile_staged_kernel_binding_declarations(
        "graphics",
        &declared_view,
        &reflected_view,
        stages_the_kernel_was_built_from,
    )
}

impl GraphicsBindingSpec {
    pub const fn sampled_texture(binding: u32, stages: GraphicsShaderStageFlags) -> Self {
        Self {
            binding,
            kind: GraphicsBindingKind::SampledTexture,
            stages,
            name: None,
        }
    }

    pub const fn storage_buffer(binding: u32, stages: GraphicsShaderStageFlags) -> Self {
        Self {
            binding,
            kind: GraphicsBindingKind::StorageBuffer,
            stages,
            name: None,
        }
    }

    pub const fn uniform_buffer(binding: u32, stages: GraphicsShaderStageFlags) -> Self {
        Self {
            binding,
            kind: GraphicsBindingKind::UniformBuffer,
            stages,
            name: None,
        }
    }

    pub const fn storage_image(binding: u32, stages: GraphicsShaderStageFlags) -> Self {
        Self {
            binding,
            kind: GraphicsBindingKind::StorageImage,
            stages,
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

/// Push-constant range declaration. Set `size = 0` to opt out.
#[derive(Debug, Clone, Copy)]
pub struct GraphicsPushConstants {
    pub size: u32,
    pub stages: GraphicsShaderStageFlags,
}

impl GraphicsPushConstants {
    pub const NONE: Self = Self {
        size: 0,
        stages: GraphicsShaderStageFlags::NONE,
    };
}

/// Primitive topology for assembled vertices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveTopology {
    PointList,
    LineList,
    LineStrip,
    TriangleList,
    TriangleStrip,
    TriangleFan,
}

/// Per-vertex format for a vertex attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexAttributeFormat {
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

/// Whether a vertex input binding advances per-vertex or per-instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexInputRate {
    Vertex,
    Instance,
}

/// One vertex buffer binding slot: stride and step rate.
#[derive(Debug, Clone, Copy)]
pub struct VertexInputBinding {
    pub binding: u32,
    pub stride: u32,
    pub input_rate: VertexInputRate,
}

/// One vertex attribute pulled from a binding: shader location, source binding,
/// element format, byte offset within the vertex.
#[derive(Debug, Clone, Copy)]
pub struct VertexInputAttribute {
    pub location: u32,
    pub binding: u32,
    pub format: VertexAttributeFormat,
    pub offset: u32,
}

/// Vertex input state. `None` means the vertex shader fabricates positions
/// from `gl_VertexIndex` / `gl_InstanceIndex` (fullscreen-triangle pattern).
#[derive(Debug, Clone)]
pub enum VertexInputState {
    None,
    Buffers {
        bindings: Vec<VertexInputBinding>,
        attributes: Vec<VertexInputAttribute>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolygonMode {
    Fill,
    Line,
    Point,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CullMode {
    None,
    Front,
    Back,
    FrontAndBack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontFace {
    CounterClockwise,
    Clockwise,
}

#[derive(Debug, Clone, Copy)]
pub struct RasterizationState {
    pub polygon_mode: PolygonMode,
    pub cull_mode: CullMode,
    pub front_face: FrontFace,
    pub line_width: f32,
}

impl Default for RasterizationState {
    fn default() -> Self {
        Self {
            polygon_mode: PolygonMode::Fill,
            cull_mode: CullMode::None,
            front_face: FrontFace::CounterClockwise,
            line_width: 1.0,
        }
    }
}

/// Multisample state. Only `samples = 1` is supported today (MSAA is a
/// follow-up).
#[derive(Debug, Clone, Copy)]
pub struct MultisampleState {
    pub samples: u32,
}

impl Default for MultisampleState {
    fn default() -> Self {
        Self { samples: 1 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthCompareOp {
    Never,
    Less,
    Equal,
    LessOrEqual,
    Greater,
    NotEqual,
    GreaterOrEqual,
    Always,
}

/// Depth/stencil test state. Stencil testing is not exposed today (no
/// in-tree consumer needs it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthStencilState {
    Disabled,
    Enabled {
        depth_test: DepthCompareOp,
        depth_write: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendFactor {
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
pub enum BlendOp {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

/// Color write-mask flags for a color attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorWriteMask(u32);

impl ColorWriteMask {
    pub const R: Self = Self(0b0001);
    pub const G: Self = Self(0b0010);
    pub const B: Self = Self(0b0100);
    pub const A: Self = Self(0b1000);
    pub const RGBA: Self = Self(0b1111);

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The mask a raw bitmask names, or `None` if it names a bit no color
    /// channel owns.
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !Self::RGBA.0 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }
}

impl std::ops::BitOr for ColorWriteMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ColorBlendAttachment {
    pub src_color_blend_factor: BlendFactor,
    pub dst_color_blend_factor: BlendFactor,
    pub color_blend_op: BlendOp,
    pub src_alpha_blend_factor: BlendFactor,
    pub dst_alpha_blend_factor: BlendFactor,
    pub alpha_blend_op: BlendOp,
    pub color_write_mask: ColorWriteMask,
}

impl ColorBlendAttachment {
    /// Standard alpha blend over the destination.
    pub const ALPHA_OVER: Self = Self {
        src_color_blend_factor: BlendFactor::SrcAlpha,
        dst_color_blend_factor: BlendFactor::OneMinusSrcAlpha,
        color_blend_op: BlendOp::Add,
        src_alpha_blend_factor: BlendFactor::One,
        dst_alpha_blend_factor: BlendFactor::OneMinusSrcAlpha,
        alpha_blend_op: BlendOp::Add,
        color_write_mask: ColorWriteMask::RGBA,
    };
}

/// Color blend state. Multi-attachment blending (MRT) is a follow-up; the
/// kernel today targets a single color attachment.
#[derive(Debug, Clone, Copy)]
pub enum ColorBlendState {
    Disabled { color_write_mask: ColorWriteMask },
    Enabled(ColorBlendAttachment),
}

impl Default for ColorBlendState {
    fn default() -> Self {
        Self::Disabled {
            color_write_mask: ColorWriteMask::RGBA,
        }
    }
}

/// Depth attachment format for graphics-pipeline depth testing.
///
/// Lives next to the graphics-kernel API (rather than in the public
/// `TextureFormat` enum) because depth/stencil formats only show up at
/// graphics-pipeline boundaries — `streamlib-consumer-rhi`'s
/// `TextureFormat` covers color-only formats, and depth `Texture`
/// allocation is a separate concern tracked as a follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepthFormat {
    D16Unorm,
    D32Sfloat,
    D24UnormS8Uint,
}

/// Color and depth attachment formats the graphics pipeline targets.
///
/// Used by Vulkan dynamic rendering (`VkPipelineRenderingCreateInfo`); the
/// formats must match the actual attachments at draw time.
#[derive(Debug, Clone)]
pub struct AttachmentFormats {
    pub color: Vec<TextureFormat>,
    pub depth: Option<DepthFormat>,
}

impl AttachmentFormats {
    /// Single color attachment, no depth.
    pub fn color_only(format: TextureFormat) -> Self {
        Self {
            color: vec![format],
            depth: None,
        }
    }
}

/// Which pipeline state is set dynamically per-draw vs baked into the
/// pipeline at creation.
///
/// `ViewportScissor` (the canonical render-loop choice) lets the same
/// pipeline serve every swapchain extent; `None` bakes a default 1x1
/// viewport into the pipeline, which is only correct for offscreen
/// fixed-size rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsDynamicState {
    None,
    ViewportScissor,
}

/// Complete fixed-function state plus attachment formats for a graphics
/// pipeline.
#[derive(Debug, Clone)]
pub struct GraphicsPipelineState {
    pub topology: PrimitiveTopology,
    pub vertex_input: VertexInputState,
    pub rasterization: RasterizationState,
    pub multisample: MultisampleState,
    pub depth_stencil: DepthStencilState,
    pub color_blend: ColorBlendState,
    pub attachment_formats: AttachmentFormats,
    pub dynamic_state: GraphicsDynamicState,
}

/// Graphics-kernel descriptor: stages + bindings + pipeline state +
/// descriptor-set ring depth.
#[derive(Debug, Clone)]
pub struct GraphicsKernelDescriptor<'a> {
    /// Human-readable label used in error messages and tracing.
    pub label: &'a str,
    /// One entry per shader stage (vertex + fragment minimum).
    pub stages: &'a [GraphicsStage<'a>],
    /// Binding declarations for descriptor set 0.
    pub bindings: &'a [GraphicsBindingSpec],
    /// Push-constant declaration. `GraphicsPushConstants::NONE` if unused.
    pub push_constants: GraphicsPushConstants,
    /// Fixed-function pipeline state + attachment formats.
    pub pipeline_state: GraphicsPipelineState,
    /// Number of descriptor sets in the ring.
    ///
    /// Render-loop callers pass `frame_index ∈ [0, descriptor_sets_in_flight)`
    /// to `set_*` and `cmd_bind_and_draw`. Must be ≥ 1.
    pub descriptor_sets_in_flight: u32,
}

/// Type of indices for indexed draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexType {
    Uint16,
    Uint32,
}

/// Viewport rectangle for `cmd_bind_and_draw*` when the pipeline declares
/// dynamic viewport state.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub min_depth: f32,
    pub max_depth: f32,
}

impl Viewport {
    /// Convenience: full-extent viewport with depth range \[0, 1\].
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: width as f32,
            height: height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ScissorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl ScissorRect {
    pub fn full(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }
}

/// One non-indexed draw call.
#[derive(Debug, Clone, Copy)]
pub struct DrawCall {
    pub vertex_count: u32,
    pub instance_count: u32,
    pub first_vertex: u32,
    pub first_instance: u32,
    pub viewport: Option<Viewport>,
    pub scissor: Option<ScissorRect>,
}

/// One indexed draw call. Caller must have set an index buffer via
/// `set_index_buffer`.
#[derive(Debug, Clone, Copy)]
pub struct DrawIndexedCall {
    pub index_count: u32,
    pub instance_count: u32,
    pub first_index: u32,
    pub vertex_offset: i32,
    pub first_instance: u32,
    pub viewport: Option<Viewport>,
    pub scissor: Option<ScissorRect>,
}

/// Reflect a multi-stage SPIR-V set and return the merged binding declaration
/// + total push-constant size + visible stages per binding.
///
/// Used by the host to derive a descriptor shape from raw shaders without
/// the caller restating it. Each stage's reflection is unioned: a binding
/// declared by both vertex and fragment is reported once with stages
/// `VERTEX | FRAGMENT`.
///
/// Rejects descriptor-type conflicts (same binding declared as
/// StorageBuffer in vertex and UniformBuffer in fragment) and any descriptor
/// set other than set 0.
///
/// Every derived spec carries the shader's own name for its binding, and the
/// two ways a name can fail to identify one binding are rejected here rather
/// than at draw time: a slot the two stages spell differently, and one name on
/// two slots. A blob whose `OpName` decorations were stripped is rejected for
/// the same reason — bindings are resolved by name in one spelling for both
/// languages, so an unnamed binding cannot be bound at all.
pub fn derive_bindings_from_spirv_multistage(
    stages: &[GraphicsStage<'_>],
) -> Result<(Vec<GraphicsBindingSpec>, GraphicsPushConstants)> {
    let stage_modules: Vec<_> = stages
        .iter()
        .map(|stage| KernelShaderStageSpirvModule {
            stage: stage.stage,
            spirv: stage.spv,
        })
        .collect();
    let (derived, push_constants) = derive_staged_kernel_bindings_from_shader_reflection(
        "Graphics kernel",
        &stage_modules,
        stage_to_flag,
        spirv_type_to_kind,
    )?;
    Ok((
        derived
            .into_iter()
            .map(|binding| GraphicsBindingSpec {
                binding: binding.binding,
                kind: binding.kind,
                stages: binding.stages,
                name: Some(Cow::Owned(binding.name)),
            })
            .collect(),
        GraphicsPushConstants {
            size: push_constants.size,
            stages: push_constants.stages,
        },
    ))
}

fn stage_to_flag(stage: GraphicsShaderStage) -> GraphicsShaderStageFlags {
    match stage {
        GraphicsShaderStage::Vertex => GraphicsShaderStageFlags::VERTEX,
        GraphicsShaderStage::Fragment => GraphicsShaderStageFlags::FRAGMENT,
    }
}

fn spirv_type_to_kind(ty: RDescriptorType) -> Option<GraphicsBindingKind> {
    match ty {
        RDescriptorType::STORAGE_BUFFER => Some(GraphicsBindingKind::StorageBuffer),
        RDescriptorType::UNIFORM_BUFFER => Some(GraphicsBindingKind::UniformBuffer),
        RDescriptorType::COMBINED_IMAGE_SAMPLER => Some(GraphicsBindingKind::SampledTexture),
        RDescriptorType::STORAGE_IMAGE => Some(GraphicsBindingKind::StorageImage),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::spirv_module_rewriting_for_tests::{
        move_binding_to_another_descriptor_set_in_spirv_module,
        move_binding_to_another_slot_in_spirv_module, rename_binding_in_spirv_module,
        strip_every_debug_name_from_spirv_module,
    };

    // Reflection is host-side work on bytes the build already produced, so
    // every test here runs without a Vulkan device.
    fn display_blit_vertex_spirv() -> &'static [u8] {
        include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.vert.spv"))
    }

    fn display_blit_fragment_spirv() -> &'static [u8] {
        include_bytes!(concat!(env!("OUT_DIR"), "/display_blit.frag.spv"))
    }

    #[test]
    fn reflection_keeps_the_shaders_own_name_for_every_binding() {
        let stages = [
            GraphicsStage::vertex(display_blit_vertex_spirv()),
            GraphicsStage::fragment(display_blit_fragment_spirv()),
        ];
        let (bindings, _) = derive_bindings_from_spirv_multistage(&stages).expect("derive");
        let named: Vec<(u32, &str)> = bindings
            .iter()
            .map(|spec| {
                (
                    spec.binding,
                    spec.name.as_deref().expect("every binding is named"),
                )
            })
            .collect();
        assert_eq!(named, vec![(0, "cameraTexture")]);
    }

    #[test]
    fn a_name_stripped_stage_is_refused_at_derive() {
        let stripped = strip_every_debug_name_from_spirv_module(display_blit_fragment_spirv());
        let stages = [
            GraphicsStage::vertex(display_blit_vertex_spirv()),
            GraphicsStage::fragment(&stripped),
        ];
        let refusal = derive_bindings_from_spirv_multistage(&stages)
            .expect_err("a name-stripped blob cannot be bound by name");
        let message = refusal.to_string();
        assert!(
            message.contains("carries no name") && message.contains("glslc -g"),
            "the refusal must name the cause and the fix: {message}"
        );
    }

    #[test]
    fn one_slot_spelled_two_ways_is_refused_at_derive() {
        // display_blit's vertex stage declares no binding, so the second
        // spelling of slot 0 has to come from a rewrite of the fragment blob.
        let respelled =
            rename_binding_in_spirv_module(display_blit_fragment_spirv(), "cameraTexture", "cam");
        let stages = [
            GraphicsStage::vertex(&respelled),
            GraphicsStage::fragment(display_blit_fragment_spirv()),
        ];
        let refusal = derive_bindings_from_spirv_multistage(&stages)
            .expect_err("one slot cannot carry two names");
        let message = refusal.to_string();
        assert!(
            message.contains("binding 0 is named `cam`")
                && message.contains("`cameraTexture`")
                && message.contains("one slot spelled two ways"),
            "the refusal must name the slot and both spellings: {message}"
        );
    }

    #[test]
    fn one_name_on_two_slots_is_refused_at_derive() {
        let moved =
            move_binding_to_another_slot_in_spirv_module(display_blit_fragment_spirv(), 0, 1);
        let stages = [
            GraphicsStage::vertex(&moved),
            GraphicsStage::fragment(display_blit_fragment_spirv()),
        ];
        let refusal = derive_bindings_from_spirv_multistage(&stages)
            .expect_err("one name cannot identify two slots");
        let message = refusal.to_string();
        assert!(
            message.contains("bindings 0 and 1 are both named `cameraTexture`")
                && message.contains("one name on two slots"),
            "the refusal must name both slots and the name: {message}"
        );
    }

    #[test]
    fn a_stage_whose_only_set_is_not_set_0_is_refused_at_derive() {
        let moved_to_set_1 = move_binding_to_another_descriptor_set_in_spirv_module(
            display_blit_fragment_spirv(),
            0,
            1,
        );
        let stages = [
            GraphicsStage::vertex(display_blit_vertex_spirv()),
            GraphicsStage::fragment(&moved_to_set_1),
        ];
        let message = match derive_bindings_from_spirv_multistage(&stages) {
            Ok((derived, _)) => panic!(
                "a binding outside set 0 cannot be bound, so it cannot be dropped in silence; \
                 derive returned {} binding(s): {:?}",
                derived.len(),
                derived
                    .iter()
                    .map(|spec| spec.name.as_deref())
                    .collect::<Vec<_>>()
            ),
            Err(refusal) => refusal.to_string(),
        };
        assert!(
            message.contains("only descriptor set 0 is supported") && message.contains('1'),
            "the refusal must name the unsupported set: {message}"
        );
    }
}
