// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use super::format_vocabulary::parse_texture_format_name;
use super::kernels::{
    PythonAccelerationStructureHandle, ReflectedKernelBinding, bound_acceleration_structure_id,
    bound_surface_id, reflected_binding_names,
};

/// Lowercase hex, no `0x`, no separators — the encoding every escalate blob
/// field uses.
pub(super) fn encode_lowercase_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut encoded, byte| {
            let _ = write!(encoded, "{byte:02x}");
            encoded
        },
    )
}

/// A geometry blob as the wire carries it: little-endian `f32`s, lowercase hex.
pub(super) fn encode_little_endian_f32_hex(values: &[f32]) -> String {
    encode_lowercase_hex(
        &values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<u8>>(),
    )
}

/// An index blob as the wire carries it: little-endian `u32`s, lowercase hex.
pub(super) fn encode_little_endian_u32_hex(values: &[u32]) -> String {
    encode_lowercase_hex(
        &values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<u8>>(),
    )
}

/// One word of a fixed wire vocabulary, or the refusal naming the whole set.
///
/// Every enum the escalate wire spells travels as a string the host parses, so
/// checking the spelling here is what keeps a typo on the caller's own stack
/// rather than arriving as an escalate failure a round trip later.
fn parse_wire_vocabulary_word(
    vocabulary_label: &str,
    supplied: &str,
    accepted: &[&'static str],
) -> PyResult<&'static str> {
    accepted
        .iter()
        .find(|known| **known == supplied)
        .copied()
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown {vocabulary_label} {supplied:?}; the accepted spellings are {}",
                accepted.join(", ")
            ))
        })
}

/// The binding kinds a compute kernel's wire spells.
const COMPUTE_BINDING_KIND_WIRE_NAMES: &[&str] = &[
    "sampled_image",
    "sampled_texture",
    "storage_buffer",
    "storage_image",
    "uniform_buffer",
];

/// The binding kinds a graphics kernel's wire spells. No `sampled_image`: the
/// graphics pipeline has no samplerless-texture descriptor.
pub(super) const GRAPHICS_BINDING_KIND_WIRE_NAMES: &[&str] = &[
    "sampled_texture",
    "storage_buffer",
    "storage_image",
    "uniform_buffer",
];

/// The binding kinds a ray-tracing kernel's wire spells.
pub(super) const RAY_TRACING_BINDING_KIND_WIRE_NAMES: &[&str] = &[
    ACCELERATION_STRUCTURE_BINDING_KIND_WIRE_NAME,
    "sampled_texture",
    "storage_buffer",
    "storage_image",
    "uniform_buffer",
];

/// The one binding kind whose value is an acceleration structure rather than a
/// surface, which is why the dispatch path branches on it by name.
const ACCELERATION_STRUCTURE_BINDING_KIND_WIRE_NAME: &str = "acceleration_structure";

/// The stage bits a graphics binding declaration may name. Host counterpart:
/// `GraphicsShaderStageFlags`.
pub(super) const GRAPHICS_SHADER_STAGE_WIRE_BITS: &[(&str, u32)] =
    &[("vertex", 1), ("fragment", 2)];

/// The stage bits a ray-tracing binding declaration may name. Host
/// counterpart: `RayTracingShaderStageFlags`.
pub(super) const RAY_TRACING_SHADER_STAGE_WIRE_BITS: &[(&str, u32)] = &[
    ("ray_gen", 1),
    ("miss", 2),
    ("closest_hit", 4),
    ("any_hit", 8),
    ("intersection", 16),
    ("callable", 32),
];

/// The stages a ray-tracing kernel's shader modules may fill.
const RAY_TRACING_SHADER_STAGE_WIRE_NAMES: &[&str] = &[
    "any_hit",
    "callable",
    "closest_hit",
    "intersection",
    "miss",
    "ray_gen",
];

/// The shader-group kinds a ray-tracing kernel's binding table is built from.
const RAY_TRACING_GROUP_KIND_WIRE_NAMES: &[&str] = &["general", "procedural_hit", "triangles_hit"];

/// What a shader group's stage index carries when the group names no stage
/// there. Every stage-index field is present on the wire, so absent needs a
/// value; host counterpart: `RAY_TRACING_STAGE_INDEX_NONE`.
const RAY_TRACING_STAGE_INDEX_NONE: u32 = u32::MAX;

