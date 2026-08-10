// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Golden wire vectors for the escalate IPC protocol.
//!
//! The helper side builds these documents as plain Python dicts, so serde's
//! encoding of these types is the entire wire contract — these bytes are that
//! contract, and a diff here is a break for every helper already deployed.
//! Each vector is a fully-populated message: every optional present, every
//! nested structure recursed. Two further tests cover what a populated
//! document cannot — which absent optionals drop out of the encoding, and
//! which carry an explicit null.

use super::escalate_request::{
    EscalateRequestLogLevel, EscalateRequestLogSource,
    EscalateRequestRegisterGraphicsKernelBindingKind,
    EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor,
    EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp,
    EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace,
    EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode,
    EscalateRequestRegisterGraphicsKernelPipelineStateTopology,
    EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat,
    EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate,
    EscalateRequestRegisterRayTracingKernelBindingKind,
    EscalateRequestRegisterRayTracingKernelGroupKind,
    EscalateRequestRegisterRayTracingKernelStageStage, EscalateRequestRunCpuReadbackCopyDirection,
    EscalateRequestRunGraphicsDrawBindingKind, EscalateRequestRunGraphicsDrawDrawKind,
    EscalateRequestRunGraphicsDrawIndexBufferIndexType,
    EscalateRequestRunRayTracingKernelBindingKind, EscalateRequestTryRunCpuReadbackCopyDirection,
};
use super::escalate_response::EscalateResponseOk;
use super::{EscalateRequest, EscalateResponse};

/// Decode each golden, require it to land in the variant it names, and require
/// the bytes back unchanged.
///
/// The variant is asserted, not merely labelled: two variants whose payloads
/// carry identical fields — run vs try CPU readback — round-trip symmetrically,
/// so re-encoding alone cannot tell them apart if their wire tags are ever
/// swapped. That swap is a dispatch break, not a wire break.
macro_rules! assert_golden_vectors_round_trip {
    ($enum_type:ident: $($variant:ident => $golden:literal),+ $(,)?) => {
        $({
            let decoded: $enum_type = serde_json::from_str($golden).unwrap_or_else(|e| {
                panic!(concat!(stringify!($variant), ": golden failed to decode: {}"), e)
            });
            assert!(
                matches!(decoded, $enum_type::$variant(_)),
                concat!(stringify!($variant), ": golden decoded into a different variant")
            );
            let reencoded = serde_json::to_string(&decoded).unwrap_or_else(|e| {
                panic!(concat!(stringify!($variant), ": re-encode failed: {}"), e)
            });
            assert_eq!(
                reencoded, $golden,
                concat!(stringify!($variant), ": wire encoding drifted from the golden vector")
            );
        })+
    };
}

/// Assert each named variant serialises to its wire spelling.
macro_rules! assert_wire_spellings {
    ($($enum_type:ident { $($variant:ident => $wire:literal),+ $(,)? })+) => {
        $($(
            assert_eq!(
                serde_json::to_string(&$enum_type::$variant).unwrap(),
                concat!("\"", $wire, "\"")
            );
        )+)+
    };
}

