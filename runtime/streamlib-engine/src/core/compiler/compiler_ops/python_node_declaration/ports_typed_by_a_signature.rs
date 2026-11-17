// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The pin types a function or shader node's signature states exactly: its number and Boolean
//! values. Its picture ports are typed by their Image family, as every picture port is.

use crate::core::descriptors::{NodeDescriptor, PinDeclaration, PinType};

use super::super::shader_node_program::ShaderNodeProgram;

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

/// Each port a described function node's signature types exactly, by name. None when the
/// describe states no signature.
pub(super) fn ports_a_function_node_signature_types(
    described_node_type: &serde_json::Value,
) -> std::result::Result<Vec<(String, PinType)>, String> {
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
            if let Some(pin_type) =
                exact_pin_type_of_a_function_node_value_type(field("value_type")?)
            {
                typed_ports.push((port.to_string(), pin_type));
            }
        }
    }
    Ok(typed_ports)
}

/// Each port a shader node's program types exactly, by name: its float inputs.
pub(super) fn ports_a_shader_node_program_types(
    program: &ShaderNodeProgram,
) -> Vec<(String, PinType)> {
    program
        .float_input_ports()
        .map(|port| (port.to_string(), PinType::Float { exact_size: None }))
        .collect()
}

/// Declare each of `typed_ports` on the port of its name: exactly that type, in its default
/// flow. A name the node has no port of is refused.
pub(super) fn declare_each_typed_port(
    descriptor: &mut NodeDescriptor,
    typed_ports: Vec<(String, PinType)>,
) -> std::result::Result<(), String> {
    for (port_name, pin_type) in typed_ports {
        let port = descriptor
            .inputs
            .iter_mut()
            .chain(descriptor.outputs.iter_mut())
            .find(|port| port.name == port_name)
            .ok_or_else(|| {
                format!("the signature types port {port_name:?}, which the node does not declare")
            })?;
        port.pin_declaration = Some(PinDeclaration::exactly(pin_type));
    }
    Ok(())
}