/// Turn a sequence of spelled-out names into the bitmask the wire carries.
///
/// Every bitmask the escalate wire carries — a binding's stage visibility, a
/// TLAS instance's geometry flags — is spelled here rather than handed over as
/// a raw integer, so a caller never writes a bit position. An empty sequence is
/// an empty mask, which for stages asserts nothing and lets reflection stand.
fn named_bits_to_wire_bitmask(
    vocabulary_label: &str,
    named: &Bound<'_, PyAny>,
    bit_vocabulary: &[(&'static str, u32)],
) -> PyResult<u32> {
    let accepted: Vec<&'static str> = bit_vocabulary.iter().map(|(name, _)| *name).collect();
    let mut mask = 0u32;
    for name in named.try_iter()? {
        let name: String = name?.extract()?;
        let named = parse_wire_vocabulary_word(vocabulary_label, &name, &accepted)?;
        mask |= bit_vocabulary
            .iter()
            .find(|(candidate, _)| *candidate == named)
            .map_or(0, |(_, bit)| *bit);
    }
    Ok(mask)
}

/// Turn `{name: kind}` into the wire's declaration array.
pub(super) fn declared_compute_bindings_to_wire<'py>(
    python: Python<'py>,
    declared: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyList>> {
    let wire = PyList::empty(python);
    if let Some(declared) = declared {
        for (name, kind) in declared.iter() {
            let name: String = name.extract()?;
            let kind: String = kind.extract()?;
            let entry = PyDict::new(python);
            entry.set_item("name", name)?;
            entry.set_item(
                "kind",
                parse_wire_vocabulary_word(
                    "compute binding kind",
                    &kind,
                    COMPUTE_BINDING_KIND_WIRE_NAMES,
                )?,
            )?;
            wire.append(entry)?;
        }
    }
    Ok(wire)
}

/// Turn `{name: kind}` or `{name: (kind, stages)}` into the wire's declaration
/// array, for a kernel kind whose bindings carry a stage mask.
///
/// Graphics and ray tracing differ only in which two vocabularies they name,
/// which is also why the host reconciles both through one function.
pub(super) fn declared_staged_kernel_bindings_to_wire<'py>(
    python: Python<'py>,
    declared: Option<&Bound<'py, PyDict>>,
    binding_kind_label: &str,
    binding_kind_vocabulary: &[&'static str],
    stage_label: &str,
    stage_bits: &[(&'static str, u32)],
) -> PyResult<Bound<'py, PyList>> {
    let wire = PyList::empty(python);
    let Some(declared) = declared else {
        return Ok(wire);
    };
    for (name, declaration) in declared.iter() {
        let name: String = name.extract()?;
        let (kind, stages) = match declaration.extract::<String>() {
            Ok(kind) => (kind, 0),
            Err(_) => {
                let (kind, named_stages) = declaration
                    .extract::<(String, Bound<'_, PyAny>)>()
                    .map_err(|_| {
                        PyTypeError::new_err(format!(
                            "binding {name:?} must be declared as a kind, or as a (kind, stages) \
                             pair naming the stages that read it"
                        ))
                    })?;
                (
                    kind,
                    named_bits_to_wire_bitmask(stage_label, &named_stages, stage_bits)?,
                )
            }
        };
        let entry = PyDict::new(python);
        entry.set_item("name", name)?;
        entry.set_item(
            "kind",
            parse_wire_vocabulary_word(binding_kind_label, &kind, binding_kind_vocabulary)?,
        )?;
        entry.set_item("stages", stages)?;
        wire.append(entry)?;
    }
    Ok(wire)
}

/// One entry of a list-of-mappings argument, refused by name when it is not a
/// mapping.
fn mapping_argument_entry<'py>(
    argument_label: &str,
    index: usize,
    entry: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyDict>> {
    entry
        .cast::<PyDict>()
        .cloned()
        .map_err(|_| PyTypeError::new_err(format!("{argument_label} {index} must be a dict")))
}

/// Refuse a mapping carrying a key this argument does not accept.
///
/// A misspelled key would otherwise travel as an absent one the wire fills with
/// a default, which is the silently-wrong-result shape.
fn refuse_unaccepted_mapping_keys(
    mapping_label: &str,
    mapping: &Bound<'_, PyDict>,
    accepted: &[&str],
) -> PyResult<()> {
    for key in mapping.keys() {
        let key: String = key.extract()?;
        if !accepted.contains(&key.as_str()) {
            return Err(PyValueError::new_err(format!(
                "{mapping_label} was given an unknown key {key:?}; it accepts {}",
                accepted.join(", ")
            )));
        }
    }
    Ok(())
}

/// The `u32` at `key`, or `None` when the mapping does not carry it.
fn optional_u32_in(mapping: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<u32>> {
    match mapping.get_item(key)? {
        Some(value) => Ok(Some(value.extract()?)),
        None => Ok(None),
    }
}

/// The string at `key`, or `None` when the mapping does not carry it.
fn optional_string_in(mapping: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    match mapping.get_item(key)? {
        Some(value) => Ok(Some(value.extract()?)),
        None => Ok(None),
    }
}

/// The primitive topologies a graphics pipeline can assemble.
const GRAPHICS_TOPOLOGY_WIRE_NAMES: &[&str] = &[
    "line_list",
    "line_strip",
    "point_list",
    "triangle_fan",
    "triangle_list",
    "triangle_strip",
];

const GRAPHICS_POLYGON_MODE_WIRE_NAMES: &[&str] = &["fill", "line", "point"];

const GRAPHICS_CULL_MODE_WIRE_NAMES: &[&str] = &["back", "front", "front_and_back", "none"];

const GRAPHICS_FRONT_FACE_WIRE_NAMES: &[&str] = &["clockwise", "counter_clockwise"];

const GRAPHICS_DYNAMIC_STATE_WIRE_NAMES: &[&str] = &["none", "viewport_scissor"];

const COLOR_BLEND_FACTOR_WIRE_NAMES: &[&str] = &[
    "constant_alpha",
    "constant_color",
    "dst_alpha",
    "dst_color",
    "one",
    "one_minus_constant_alpha",
    "one_minus_constant_color",
    "one_minus_dst_alpha",
    "one_minus_dst_color",
    "one_minus_src_alpha",
    "one_minus_src_color",
    "src_alpha",
    "src_alpha_saturate",
    "src_color",
    "zero",
];

const COLOR_BLEND_OP_WIRE_NAMES: &[&str] = &["add", "max", "min", "reverse_subtract", "subtract"];

/// The keys the `color_blend` argument accepts, each defaulting to the
/// conventional source-alpha-over blend when the mapping omits it.
const COLOR_BLEND_ARGUMENT_KEYS: &[&str] = &[
    "alpha_op",
    "color_op",
    "dst_alpha_factor",
    "dst_color_factor",
    "src_alpha_factor",
    "src_color_factor",
];

/// The colour channels a draw writes, as the bitmask the wire carries.
fn color_write_channels_to_wire(channels: &str) -> PyResult<u32> {
    let mut mask = 0u32;
    for channel in channels.chars() {
        mask |= match channel {
            'r' => 1,
            'g' => 2,
            'b' => 4,
            'a' => 8,
            _ => {
                return Err(PyValueError::new_err(format!(
                    "unknown colour channel {channel:?} in {channels:?}; a write mask names some \
                     of \"rgba\""
                )));
            }
        };
    }
    Ok(mask)
}

/// The fixed-function state and attachment formats `create_graphics_kernel` was
/// asked for.
pub(super) struct GraphicsPipelineStateArguments<'a, 'py> {
    pub(super) color_attachment_formats: &'a [String],
    pub(super) topology: &'a str,
    pub(super) polygon_mode: &'a str,
    pub(super) cull_mode: &'a str,
    pub(super) front_face: &'a str,
    pub(super) line_width: f32,
    pub(super) color_write_channels: &'a str,
    pub(super) color_blend: Option<&'a Bound<'py, PyDict>>,
    pub(super) dynamic_state: &'a str,
}

/// Flatten the pipeline state into the one-level document the wire carries.
///
/// Every field is present because the wire is flat — JSON has no sum types —
/// and the flags decide which ones mean anything. Three groups are pinned here
/// rather than offered as arguments, because a caller could only ever set them
/// to a shape that fails:
/// - `multisample_samples`, since the host builds single-sampled pipelines only.
/// - the vertex-input arrays, since no escalate op mints a vertex buffer for a
///   draw to pull through them.
/// - the depth fields, since the offscreen pass a draw runs attaches colour
///   targets only.
pub(super) fn graphics_pipeline_state_to_wire<'py>(
    python: Python<'py>,
    state: &GraphicsPipelineStateArguments<'_, '_>,
) -> PyResult<Bound<'py, PyDict>> {
    if let Some(color_blend) = state.color_blend {
        refuse_unaccepted_mapping_keys("color_blend", color_blend, COLOR_BLEND_ARGUMENT_KEYS)?;
    }
    let blend_word = |key: &str,
                      when_absent: &'static str,
                      vocabulary: &[&'static str]|
     -> PyResult<&'static str> {
        let Some(color_blend) = state.color_blend else {
            return Ok(when_absent);
        };
        match optional_string_in(color_blend, key)? {
            Some(spelled) => parse_wire_vocabulary_word(key, &spelled, vocabulary),
            None => Ok(when_absent),
        }
    };

    let color_formats = PyList::empty(python);
    for format in state.color_attachment_formats {
        color_formats.append(parse_texture_format_name(format)?)?;
    }

    let wire = PyDict::new(python);
    wire.set_item("attachment_color_formats", color_formats)?;
    wire.set_item(
        "topology",
        parse_wire_vocabulary_word("topology", state.topology, GRAPHICS_TOPOLOGY_WIRE_NAMES)?,
    )?;
    wire.set_item(
        "rasterization_polygon_mode",
        parse_wire_vocabulary_word(
            "polygon mode",
            state.polygon_mode,
            GRAPHICS_POLYGON_MODE_WIRE_NAMES,
        )?,
    )?;
    wire.set_item(
        "rasterization_cull_mode",
        parse_wire_vocabulary_word("cull mode", state.cull_mode, GRAPHICS_CULL_MODE_WIRE_NAMES)?,
    )?;
    wire.set_item(
        "rasterization_front_face",
        parse_wire_vocabulary_word(
            "front face",
            state.front_face,
            GRAPHICS_FRONT_FACE_WIRE_NAMES,
        )?,
    )?;
    wire.set_item("rasterization_line_width", state.line_width)?;
    wire.set_item("multisample_samples", 1u32)?;
    wire.set_item("vertex_input_bindings", PyList::empty(python))?;
    wire.set_item("vertex_input_attributes", PyList::empty(python))?;
    wire.set_item("depth_stencil_enabled", false)?;
    wire.set_item("depth_write", false)?;
    wire.set_item("depth_compare_op", "always")?;
    wire.set_item(
        "color_write_mask",
        color_write_channels_to_wire(state.color_write_channels)?,
    )?;
    wire.set_item("color_blend_enabled", state.color_blend.is_some())?;
    wire.set_item(
        "color_blend_src_color_factor",
        blend_word(
            "src_color_factor",
            "src_alpha",
            COLOR_BLEND_FACTOR_WIRE_NAMES,
        )?,
    )?;
    wire.set_item(
        "color_blend_dst_color_factor",
        blend_word(
            "dst_color_factor",
            "one_minus_src_alpha",
            COLOR_BLEND_FACTOR_WIRE_NAMES,
        )?,
    )?;
    wire.set_item(
        "color_blend_color_op",
        blend_word("color_op", "add", COLOR_BLEND_OP_WIRE_NAMES)?,
    )?;
    wire.set_item(
        "color_blend_src_alpha_factor",
        blend_word("src_alpha_factor", "one", COLOR_BLEND_FACTOR_WIRE_NAMES)?,
    )?;
    wire.set_item(
        "color_blend_dst_alpha_factor",
        blend_word(
            "dst_alpha_factor",
            "one_minus_src_alpha",
            COLOR_BLEND_FACTOR_WIRE_NAMES,
        )?,
    )?;
    wire.set_item(
        "color_blend_alpha_op",
        blend_word("alpha_op", "add", COLOR_BLEND_OP_WIRE_NAMES)?,
    )?;
    wire.set_item(
        "dynamic_state",
        parse_wire_vocabulary_word(
            "dynamic state",
            state.dynamic_state,
            GRAPHICS_DYNAMIC_STATE_WIRE_NAMES,
        )?,
    )?;
    Ok(wire)
}

/// The keys one entry of the `stages` argument accepts.
const RAY_TRACING_STAGE_ARGUMENT_KEYS: &[&str] = &["entry_point", "source", "spirv", "stage"];

/// Turn `stages=[…]` into the wire's shader-stage array.
///
/// `source` and `spirv` both travel: exactly-one-of is refused host-side, in
/// the one place that rule is written.
pub(super) fn ray_tracing_stages_to_wire<'py>(
    python: Python<'py>,
    stages: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyList>> {
    let wire = PyList::empty(python);
    for (index, stage) in stages.try_iter()?.enumerate() {
        let stage = mapping_argument_entry("stage", index, &stage?)?;
        refuse_unaccepted_mapping_keys(
            &format!("stage {index}"),
            &stage,
            RAY_TRACING_STAGE_ARGUMENT_KEYS,
        )?;
        let named_stage = optional_string_in(&stage, "stage")?.ok_or_else(|| {
            PyValueError::new_err(format!(
                "stage {index} names no `stage`; every shader module says which stage it fills"
            ))
        })?;
        let spirv: Vec<u8> = match stage.get_item("spirv")? {
            Some(blob) => blob.extract()?,
            None => Vec::new(),
        };
        let entry = PyDict::new(python);
        entry.set_item(
            "stage",
            parse_wire_vocabulary_word(
                "ray-tracing stage",
                &named_stage,
                RAY_TRACING_SHADER_STAGE_WIRE_NAMES,
            )?,
        )?;
        entry.set_item(
            "source",
            optional_string_in(&stage, "source")?.unwrap_or_default(),
        )?;
        entry.set_item("spv_hex", encode_lowercase_hex(&spirv))?;
        entry.set_item(
            "entry_point",
            optional_string_in(&stage, "entry_point")?.unwrap_or_else(|| "main".to_string()),
        )?;
        wire.append(entry)?;
    }
    Ok(wire)
}

/// The keys one entry of the `groups` argument accepts.
const RAY_TRACING_GROUP_ARGUMENT_KEYS: &[&str] = &[
    "any_hit_stage",
    "closest_hit_stage",
    "general_stage",
    "intersection_stage",
    "kind",
];

/// Turn `groups=[…]` into the wire's shader-group array.
///
/// A group names its stages by index into the `stages` argument — the shader
/// binding table is built in this order, and two modules can fill the same
/// stage, so there is no name to use instead. Absent indices become the wire's
/// sentinel here rather than in the caller's source.
pub(super) fn ray_tracing_shader_groups_to_wire<'py>(
    python: Python<'py>,
    groups: &Bound<'_, PyAny>,
    stage_count: usize,
) -> PyResult<Bound<'py, PyList>> {
    let wire = PyList::empty(python);
    for (index, group) in groups.try_iter()?.enumerate() {
        let group = mapping_argument_entry("group", index, &group?)?;
        refuse_unaccepted_mapping_keys(
            &format!("group {index}"),
            &group,
            RAY_TRACING_GROUP_ARGUMENT_KEYS,
        )?;
        let kind = optional_string_in(&group, "kind")?
            .ok_or_else(|| PyValueError::new_err(format!("group {index} names no `kind`")))?;
        let kind = parse_wire_vocabulary_word(
            "shader group kind",
            &kind,
            RAY_TRACING_GROUP_KIND_WIRE_NAMES,
        )?;

        let named_stage = |key: &str| -> PyResult<Option<u32>> {
            let Some(stage_index) = optional_u32_in(&group, key)? else {
                return Ok(None);
            };
            if stage_index as usize >= stage_count {
                return Err(PyValueError::new_err(format!(
                    "group {index} names {key} {stage_index}, and only {stage_count} shader \
                     module(s) were supplied"
                )));
            }
            Ok(Some(stage_index))
        };
        let general = named_stage("general_stage")?;
        let closest_hit = named_stage("closest_hit_stage")?;
        let any_hit = named_stage("any_hit_stage")?;
        let intersection = named_stage("intersection_stage")?;

        match kind {
            "general" if general.is_none() => {
                return Err(PyValueError::new_err(format!(
                    "group {index} is `general` and names no `general_stage`; a general group is \
                     the one ray-gen, miss or callable module it points at"
                )));
            }
            "triangles_hit" if closest_hit.is_none() && any_hit.is_none() => {
                return Err(PyValueError::new_err(format!(
                    "group {index} is `triangles_hit` and names neither `closest_hit_stage` nor \
                     `any_hit_stage`; a hit group needs at least one of them"
                )));
            }
            "procedural_hit" if intersection.is_none() => {
                return Err(PyValueError::new_err(format!(
                    "group {index} is `procedural_hit` and names no `intersection_stage`, which \
                     is the module a procedural group intersects with"
                )));
            }
            _ => {}
        }

        let entry = PyDict::new(python);
        entry.set_item("kind", kind)?;
        entry.set_item(
            "general_stage",
            general.unwrap_or(RAY_TRACING_STAGE_INDEX_NONE),
        )?;
        entry.set_item(
            "closest_hit_stage",
            closest_hit.unwrap_or(RAY_TRACING_STAGE_INDEX_NONE),
        )?;
        entry.set_item(
            "any_hit_stage",
            any_hit.unwrap_or(RAY_TRACING_STAGE_INDEX_NONE),
        )?;
        entry.set_item(
            "intersection_stage",
            intersection.unwrap_or(RAY_TRACING_STAGE_INDEX_NONE),
        )?;
        wire.append(entry)?;
    }
    Ok(wire)
}

/// The keys one entry of `build_tlas`'s `instances` argument accepts.
const TLAS_INSTANCE_ARGUMENT_KEYS: &[&str] = &[
    "blas",
    "custom_index",
    "flags",
    "mask",
    "sbt_record_offset",
    "transform",
];

/// The `VkGeometryInstanceFlagsKHR` bits an instance can name, spelled rather
/// than passed as a raw mask.
const GEOMETRY_INSTANCE_FLAG_WIRE_BITS: &[(&str, u32)] = &[
    ("triangle_facing_cull_disable", 1),
    ("triangle_flip_facing", 2),
    ("force_opaque", 4),
    ("force_no_opaque", 8),
];

/// Row-major 3×4 identity — where an instance that names no transform sits.
const IDENTITY_TLAS_INSTANCE_TRANSFORM: [f32; 12] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0,
];

