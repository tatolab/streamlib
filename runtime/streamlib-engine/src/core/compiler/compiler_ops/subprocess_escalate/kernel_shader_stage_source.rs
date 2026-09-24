// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The shader source a kernel register op supplies for one stage, shared by the
//! compute, graphics and ray-tracing families.

use std::sync::Arc;

use super::hex_encoded_wire_bytes::decode_hex;
use crate::core::context::GpuContextLimitedAccess;
use crate::core::rhi::GlslCompilationTargetStage;

/// Which of the two shader sources a register op supplied for one stage.
///
/// Resolved before escalating. Supplying neither, both, or undecodable hex is
/// a malformed request, and refusing one must not cost a turn of the device
/// gate — the same reason every other `_hex` field is decoded up here.
pub(super) enum RegisteredShaderStageSource {
    /// GLSL text, compiled once the handler holds Full access.
    GlslSource {
        source: String,
        stage: GlslCompilationTargetStage,
        entry_point: String,
        field_name: String,
    },
    /// Bytes the caller compiled elsewhere — the escape hatch.
    PreCompiledSpirv {
        spirv: Arc<[u8]>,
        entry_point: String,
    },
}

/// Read one stage's shader out of a register op, without touching the device.
///
/// GLSL source and pre-compiled SPIR-V are alternatives: both is ambiguous
/// about which the caller meant to run, and neither leaves nothing to build.
/// Neither is guessable, so both are named.
pub(super) fn registered_shader_stage_source(
    field_prefix: &str,
    source: &str,
    spv_hex: &str,
    stage: GlslCompilationTargetStage,
    entry_point: &str,
) -> std::result::Result<RegisteredShaderStageSource, String> {
    let source_field = format!("{field_prefix}source");
    let spv_field = format!("{field_prefix}spv_hex");
    match (source.is_empty(), spv_hex.is_empty()) {
        (true, true) => Err(format!(
            "neither {source_field} nor {spv_field} was supplied for the {} stage; a kernel is \
             built from GLSL source or from pre-compiled SPIR-V, and one of the two has to \
             be there",
            stage.wire_name()
        )),
        (false, false) => Err(format!(
            "both {source_field} and {spv_field} were supplied for the {} stage; they are \
             alternatives, and which one the kernel should run is not something to guess at",
            stage.wire_name()
        )),
        (false, true) => Ok(RegisteredShaderStageSource::GlslSource {
            source: source.to_string(),
            stage,
            entry_point: normalized_shader_entry_point(entry_point).to_string(),
            field_name: source_field,
        }),
        (true, false) => decode_hex(spv_hex)
            .map(|spv| RegisteredShaderStageSource::PreCompiledSpirv {
                spirv: spv.into(),
                entry_point: normalized_shader_entry_point(entry_point).to_string(),
            })
            .map_err(|e| format!("{spv_field} decode: {e}")),
    }
}

impl RegisteredShaderStageSource {
    /// The stage's SPIR-V, compiling the GLSL if that is what was supplied.
    ///
    /// Takes the sandbox rather than a `GpuContextFullAccess`, so it needs no
    /// escalate scope and every handler calls it before opening one:
    /// compilation is CPU work that touches no device, and that gate
    /// serializes every processor's device work. A cold C++ compile is
    /// milliseconds no other processor should ever wait on.
    pub(super) fn spirv(
        &self,
        sandbox: &GpuContextLimitedAccess,
    ) -> crate::core::error::Result<Arc<[u8]>> {
        match self {
            Self::GlslSource {
                source,
                stage,
                entry_point,
                field_name,
            } => sandbox.host_inner().compile_glsl_shader_source_to_spirv(
                source,
                *stage,
                entry_point,
                field_name,
            ),
            Self::PreCompiledSpirv { spirv, .. } => Ok(Arc::clone(spirv)),
        }
    }

    /// The entry point the pipeline stage is built against — the same value
    /// the module was compiled against, normalized once at resolution.
    pub(super) fn entry_point(&self) -> &str {
        match self {
            Self::GlslSource { entry_point, .. } | Self::PreCompiledSpirv { entry_point, .. } => {
                entry_point
            }
        }
    }
}

/// An empty entry point on the wire means `main`, the same normalization the
/// graphics and ray-tracing stage fields have always documented.
pub(super) fn normalized_shader_entry_point(entry_point: &str) -> &str {
    if entry_point.is_empty() {
        crate::core::rhi::DEFAULT_SHADER_ENTRY_POINT
    } else {
        entry_point
    }
}

/// SPIR-V's magic number, little-endian — the cheapest proof that what reached
/// a bridge is a module rather than the source text.
#[cfg(test)]
pub(super) const SPIRV_MAGIC_LE: [u8; 4] = 0x0723_0203u32.to_le_bytes();
