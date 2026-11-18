// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The values set live on a stream's inputs, held apart from its graph so a live set changes no
//! node, link or port, and the value each input reads while no link feeds it: its live value,
//! else its saved one, else its declared default.

use std::collections::{BTreeMap, HashMap};

use parking_lot::RwLock;
use serde_json::{Map, Value};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    InputValueComesFromOnTheWire, InputValueReadOnTheWire,
};
use uuid::Uuid;

use super::live_input_values_kept_in_the_project_store::LiveInputValuesKeptInTheProjectStore;
use crate::core::descriptors::{NodeKind, NodeValueLifetime};
use crate::core::graph::{Graph, GraphNode, PortInfo, WhyTheNodeLeaves};
use crate::core::pubsub::RuntimeEvent;

/// The values set live on one stream's inputs, by node UUID and then input, each with the
/// lifetime its input declares. A set takes no graph write lock, so a running stream's live
/// values change while its graph is read. Each value of a `kept` input is mirrored into the
/// project's store when the stream names one.
#[derive(Debug, Default)]
pub struct LiveInputValuesOfAStream {
    values: RwLock<HashMap<Uuid, BTreeMap<String, LiveInputValue>>>,
    kept_values_mirrored_into: Option<LiveInputValuesKeptInTheProjectStore>,
}

/// One value set live on an input, and the lifetime the input declares.
#[derive(Debug)]
struct LiveInputValue {
    value: Value,
    lifetime: NodeValueLifetime,
}

impl LiveInputValuesOfAStream {
    /// The live values of a stream that mirrors each value of a `kept` input into
    /// `project_store`, its project's store.
    pub(crate) fn mirrored_into(project_store: LiveInputValuesKeptInTheProjectStore) -> Self {
        Self {
            values: RwLock::default(),
            kept_values_mirrored_into: Some(project_store),
        }
    }

    /// The project's store the values of `kept` inputs are mirrored into, when the stream
    /// names one.
    pub(crate) fn project_store_mirrored_into(
        &self,
    ) -> Option<&LiveInputValuesKeptInTheProjectStore> {
        self.kept_values_mirrored_into.as_ref()
    }

    /// Set `value` live on input `input` of `node`, whose lifetime is `lifetime`, `None` taking
    /// the live value off, answering the live value it held before. A value of a `kept` input
    /// is mirrored into the project's store, and one no longer held forgotten there.
    pub fn set(
        &self,
        node: Uuid,
        input: &str,
        value: Option<Value>,
        lifetime: NodeValueLifetime,
    ) -> Option<Value> {
        let project_store = self.kept_values_mirrored_into.as_ref();
        let kept_now = value.is_some() && lifetime.is_kept();
        if let (Some(project_store), Some(value)) = (project_store, value.as_ref())
            && kept_now
        {
            project_store.keep(node, input, value);
        }
        let before = match value {
            Some(value) => self.insert(node, input, LiveInputValue { value, lifetime }),
            None => self.take_off(node, input),
        };
        let kept_before = before
            .as_ref()
            .is_some_and(|before| before.lifetime.is_kept());
        if let Some(project_store) = project_store
            && kept_before
            && !kept_now
        {
            project_store.forget(node, input);
        }
        before.map(|before| before.value)
    }

    /// Hold `value`, which the project's store keeps for input `input` of `node`, as the live
    /// value of that `kept` input, without writing it back.
    pub(crate) fn hold_the_kept_value_the_store_holds(
        &self,
        node: Uuid,
        input: &str,
        value: Value,
    ) {
        self.insert(
            node,
            input,
            LiveInputValue {
                value,
                lifetime: NodeValueLifetime::Kept,
            },
        );
    }

    fn insert(&self, node: Uuid, input: &str, live: LiveInputValue) -> Option<LiveInputValue> {
        self.values
            .write()
            .entry(node)
            .or_default()
            .insert(input.to_owned(), live)
    }

    fn take_off(&self, node: Uuid, input: &str) -> Option<LiveInputValue> {
        let mut values = self.values.write();
        let values_of_the_node = values.get_mut(&node)?;
        let before = values_of_the_node.remove(input);
        if values_of_the_node.is_empty() {
            values.remove(&node);
        }
        before
    }

    /// The value set live on input `input` of `node`; `None` when none is.
    pub fn value_of(&self, node: Uuid, input: &str) -> Option<Value> {
        self.values
            .read()
            .get(&node)
            .and_then(|values_of_the_node| values_of_the_node.get(input))
            .map(|live| live.value.clone())
    }

    /// Hold each value set live on an input of `node` under the lifetime `lifetime_of` answers
    /// for its input where that differs from the one it is held under: a value now `kept` is
    /// mirrored into the project's store, and one now `reset` forgotten there. A value whose
    /// lifetime is unchanged is left as it is held.
    pub(crate) fn hold_each_value_of_the_node_whose_lifetime_changed(
        &self,
        node: Uuid,
        lifetime_of: impl Fn(&str) -> NodeValueLifetime,
    ) {
        let changed: Vec<(String, Value, NodeValueLifetime)> = self
            .values
            .read()
            .get(&node)
            .into_iter()
            .flatten()
            .filter_map(|(input, live)| {
                let lifetime = lifetime_of(input);
                (lifetime != live.lifetime).then(|| (input.clone(), live.value.clone(), lifetime))
            })
            .collect();
        for (input, value, lifetime) in changed {
            self.set(node, &input, Some(value), lifetime);
        }
    }