/// The widest value an instance's 24-bit `gl_InstanceCustomIndexEXT` can carry.
/// The host masks the high byte off silently, so it is refused here.
const WIDEST_TLAS_INSTANCE_CUSTOM_INDEX: u32 = 0x00ff_ffff;

/// Turn `instances=[…]` into the wire's TLAS instance array.
pub(super) fn tlas_instances_to_wire<'py>(
    python: Python<'py>,
    instances: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyList>> {
    let wire = PyList::empty(python);
    for (index, instance) in instances.try_iter()?.enumerate() {
        let instance = mapping_argument_entry("instance", index, &instance?)?;
        refuse_unaccepted_mapping_keys(
            &format!("instance {index}"),
            &instance,
            TLAS_INSTANCE_ARGUMENT_KEYS,
        )?;
        let named_blas = instance.get_item("blas")?.ok_or_else(|| {
            PyValueError::new_err(format!(
                "instance {index} names no `blas`; an instance places one bottom-level structure \
                 in the scene"
            ))
        })?;
        let bottom_level = named_blas
            .extract::<PyRef<'_, PythonAccelerationStructureHandle>>()
            .map_err(|_| {
                PyTypeError::new_err(format!(
                    "instance {index}'s `blas` must be the handle `build_triangles_blas` returned"
                ))
            })?;
        if bottom_level.is_top_level {
            return Err(PyValueError::new_err(format!(
                "instance {index}'s `blas` is a top-level structure; an instance places a \
                 bottom-level one, and the top-level structure is what a trace binds"
            )));
        }
        let transform: Vec<f32> = match instance.get_item("transform")? {
            Some(transform) => transform.extract()?,
            None => IDENTITY_TLAS_INSTANCE_TRANSFORM.to_vec(),
        };
        if transform.len() != 12 {
            return Err(PyValueError::new_err(format!(
                "instance {index}'s transform has {} floats; it is a row-major 3×4 affine, so \
                 exactly 12",
                transform.len()
            )));
        }
        let mask = optional_u32_in(&instance, "mask")?.unwrap_or(0xff);
        if mask > 0xff {
            return Err(PyValueError::new_err(format!(
                "instance {index}'s mask is {mask}; a visibility mask is 8-bit, and a ray hits \
                 the instance when `mask & cull_mask` is non-zero"
            )));
        }
        let custom_index = optional_u32_in(&instance, "custom_index")?.unwrap_or(0);
        if custom_index > WIDEST_TLAS_INSTANCE_CUSTOM_INDEX {
            return Err(PyValueError::new_err(format!(
                "instance {index}'s custom_index is {custom_index}; it reaches hit shaders as a \
                 24-bit `gl_InstanceCustomIndexEXT`, so anything above \
                 {WIDEST_TLAS_INSTANCE_CUSTOM_INDEX} would arrive truncated"
            )));
        }
        let flags = match instance.get_item("flags")? {
            Some(named_flags) => named_bits_to_wire_bitmask(
                "geometry instance flag",
                &named_flags,
                GEOMETRY_INSTANCE_FLAG_WIRE_BITS,
            )?,
            None => 0,
        };

        let entry = PyDict::new(python);
        entry.set_item("blas_id", bottom_level.acceleration_structure_id.as_str())?;
        entry.set_item("transform", transform)?;
        entry.set_item("mask", mask)?;
        entry.set_item("custom_index", custom_index)?;
        entry.set_item(
            "sbt_record_offset",
            optional_u32_in(&instance, "sbt_record_offset")?.unwrap_or(0),
        )?;
        entry.set_item("flags", flags)?;
        wire.append(entry)?;
    }
    Ok(wire)
}

