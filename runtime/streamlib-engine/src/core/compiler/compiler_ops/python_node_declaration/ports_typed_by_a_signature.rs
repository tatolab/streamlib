// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The pin types a function or shader node's signature states exactly: its number and Boolean
//! values, and a shader's output Texture in the one format every shader writes. Its other
//! picture ports are typed by their Image family, as every picture port is.

use crate::core::descriptors::{
    FloatSize, NodeDescriptor, PinDeclaration, PinType, TexturePixelFormat,
};

use super::super::shader_node_program::{SHADER_NODE_OUTPUT_TEXTURE_FORMAT, ShaderNodeProgram};

/// The described key a function node's signature sits under.
const DESCRIBED_FUNCTION_SIGNATURE_KEY: &str = "function_signature";

/// The pin type a function node's signature value type states exactly; `None` for one that
/// states no exact type.
fn exact_pin_type_of_a_function_node_value_type(value_type: &str) -> Option<PinType> {
    match value_type {
        "float" => Some(PinType::Float { exact_size: None }),
        "int" => Some(PinType::Integer { exact_size: None }),
        "bool" => Some(PinType::Boolean {}),
        _ => None,
    }
}

/// Each port a described function node's signature types exactly, by name, with its
/// declaration: exactly that type in its default flow, and an optional input, which reads
/// nothing while unwired, may be unset. None when the describe states no signature.
pub(super) fn ports_a_function_node_signature_types(
    described_node_type: &serde_json::Value,
) -> std::result::Result<Vec<(String, PinDeclaration)>, String> {
    let Some(signature) = described_node_type
        .get(DESCRIBED_FUNCTION_SIGNATURE_KEY)
        .filter(|signature| !signature.is_null())
    else {
        return Ok(Vec::new());
    };
    let mut typed_ports = Vec::new();
    for side in ["inputs", "outputs"] {
        let entries = signature
            .get(side)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("{DESCRIBED_FUNCTION_SIGNATURE_KEY}.{side} must be a list"))?;
        for entry in entries {
            let field = |key: &str| {
                entry
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        format!(
                            "each of {DESCRIBED_FUNCTION_SIGNATURE_KEY}.{side} names its `{key}` \
                             as a string"
                        )
                    })
            };
            let port = field("port")?;
            let may_be_unset = match entry.get("optional") {
                None | Some(serde_json::Value::Null) => false,
                Some(optional) => optional.as_bool().ok_or_else(|| {
                    format!(
                        "each of {DESCRIBED_FUNCTION_SIGNATURE_KEY}.{side} states `optional` as \
                         a bool"
                    )
                })?,
            };
            if let Some(pin_type) =
                exact_pin_type_of_a_function_node_value_type(field("value_type")?)
            {
                let declaration = PinDeclaration {
                    may_be_unset,
                    ..PinDeclaration::exactly(pin_type)
                };
                typed_ports.push((port.to_string(), declaration));
            }
        }
    }
    Ok(typed_ports)
}

/// Each port a shader node's program types exactly, by name, with its declaration: its inputs
/// of numbers, a Float exactly a 32-bit Float, the four bytes its push constant holds, and a
/// Color or a Vector as its signature states it, each part a 32-bit push constant too; and its
/// output, a Texture of the format the runtime writes every shader's output in.
pub(super) fn ports_a_shader_node_program_types(
    program: &ShaderNodeProgram,
) -> Vec<(String, PinDeclaration)> {
    let number_inputs = program.number_inputs().map(|(port, pin_type)| {
        let pin_type = match pin_type {
            PinType::Float { .. } => PinType::Float {
                exact_size: Some(FloatSize::Float32),
            },
            other => other.clone(),
        };
        (port.to_string(), PinDeclaration::exactly(pin_type))
    });
    let output = (
        program.output_port.clone(),
        PinDeclaration::exactly(shader_node_output_texture()),
    );
    number_inputs.chain(std::iter::once(output)).collect()
}

/// The Texture every shader node's output is: of the one format the runtime writes it in.
fn shader_node_output_texture() -> PinType {
    let pixel_format =
        TexturePixelFormat::from_wire_name(SHADER_NODE_OUTPUT_TEXTURE_FORMAT.wire_name())
            .expect("a shader node's output format is a Texture pixel format");
    PinType::Texture {
        pixel_formats: vec![pixel_format],
    }
}

/// Declare each of `typed_ports` on the port of its name, unless that port's author states its
/// own declaration, as a pin under the typed grammar does: what the author states, its
/// annotations included, is the port's. A name the node has no port of is refused.
pub(super) fn declare_each_typed_port(
    descriptor: &mut NodeDescriptor,
    typed_ports: Vec<(String, PinDeclaration)>,
) -> std::result::Result<(), String> {
    for (port_name, pin_declaration) in typed_ports {
        let port = descriptor
            .inputs
            .iter_mut()
            .chain(descriptor.outputs.iter_mut())
            .find(|port| port.name == port_name)
            .ok_or_else(|| {
                format!("the signature types port {port_name:?}, which the node does not declare")
            })?;
        port.pin_declaration.get_or_insert(pin_declaration);
    }
    Ok(())
}