    /// Each value set live on an input of `node`, by input.
    pub fn values_of_the_node(&self, node: Uuid) -> Map<String, Value> {
        self.values
            .read()
            .get(&node)
            .into_iter()
            .flatten()
            .map(|(input, live)| (input.clone(), live.value.clone()))
            .collect()
    }

    /// Forget the values set live on inputs of `node` as it leaves the graph, as `why` it
    /// leaves says: removed, every one, the project's store forgetting each `kept` one; torn
    /// down at a Stop, to be put back, each on a `reset` input, the `kept` ones held across.
    pub fn forget_the_node(&self, node: Uuid, why: WhyTheNodeLeaves) {
        let forgotten = {
            let mut values = self.values.write();
            match why {
                WhyTheNodeLeaves::Removed => values.remove(&node),
                WhyTheNodeLeaves::TornDownAtAStop => {
                    if let Some(values_of_the_node) = values.get_mut(&node) {
                        values_of_the_node.retain(|_, live| live.lifetime.is_kept());
                        if values_of_the_node.is_empty() {
                            values.remove(&node);
                        }
                    }
                    None
                }
            }
        };
        let Some(project_store) = self.kept_values_mirrored_into.as_ref() else {
            return;
        };
        for (input, _) in forgotten
            .into_iter()
            .flatten()
            .filter(|(_, live)| live.lifetime.is_kept())
        {
            project_store.forget(node, &input);
        }
    }

    /// Forget every value set live on a `reset` input, as the stream stops, answering each
    /// input forgotten as `(node, input)`.
    pub(crate) fn forget_every_reset_value(&self) -> Vec<(Uuid, String)> {
        let mut values = self.values.write();
        let mut forgotten = Vec::new();
        values.retain(|node, values_of_the_node| {
            values_of_the_node.retain(|input, live| {
                let kept = live.lifetime.is_kept();
                if !kept {
                    forgotten.push((*node, input.clone()));
                }
                kept
            });
            !values_of_the_node.is_empty()
        });
        forgotten
    }
}

/// The lifetime input `input` of `node` declares; kept for an input it does not have.
pub(crate) fn the_lifetime_of_the_input(node: &GraphNode, input: &str) -> NodeValueLifetime {
    node.input_port(input)
        .map_or_else(NodeValueLifetime::default, |port| port.lifetime)
}

/// Whether input `port` of a node running as `node_kind` takes a stored value: a setting does;
/// else what its pin type holds and, on a node that is no function or shader node, only a
/// Channels input; on a port that names no pin type, any input of a function node.
pub(crate) fn the_input_port_takes_a_value(node_kind: Option<NodeKind>, port: &PortInfo) -> bool {
    if !port.takes_a_wire() {
        return true;
    }
    let read_whole_by_its_node = matches!(
        node_kind,
        Some(NodeKind::Function { .. } | NodeKind::Shader)
    );
    match &port.pin {
        Some(pin) if !pin.pin_type.holds_a_stored_value() => false,
        Some(_) if read_whole_by_its_node => true,
        None if matches!(node_kind, Some(NodeKind::Function { .. })) => true,
        _ => port.family.family == crate::core::descriptors::PortFamily::Channels,
    }
}

/// The value input `port` of `node` in `graph` reads, `is_linked` when a link feeds it: wire;
/// else its live value, its saved one, which is its config field's for a config field the node
/// exposes as an input, or its declared default; else nothing. `None` for an input that takes
/// no stored value and no link feeds.
pub(crate) fn the_value_an_input_reads(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
    is_linked: bool,
) -> Option<InputValueReadOnTheWire> {
    if is_linked {
        return Some(InputValueReadOnTheWire {
            value: None,
            value_comes_from: Some(InputValueComesFromOnTheWire::Wire),
        });
    }
    let exposed_config_field = node.is_an_exposed_config_field_input(&port.name);
    if !exposed_config_field && !the_input_port_takes_a_value(node.node_kind(), port) {
        return None;
    }
    let saved = if exposed_config_field {
        node.config
            .as_ref()
            .and_then(|config| config.get(&port.name))
            .filter(|value| !value.is_null())
            .cloned()
    } else {
        node.pins.get(&port.name).cloned()
    };
    let read = graph
        .live_input_values()
        .value_of(node.node_uuid, &port.name)
        .map(|value| (value, InputValueComesFromOnTheWire::Live))
        .or_else(|| saved.map(|value| (value, InputValueComesFromOnTheWire::Saved)))
        .or_else(|| {
            port.pin_default
                .clone()
                .map(|value| (value, InputValueComesFromOnTheWire::Default))
        });
    Some(match read {
        Some((value, value_comes_from)) => InputValueReadOnTheWire {
            value: Some(value),
            value_comes_from: Some(value_comes_from),
        },
        None => InputValueReadOnTheWire {
            value: Some(Value::Null),
            value_comes_from: None,
        },
    })
}