/// The kind the shaders declare `name` as.
///
/// An unknown name is refused here rather than sent — the round trip would
/// refuse it too, but the caller's own stack is where the mistake is.
fn reflected_kind_of_binding<'a>(
    reflected: &'a [ReflectedKernelBinding],
    name: &str,
) -> PyResult<&'a str> {
    reflected
        .iter()
        .find(|binding| binding.name == name)
        .map(|binding| binding.kind.as_str())
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "no binding named {name:?}; these shaders declare {}",
                reflected_binding_names(reflected)
                    .iter()
                    .map(|declared| format!("{declared:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

/// Refuse a push-constant payload that is not the size the kernel declares.
///
/// The engine reconciles the declared size against reflection at construction,
/// so a kernel that exists agrees with its shaders and this check is the
/// shaders' own.
pub(super) fn require_declared_push_constant_size(
    declared_size: u32,
    supplied: &[u8],
) -> PyResult<()> {
    if supplied.len() != declared_size as usize {
        return Err(PyValueError::new_err(format!(
            "this kernel declares {declared_size} push-constant bytes but {} were supplied",
            supplied.len()
        )));
    }
    Ok(())
}

/// One dispatch's bindings as the wire carries them, each resolved by the kind
/// the shaders declare it.
///
/// `wire_target_field_name` is the wire's own name for the bound resource —
/// `surface_uuid` on a graphics draw, `target_id` everywhere else. An
/// `acceleration_structure` binding resolves through its own registry rather
/// than through a surface, so it is the one kind that takes a different handle.
pub(super) fn supplied_kernel_bindings_to_wire<'py>(
    python: Python<'py>,
    reflected: &[ReflectedKernelBinding],
    supplied: &Bound<'py, PyDict>,
    wire_target_field_name: &str,
) -> PyResult<Bound<'py, PyList>> {
    let wire_bindings = PyList::empty(python);
    for (name, bound_to) in supplied.iter() {
        let name: String = name.extract()?;
        let kind = reflected_kind_of_binding(reflected, &name)?.to_string();
        let target_id = if kind == ACCELERATION_STRUCTURE_BINDING_KIND_WIRE_NAME {
            bound_acceleration_structure_id(&name, &bound_to)?
        } else {
            bound_surface_id(&name, &bound_to)?
        };
        let entry = PyDict::new(python);
        entry.set_item(wire_target_field_name, target_id)?;
        entry.set_item("name", name)?;
        entry.set_item("kind", kind)?;
        wire_bindings.append(entry)?;
    }
    Ok(wire_bindings)
}

/// What a caller can get wrong building a graphics or ray-tracing kernel,
/// refused before anything is sent.
///
/// Each of these travels as a plain field of a `#[serde(deny_unknown_fields)]`
/// escalate document, so a mistake the wheel forwards comes back as a parse
/// failure naming a wire field the author never wrote. Provable with no GPU:
/// nothing here reaches the exchange client.
#[cfg(test)]
mod kernel_argument_tests;
