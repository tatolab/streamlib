// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Inputs of one node type that became one input: a Color that was three or four numbers, or a
//! Vector that was two to four. A saved document opens with the one input holding the merged
//! value, and a wire into an old input is kept through a visible combining node beside the node
//! it fed, which holds the other parts' values and feeds the new input.

use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::NODE_INPUT_VALUES_KEY;

/// The core node type that makes a Color from its parts.
const COMBINE_COLOR_NODE_TYPE: &str =
    "imports/core-nodes/core_nodes.combine_and_separate:CombineColor";
/// The core node type that makes a Vector of any size from its parts.
const COMBINE_VECTOR_NODE_TYPE: &str =
    "imports/core-nodes/core_nodes.combine_and_separate:CombineVector";
/// The output of a combining node.
const COMBINED_OUTPUT: &str = "result";
/// How far left of the node it feeds a combining node is drawn, and how far apart the
/// combining nodes of one node are.
const COMBINING_NODE_OFFSET: (f64, f64) = (-240.0, 120.0);

/// Each input of a node type that became one input.
pub(super) const MERGED_INPUTS: &[MergedInput] = &[
    // Tint's three numbers, one Color with the alpha it never had.
    MergedInput {
        node_type: "imports/core-nodes/core_nodes.color:Tint",
        input: "color",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "red",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "green",
                default: 0.5,
            },
            MergedPart::FedBy {
                old_input: "blue",
                default: 0.2,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    // Duotone's shadow and highlight colours, three numbers each.
    MergedInput {
        node_type: "imports/core-nodes/core_nodes.color:Duotone",
        input: "dark",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "dark_red",
                default: 0.05,
            },
            MergedPart::FedBy {
                old_input: "dark_green",
                default: 0.0,
            },
            MergedPart::FedBy {
                old_input: "dark_blue",
                default: 0.3,
            },
            MergedPart::Fixed(1.0),
        ],
    },
    MergedInput {
        node_type: "imports/core-nodes/core_nodes.color:Duotone",
        input: "light",
        merged_into: MergedInto::Color,
        parts: &[
            MergedPart::FedBy {
                old_input: "light_red",
                default: 1.0,
            },
            MergedPart::FedBy {
                old_input: "light_green",
                default: 0.8,
            },
            MergedPart::FedBy {
                old_input: "light_blue",
                default: 0.4,
            },
            MergedPart::Fixed(1.0),
        ],
    },
];

/// One input that a node type's old inputs became.
#[derive(Debug, Clone, Copy)]
pub(super) struct MergedInput {
    /// The node type's import path, as it is named after any rename.
    pub(super) node_type: &'static str,
    /// The input the old ones became.
    pub(super) input: &'static str,
    /// What the merged value is.
    pub(super) merged_into: MergedInto,
    /// Each part of the merged value in order (`red`, `green`, `blue`, `alpha`, or `x`, `y`,
    /// `z`, `w`), as an old input fed it.
    pub(super) parts: &'static [MergedPart],
}

/// What a merged input holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MergedInto {
    /// A Color, of four parts.
    Color,
    /// A Vector, of as many parts as the row names.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "no shipped row merges a Vector yet")
    )]
    Vector,
}

impl MergedInto {
    fn part_names(self) -> [&'static str; 4] {
        match self {
            Self::Color => ["red", "green", "blue", "alpha"],
            Self::Vector => ["x", "y", "z", "w"],
        }
    }

    fn combining_node_type(self) -> &'static str {
        match self {
            Self::Color => COMBINE_COLOR_NODE_TYPE,
            Self::Vector => COMBINE_VECTOR_NODE_TYPE,
        }
    }
}

/// How one part of a merged value was fed.
#[derive(Debug, Clone, Copy)]
pub(super) enum MergedPart {
    /// By an old input, and its default when the row was written.
    FedBy {
        old_input: &'static str,
        default: f64,
    },
    /// By no old input: the part always held this value.
    Fixed(f64),
}

/// How one part of a merged input arrives: its number, or a wire into the old input.
enum PartArrives {
    AsANumber(f64),
    OnAWire(usize),
}

/// Merge the old inputs of each node of one level that `rows` name, in place; whether any was
/// merged. A wired part adds a combining node to `nodes` and its link to `links`.
pub(super) fn the_inputs_of_the_level_merged(
    nodes: &mut Vec<Value>,
    links: &mut Vec<Value>,
    rows: &[MergedInput],
) -> bool {
    let mut combining_nodes = Vec::new();
    let mut any_merged = false;
    for node in nodes.iter_mut() {
        if let Some(node) = node.as_object_mut() {
            any_merged |= the_inputs_of_a_node_merged(node, links, rows, &mut combining_nodes);
        }
    }
    for mut combining_node in combining_nodes {
        let name = a_name_no_node_of_the_level_holds(nodes, &combining_node["name"]);
        combining_node["name"] = Value::String(name);
        nodes.push(combining_node);
    }
    any_merged
}

/// Merge the old inputs of `node` that `rows` name, in place; whether any was merged. A wired
/// part retargets its link in `links` and adds a combining node to `combining_nodes`.
pub(super) fn the_inputs_of_a_node_merged(
    node: &mut Map<String, Value>,
    links: &mut Vec<Value>,
    rows: &[MergedInput],
    combining_nodes: &mut Vec<Value>,
) -> bool {
    let Some(node_type) = node.get("type").and_then(Value::as_str).map(str::to_owned) else {
        return false;
    };
    let mut any_merged = false;
    let rows_of_the_type = rows.iter().filter(|row| row.node_type == node_type);
    for (row_index, row) in rows_of_the_type.enumerate() {
        if let Some(merged) = the_input_merged(node, links, row, row_index) {
            any_merged = true;
            combining_nodes.extend(merged);
        }
    }
    any_merged
}