/// The value each input of `node` in `graph` reads while no link feeds it, by input: its live
/// value, else its saved one, else its declared default. An input that takes no stored value, a
/// config field the node exposes as an input, which reads the node's config, and an input with
/// none of the three are left out.
pub(crate) fn the_unlinked_input_values(graph: &Graph, node: &GraphNode) -> Map<String, Value> {
    node.ports
        .inputs
        .iter()
        .filter(|port| !node.is_an_exposed_config_field_input(&port.name))
        .filter_map(|port| {
            let value = the_value_an_input_reads(graph, node, port, false)?.value?;
            (!value.is_null()).then(|| (port.name.clone(), value))
        })
        .collect()
}

/// The `InputValueDidChange` announcing what input `input` of `node` in `graph` reads now, under
/// the input's current name; `None` when the node has no such input.
pub(crate) fn the_input_value_change(
    graph: &Graph,
    node: &GraphNode,
    input: &str,
) -> Option<RuntimeEvent> {
    let port = node.input_port(input)?;
    Some(RuntimeEvent::InputValueDidChange {
        node: node.node_uuid,
        input: port.name.clone(),
        value_read: the_value_an_input_reads(
            graph,
            node,
            port,
            graph.link_into(&node.id, &port.name).is_some(),
        ),
    })
}

/// The pin values `node` of `graph` reads, each value set live on one of its inputs over the
/// saved one.
pub(crate) fn the_pin_values_with_the_live_ones_over_them(
    graph: &Graph,
    node: &GraphNode,
) -> Map<String, Value> {
    let mut pin_values = node.pins.clone();
    pin_values.extend(graph.live_input_values().values_of_the_node(node.node_uuid));
    pin_values
}

/// One value a live set makes: the node, by UUID, its input, and the value, `None` taking the
/// input's live value off.
#[derive(Debug, Clone, PartialEq)]
pub struct InputValueToSetLive {
    /// The node, by UUID.
    pub node: Uuid,
    /// The input, by name.
    pub input: String,
    /// The value, `None` taking the live value off.
    pub value: Option<Value>,
}

/// What a live set of values made: each set apart from the graph, or none, because one is held
/// on the graph and the set takes the path a graph edit takes.
#[derive(Debug, Clone, PartialEq)]
pub enum InputValuesSetLive {
    /// Each value set live apart from the graph, in the order asked; no node, link or port
    /// changed.
    ApartFromTheGraph(Vec<InputValueSetLive>),
    /// One value is held on the graph: a setting, a config field exposed as an input, an input
    /// its node does not have, or a node the graph does not hold. None was set.
    OnTheGraph,
}

/// Why a live set of values was refused, every value left as it was.
#[derive(Debug)]
pub enum LiveInputValueSetRefusal {
    /// The input of the value at `set_index` does not take it.
    TheInputDoesNotTakeIt {
        /// The index of the refused value among those asked.
        set_index: usize,
        /// Why its input does not take it.
        refusal: super::InputValueRefusal,
    },
    /// The value at `set_index` is on a `reset` input of a stream whose playback is stopped,
    /// which holds no live value until it plays: its node does not run, because its playback is
    /// stopped.
    TheInputIsResetWhileStopped {
        /// The index of the refused value among those asked.
        set_index: usize,
        /// Why, in the runtime's words, naming the node and the input.
        runtime_words: String,
    },
    /// The value at `set_index` is on an input an earlier value of the same set is on, of the
    /// same node: it would replace the earlier value before the node is handed either, so one set
    /// names each input once.
    TheInputIsSetTwice {
        /// The index of the second value on the input among those asked.
        set_index: usize,
        /// Why, in the runtime's words, naming the node and the input.
        runtime_words: String,
    },
    /// A helper or island reading one of the values was not handed it.
    NotHandedOver(crate::core::Error),
}

/// Why a live set naming input `input` of the node named `node_name` more than once is refused:
/// the last value would replace each earlier one before the node is handed any, so the earlier
/// writes would never be published.
pub fn the_words_of_an_input_set_twice_live(input: &str, node_name: &str) -> String {
    format!(
        "input `{input}` of `{node_name}` is set twice in one live transaction, and the second \
         value would replace the first before the node is handed either, so the first would \
         never be published. To fix: set each input once, or make each write a transaction of \
         its own"
    )
}

/// One value set live apart from the graph: the input, the live value it held before, the
/// saved value under it, and the output of the link that feeds the input, by node UUID and port,
/// when one does.
#[derive(Debug, Clone, PartialEq)]
pub struct InputValueSetLive {
    /// The input, by name.
    pub input: String,
    /// The value set live on it before the set; `None` when none was.
    pub live_value_before: Option<Value>,
    /// The saved value under the live one; `None` when the graph document holds none.
    pub saved_value: Option<Value>,
    /// The output a link feeds the input from, as `(node UUID, port)`, which overrides the value.
    pub linked_from: Option<(Uuid, String)>,
}