/// Every `EscalateRequest` variant survives a decode/encode round trip unchanged.
#[test]
fn escalate_request_vectors_round_trip() {
    assert_golden_vectors_round_trip!(EscalateRequest:
        AcquireImage => r#"{"op":"acquire_image","format":"acquire_image.format-1","height":2,"request_id":"acquire_image.request_id-3","width":4}"#,
        AcquirePixelBuffer => r#"{"op":"acquire_pixel_buffer","format":"acquire_pixel_buffer.format-5","height":6,"request_id":"acquire_pixel_buffer.request_id-7","width":8}"#,
        AcquireTexture => r#"{"op":"acquire_texture","format":"acquire_texture.format-9","height":10,"request_id":"acquire_texture.request_id-11","usage":["acquire_texture.usage[0]-12","acquire_texture.usage[1]-13"],"width":14}"#,
        CopyDeviceExportStagingBackToSurface => r#"{"op":"copy_device_export_staging_back_to_surface","request_id":"copy_device_export_staging_back_to_surface.request_id-15","surface_id":"copy_device_export_staging_back_to_surface.surface_id-16"}"#,
        Log => r#"{"op":"log","attrs":{"attr":"log.attrs.attr-17"},"channel":"log.channel-18","intercepted":false,"level":"debug","message":"log.message-21","pipeline_id":"log.pipeline_id-22","processor_id":"log.processor_id-23","source":"python","source_seq":"log.source_seq-25","source_ts":"log.source_ts-26"}"#,
        OpenDeviceExportStaging => r#"{"op":"open_device_export_staging","request_id":"open_device_export_staging.request_id-27","surface_id":"open_device_export_staging.surface_id-28"}"#,
        RefillDeviceExportStaging => r#"{"op":"refill_device_export_staging","request_id":"refill_device_export_staging.request_id-29","surface_id":"refill_device_export_staging.surface_id-30"}"#,
        RegisterAccelerationStructureBlas => r#"{"op":"register_acceleration_structure_blas","indices_hex":"register_acceleration_structure_blas.indices_hex-31","label":"register_acceleration_structure_blas.label-32","request_id":"register_acceleration_structure_blas.request_id-33","vertices_hex":"register_acceleration_structure_blas.vertices_hex-34"}"#,
        RegisterAccelerationStructureTlas => r#"{"op":"register_acceleration_structure_tlas","instances":[{"blas_id":"register_acceleration_structure_tlas.instances[0].blas_id-35","custom_index":36,"flags":37,"mask":38,"sbt_record_offset":39,"transform":[40.5,41.5]},{"blas_id":"register_acceleration_structure_tlas.instances[1].blas_id-42","custom_index":43,"flags":44,"mask":45,"sbt_record_offset":46,"transform":[47.5,48.5]}],"label":"register_acceleration_structure_tlas.label-49","request_id":"register_acceleration_structure_tlas.request_id-50"}"#,
        RegisterComputeKernel => r#"{"op":"register_compute_kernel","push_constant_size":51,"request_id":"register_compute_kernel.request_id-52","spv_hex":"register_compute_kernel.spv_hex-53"}"#,
        RegisterGraphicsKernel => r#"{"op":"register_graphics_kernel","bindings":[{"binding":54,"kind":"uniform_buffer","stages":56},{"binding":57,"kind":"storage_image","stages":59}],"descriptor_sets_in_flight":60,"fragment_entry_point":"register_graphics_kernel.fragment_entry_point-61","fragment_spv_hex":"register_graphics_kernel.fragment_spv_hex-62","label":"register_graphics_kernel.label-63","pipeline_state":{"attachment_color_formats":["register_graphics_kernel.pipeline_state.attachment_color_formats[0]-64","register_graphics_kernel.pipeline_state.attachment_color_formats[1]-65"],"color_blend_alpha_op":"max","color_blend_color_op":"min","color_blend_dst_alpha_factor":"one_minus_dst_color","color_blend_dst_color_factor":"one_minus_src_alpha","color_blend_enabled":true,"color_blend_src_alpha_factor":"src_alpha","color_blend_src_color_factor":"src_alpha_saturate","color_write_mask":73,"depth_compare_op":"greater","depth_stencil_enabled":false,"depth_write":true,"dynamic_state":"viewport_scissor","multisample_samples":78,"rasterization_cull_mode":"none","rasterization_front_face":"clockwise","rasterization_line_width":81.5,"rasterization_polygon_mode":"line","topology":"triangle_strip","vertex_input_attributes":[{"binding":84,"format":"r32_sint","location":86,"offset":87},{"binding":88,"format":"rg32_uint","location":90,"offset":91}],"vertex_input_bindings":[{"binding":92,"input_rate":"vertex","stride":94},{"binding":95,"input_rate":"instance","stride":97}],"attachment_depth_format":"d32_sfloat"},"push_constant_size":99,"push_constant_stages":100,"request_id":"register_graphics_kernel.request_id-101","vertex_entry_point":"register_graphics_kernel.vertex_entry_point-102","vertex_spv_hex":"register_graphics_kernel.vertex_spv_hex-103"}"#,
        RegisterRayTracingKernel => r#"{"op":"register_ray_tracing_kernel","bindings":[{"binding":104,"kind":"acceleration_structure","stages":106},{"binding":107,"kind":"storage_image","stages":109}],"groups":[{"any_hit_stage":110,"closest_hit_stage":111,"general_stage":112,"intersection_stage":113,"kind":"general"},{"any_hit_stage":115,"closest_hit_stage":116,"general_stage":117,"intersection_stage":118,"kind":"triangles_hit"}],"label":"register_ray_tracing_kernel.label-120","max_recursion_depth":121,"push_constant_size":122,"push_constant_stages":123,"request_id":"register_ray_tracing_kernel.request_id-124","stages":[{"entry_point":"register_ray_tracing_kernel.stages[0].entry_point-125","spv_hex":"register_ray_tracing_kernel.stages[0].spv_hex-126","stage":"callable"},{"entry_point":"register_ray_tracing_kernel.stages[1].entry_point-128","spv_hex":"register_ray_tracing_kernel.stages[1].spv_hex-129","stage":"miss"}]}"#,
        ReleaseHandle => r#"{"op":"release_handle","handle_id":"release_handle.handle_id-131","request_id":"release_handle.request_id-132"}"#,
        RunComputeKernel => r#"{"op":"run_compute_kernel","group_count_x":133,"group_count_y":134,"group_count_z":135,"kernel_id":"run_compute_kernel.kernel_id-136","push_constants_hex":"run_compute_kernel.push_constants_hex-137","request_id":"run_compute_kernel.request_id-138","surface_uuid":"run_compute_kernel.surface_uuid-139"}"#,
        RunCpuReadbackCopy => r#"{"op":"run_cpu_readback_copy","direction":"buffer_to_image","request_id":"run_cpu_readback_copy.request_id-141","surface_id":"run_cpu_readback_copy.surface_id-142"}"#,
        RunGraphicsDraw => r#"{"op":"run_graphics_draw","bindings":[{"binding":143,"kind":"sampled_texture","surface_uuid":"run_graphics_draw.bindings[0].surface_uuid-145"},{"binding":146,"kind":"uniform_buffer","surface_uuid":"run_graphics_draw.bindings[1].surface_uuid-148"}],"color_target_uuids":["run_graphics_draw.color_target_uuids[0]-149","run_graphics_draw.color_target_uuids[1]-150"],"draw":{"first_index":151,"first_instance":152,"first_vertex":153,"index_count":154,"instance_count":155,"kind":"draw","vertex_count":157,"vertex_offset":158},"extent_height":159,"extent_width":160,"frame_index":161,"kernel_id":"run_graphics_draw.kernel_id-162","push_constants_hex":"run_graphics_draw.push_constants_hex-163","request_id":"run_graphics_draw.request_id-164","vertex_buffers":[{"binding":165,"offset":"run_graphics_draw.vertex_buffers[0].offset-166","surface_uuid":"run_graphics_draw.vertex_buffers[0].surface_uuid-167"},{"binding":168,"offset":"run_graphics_draw.vertex_buffers[1].offset-169","surface_uuid":"run_graphics_draw.vertex_buffers[1].surface_uuid-170"}],"depth_target_uuid":"run_graphics_draw.depth_target_uuid-171","index_buffer":{"index_type":"uint16","offset":"run_graphics_draw.index_buffer.offset-173","surface_uuid":"run_graphics_draw.index_buffer.surface_uuid-174"},"scissor":{"height":175,"width":176,"x":177,"y":178},"viewport":{"height":179.5,"max_depth":180.5,"min_depth":181.5,"width":182.5,"x":183.5,"y":184.5}}"#,
        RunRayTracingKernel => r#"{"op":"run_ray_tracing_kernel","bindings":[{"binding":185,"kind":"sampled_texture","target_id":"run_ray_tracing_kernel.bindings[0].target_id-187"},{"binding":188,"kind":"uniform_buffer","target_id":"run_ray_tracing_kernel.bindings[1].target_id-190"}],"depth":191,"height":192,"kernel_id":"run_ray_tracing_kernel.kernel_id-193","push_constants_hex":"run_ray_tracing_kernel.push_constants_hex-194","request_id":"run_ray_tracing_kernel.request_id-195","width":196}"#,
        TryRunCpuReadbackCopy => r#"{"op":"try_run_cpu_readback_copy","direction":"image_to_buffer","request_id":"try_run_cpu_readback_copy.request_id-198","surface_id":"try_run_cpu_readback_copy.surface_id-199"}"#,
        WaitDeviceIdle => r#"{"op":"wait_device_idle","request_id":"wait_device_idle.request_id-200"}"#,
    );
}