/// `wanted`, or the first of `wanted_2`, `wanted_3`, … that no node of `nodes` is named.
fn a_name_no_node_of_the_level_holds(nodes: &[Value], wanted: &Value) -> String {
    let wanted = wanted.as_str().unwrap_or_default();
    let is_held = |name: &str| nodes.iter().any(|node| node["name"] == name);
    (1..)
        .map(|count| match count {
            1 => wanted.to_owned(),
            _ => format!("{wanted}_{count}"),
        })
        .find(|name| !is_held(name))
        .unwrap_or_default()
}

/// Merge the old inputs `row` names on `node`, the `row_index`th row of its type; `None` when
/// the node held none of them, saved or wired, or already holds the new input. Answers the
/// combining node a wired part adds.
fn the_input_merged(
    node: &mut Map<String, Value>,
    links: &mut Vec<Value>,
    row: &MergedInput,
    row_index: usize,
) -> Option<Option<Value>> {
    let node_id = node.get("id").and_then(Value::as_str)?.to_owned();
    let input_values = node
        .get(NODE_INPUT_VALUES_KEY)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let is_linked_into = |links: &[Value], input: &str| {
        links
            .iter()
            .position(|link| link["target"]["node"] == node_id && link["target"]["port"] == input)
    };
    if input_values.contains_key(row.input) || is_linked_into(links, row.input).is_some() {
        return None;
    }
    let arriving: Vec<PartArrives> = row
        .parts
        .iter()
        .map(|part| match *part {
            MergedPart::FedBy { old_input, default } => is_linked_into(links, old_input)
                .map(PartArrives::OnAWire)
                .unwrap_or_else(|| {
                    PartArrives::AsANumber(
                        input_values
                            .get(old_input)
                            .and_then(Value::as_f64)
                            .unwrap_or(default),
                    )
                }),
            MergedPart::Fixed(value) => PartArrives::AsANumber(value),
        })
        .collect();
    let held_any = row.parts.iter().any(|part| match part {
        MergedPart::FedBy { old_input, .. } => input_values.contains_key(*old_input),
        MergedPart::Fixed(_) => false,
    });
    let wired_any = arriving
        .iter()
        .any(|part| matches!(part, PartArrives::OnAWire(_)));
    if !held_any && !wired_any {
        return None;
    }
    let mut input_values = input_values;
    for part in row.parts {
        if let MergedPart::FedBy { old_input, .. } = part {
            input_values.remove(*old_input);
        }
    }
    let part_names = row.merged_into.part_names();
    let combining_node = if wired_any {
        let combining_node_id = the_combining_node_id(&node_id, row.input);
        let mut combining_values = Map::new();
        for (part_name, part) in part_names.iter().zip(&arriving) {
            match part {
                PartArrives::AsANumber(value) => {
                    combining_values.insert((*part_name).to_owned(), json!(value));
                }
                PartArrives::OnAWire(link) => {
                    links[*link]["target"] = json!({"node": combining_node_id, "port": part_name});
                }
            }
        }
        links.push(json!({
            "source": {"node": combining_node_id, "port": COMBINED_OUTPUT},
            "target": {"node": node_id, "port": row.input},
        }));
        Some(the_combining_node(
            node,
            row,
            row_index,
            &combining_node_id,
            combining_values,
        ))
    } else {
        let merged: Map<String, Value> = part_names
            .iter()
            .zip(&arriving)
            .filter_map(|(part_name, part)| match part {
                PartArrives::AsANumber(value) => Some(((*part_name).to_owned(), json!(value))),
                PartArrives::OnAWire(_) => None,
            })
            .collect();
        input_values.insert(row.input.to_owned(), Value::Object(merged));
        None
    };
    super::put_input_values(node, input_values);
    Some(combining_node)
}

/// The UUID of the combining node that feeds `input` of the node `node_id`.
fn the_combining_node_id(node_id: &str, input: &str) -> String {
    let namespace = Uuid::parse_str(node_id).unwrap_or(Uuid::NAMESPACE_OID);
    Uuid::new_v5(&namespace, format!("combines {input}").as_bytes()).to_string()
}

/// The combining node `row` places beside `node`, holding `values`.
fn the_combining_node(
    node: &Map<String, Value>,
    row: &MergedInput,
    row_index: usize,
    combining_node_id: &str,
    values: Map<String, Value>,
) -> Value {
    let node_name = node.get("name").and_then(Value::as_str).unwrap_or("node");
    let mut combining_node = json!({
        "id": combining_node_id,
        "name": format!("{node_name}_{}", row.input),
        "type": row.merged_into.combining_node_type(),
        NODE_INPUT_VALUES_KEY: values,
    });
    let position = node
        .get("studio")
        .and_then(|studio| studio.get("position"))
        .and_then(|position| Some((position.get("x")?.as_f64()?, position.get("y")?.as_f64()?)));
    if let Some((x, y)) = position {
        let (left, apart) = COMBINING_NODE_OFFSET;
        combining_node["studio"] = json!({
            "position": {"x": x + left, "y": y + apart * row_index as f64}
        });
    }
    combining_node
}

#[cfg(test)]
mod tests;