/// Every `EscalateResponse` variant survives a decode/encode round trip unchanged.
#[test]
fn escalate_response_vectors_round_trip() {
    assert_golden_vectors_round_trip!(EscalateResponse:
        Contended => r#"{"result":"contended","request_id":"contended.request_id-1"}"#,
        Err => r#"{"result":"err","message":"err.message-2","request_id":"err.request_id-3"}"#,
        Ok => r#"{"result":"ok","handle_id":"ok.handle_id-4","request_id":"ok.request_id-5","bytes_per_row":"ok.bytes_per_row-6","exporting_device_uuid":"ok.exporting_device_uuid-7","format":"ok.format-8","height":9,"staging_byte_size":"ok.staging_byte_size-10","timeline_value":"ok.timeline_value-11","usage":["ok.usage[0]-12","ok.usage[1]-13"],"width":14,"writable":false}"#,
    );
}

/// Every enum variant keeps its wire spelling.
#[test]
fn escalate_enum_variants_keep_their_wire_spelling() {
    assert_wire_spellings! {
        EscalateRequestLogLevel {
            Debug => "debug",
            Error => "error",
            Info => "info",
            Trace => "trace",
            Warn => "warn",
        }
        EscalateRequestLogSource {
            Python => "python",
        }
        EscalateRequestRegisterGraphicsKernelBindingKind {
            SampledTexture => "sampled_texture",
            StorageBuffer => "storage_buffer",
            StorageImage => "storage_image",
            UniformBuffer => "uniform_buffer",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendAlphaOp {
            Add => "add",
            Max => "max",
            Min => "min",
            ReverseSubtract => "reverse_subtract",
            Subtract => "subtract",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendColorOp {
            Add => "add",
            Max => "max",
            Min => "min",
            ReverseSubtract => "reverse_subtract",
            Subtract => "subtract",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstAlphaFactor {
            ConstantAlpha => "constant_alpha",
            ConstantColor => "constant_color",
            DstAlpha => "dst_alpha",
            DstColor => "dst_color",
            One => "one",
            OneMinusConstantAlpha => "one_minus_constant_alpha",
            OneMinusConstantColor => "one_minus_constant_color",
            OneMinusDstAlpha => "one_minus_dst_alpha",
            OneMinusDstColor => "one_minus_dst_color",
            OneMinusSrcAlpha => "one_minus_src_alpha",
            OneMinusSrcColor => "one_minus_src_color",
            SrcAlpha => "src_alpha",
            SrcAlphaSaturate => "src_alpha_saturate",
            SrcColor => "src_color",
            Zero => "zero",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendDstColorFactor {
            ConstantAlpha => "constant_alpha",
            ConstantColor => "constant_color",
            DstAlpha => "dst_alpha",
            DstColor => "dst_color",
            One => "one",
            OneMinusConstantAlpha => "one_minus_constant_alpha",
            OneMinusConstantColor => "one_minus_constant_color",
            OneMinusDstAlpha => "one_minus_dst_alpha",
            OneMinusDstColor => "one_minus_dst_color",
            OneMinusSrcAlpha => "one_minus_src_alpha",
            OneMinusSrcColor => "one_minus_src_color",
            SrcAlpha => "src_alpha",
            SrcAlphaSaturate => "src_alpha_saturate",
            SrcColor => "src_color",
            Zero => "zero",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcAlphaFactor {
            ConstantAlpha => "constant_alpha",
            ConstantColor => "constant_color",
            DstAlpha => "dst_alpha",
            DstColor => "dst_color",
            One => "one",
            OneMinusConstantAlpha => "one_minus_constant_alpha",
            OneMinusConstantColor => "one_minus_constant_color",
            OneMinusDstAlpha => "one_minus_dst_alpha",
            OneMinusDstColor => "one_minus_dst_color",
            OneMinusSrcAlpha => "one_minus_src_alpha",
            OneMinusSrcColor => "one_minus_src_color",
            SrcAlpha => "src_alpha",
            SrcAlphaSaturate => "src_alpha_saturate",
            SrcColor => "src_color",
            Zero => "zero",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateColorBlendSrcColorFactor {
            ConstantAlpha => "constant_alpha",
            ConstantColor => "constant_color",
            DstAlpha => "dst_alpha",
            DstColor => "dst_color",
            One => "one",
            OneMinusConstantAlpha => "one_minus_constant_alpha",
            OneMinusConstantColor => "one_minus_constant_color",
            OneMinusDstAlpha => "one_minus_dst_alpha",
            OneMinusDstColor => "one_minus_dst_color",
            OneMinusSrcAlpha => "one_minus_src_alpha",
            OneMinusSrcColor => "one_minus_src_color",
            SrcAlpha => "src_alpha",
            SrcAlphaSaturate => "src_alpha_saturate",
            SrcColor => "src_color",
            Zero => "zero",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateDepthCompareOp {
            Always => "always",
            Equal => "equal",
            Greater => "greater",
            GreaterOrEqual => "greater_or_equal",
            Less => "less",
            LessOrEqual => "less_or_equal",
            Never => "never",
            NotEqual => "not_equal",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateDynamicState {
            None => "none",
            ViewportScissor => "viewport_scissor",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationCullMode {
            Back => "back",
            Front => "front",
            FrontAndBack => "front_and_back",
            None => "none",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationFrontFace {
            Clockwise => "clockwise",
            CounterClockwise => "counter_clockwise",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateRasterizationPolygonMode {
            Fill => "fill",
            Line => "line",
            Point => "point",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateTopology {
            LineList => "line_list",
            LineStrip => "line_strip",
            PointList => "point_list",
            TriangleFan => "triangle_fan",
            TriangleList => "triangle_list",
            TriangleStrip => "triangle_strip",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputAttributeFormat {
            R32Float => "r32_float",
            R32Sint => "r32_sint",
            R32Uint => "r32_uint",
            Rg32Float => "rg32_float",
            Rg32Sint => "rg32_sint",
            Rg32Uint => "rg32_uint",
            Rgb32Float => "rgb32_float",
            Rgb32Sint => "rgb32_sint",
            Rgb32Uint => "rgb32_uint",
            Rgba32Float => "rgba32_float",
            Rgba32Sint => "rgba32_sint",
            Rgba32Uint => "rgba32_uint",
            Rgba8Snorm => "rgba8_snorm",
            Rgba8Unorm => "rgba8_unorm",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateVertexInputBindingInputRate {
            Instance => "instance",
            Vertex => "vertex",
        }
        EscalateRequestRegisterGraphicsKernelPipelineStateAttachmentDepthFormat {
            D16Unorm => "d16_unorm",
            D24UnormS8Uint => "d24_unorm_s8_uint",
            D32Sfloat => "d32_sfloat",
        }
        EscalateRequestRegisterRayTracingKernelBindingKind {
            AccelerationStructure => "acceleration_structure",
            SampledTexture => "sampled_texture",
            StorageBuffer => "storage_buffer",
            StorageImage => "storage_image",
            UniformBuffer => "uniform_buffer",
        }
        EscalateRequestRegisterRayTracingKernelGroupKind {
            General => "general",
            ProceduralHit => "procedural_hit",
            TrianglesHit => "triangles_hit",
        }
        EscalateRequestRegisterRayTracingKernelStageStage {
            AnyHit => "any_hit",
            Callable => "callable",
            ClosestHit => "closest_hit",
            Intersection => "intersection",
            Miss => "miss",
            RayGen => "ray_gen",
        }
        EscalateRequestRunCpuReadbackCopyDirection {
            BufferToImage => "buffer_to_image",
            ImageToBuffer => "image_to_buffer",
        }
        EscalateRequestRunGraphicsDrawBindingKind {
            SampledTexture => "sampled_texture",
            StorageBuffer => "storage_buffer",
            StorageImage => "storage_image",
            UniformBuffer => "uniform_buffer",
        }
        EscalateRequestRunGraphicsDrawDrawKind {
            Draw => "draw",
            DrawIndexed => "draw_indexed",
        }
        EscalateRequestRunGraphicsDrawIndexBufferIndexType {
            Uint16 => "uint16",
            Uint32 => "uint32",
        }
        EscalateRequestRunRayTracingKernelBindingKind {
            AccelerationStructure => "acceleration_structure",
            SampledTexture => "sampled_texture",
            StorageBuffer => "storage_buffer",
            StorageImage => "storage_image",
            UniformBuffer => "uniform_buffer",
        }
        EscalateRequestTryRunCpuReadbackCopyDirection {
            BufferToImage => "buffer_to_image",
            ImageToBuffer => "image_to_buffer",
        }
    }
}

/// An absent optional is omitted from the encoding, never written as null.
#[test]
fn absent_optionals_are_omitted_on_a_response() {
    let response = EscalateResponse::Ok(EscalateResponseOk {
        handle_id: "handle-1".to_string(),
        request_id: "request-1".to_string(),
        ..Default::default()
    });
    assert_eq!(
        serde_json::to_string(&response).unwrap(),
        r#"{"result":"ok","handle_id":"handle-1","request_id":"request-1"}"#
    );
}

/// The log record's three nullable-required fields are the exception: they
/// carry an explicit null rather than dropping out of the document, because a
/// runtime-level record has no pipeline and an uncaptured one has no channel.
#[test]
fn a_log_records_nullable_required_fields_encode_as_null() {
    let golden = r#"{"op":"log","attrs":{},"channel":null,"intercepted":false,"level":"info","message":"hello","pipeline_id":null,"processor_id":null,"source":"python","source_seq":"1","source_ts":"2"}"#;
    let decoded: EscalateRequest = serde_json::from_str(golden).unwrap();
    assert_eq!(serde_json::to_string(&decoded).unwrap(), golden);
}
