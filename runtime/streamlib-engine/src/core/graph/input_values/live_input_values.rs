// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The values set live on a stream's inputs, held apart from its graph so a live set changes no
//! node, link or port, and the value each input reads while no link feeds it: its live value,
//! else its saved one, else its declared default.

use std::collections::{BTreeMap, HashMap, HashSet};

use parking_lot::RwLock;
use serde_json::{Map, Value};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    InputValueComesFromOnTheWire, InputValueReadOnTheWire,
};
use uuid::Uuid;

use super::bundle_input_read::{
    ValuesABundleInputHolds, the_bundle_members_of, the_live_members_over_the_held_ones,
};
use super::live_input_values_kept_in_the_project_store::LiveInputValuesKeptInTheProjectStore;
use crate::core::descriptors::{NodeClassImportPath, NodeKind, NodeValueLifetime, PortDescriptor};
use crate::core::graph::{
    Graph, GraphNode, PortInfo, WhatTheLinksIntoAnInputFeed, WhyTheNodeLeaves,
};
use crate::core::json_schema::GraphNodePortOnTheWire;
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

    /// Set `value` live on input `input` of `node`, or on its member `member` when one is
    /// named, as [`Self::set`] does: a member set changes that member of the input's live value
    /// alone, so a bundle input's live value holds the members set live. Answers the input's
    /// whole live value before.
    pub fn set_on_the_input_or_its_member(
        &self,
        node: Uuid,
        input: &str,
        member: Option<&str>,
        value: Option<Value>,
        lifetime: NodeValueLifetime,
    ) -> Option<Value> {
        let value = match member {
            None => value,
            Some(member) => streamlib_project_repository::the_input_value_after_the_set(
                self.value_of(node, input),
                Some(member),
                value.as_ref(),
            ),
        };
        self.set(node, input, value, lifetime)
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
        self.forget_the_node_but_the_inputs(node, why, &HashSet::new());
    }

    /// [`Self::forget_the_node`], keeping the value set live on each of `inputs_kept`, which a
    /// node holding `node`'s UUID after it still has.
    pub(crate) fn forget_the_node_but_the_inputs(
        &self,
        node: Uuid,
        why: WhyTheNodeLeaves,
        inputs_kept: &HashSet<String>,
    ) {
        let forgotten = {
            let mut values = self.values.write();
            match why {
                WhyTheNodeLeaves::Removed if inputs_kept.is_empty() => values.remove(&node),
                WhyTheNodeLeaves::Removed => values.get_mut(&node).map(|values_of_the_node| {
                    values_of_the_node
                        .extract_if(.., |input, _| !inputs_kept.contains(input))
                        .collect()
                }),
                WhyTheNodeLeaves::TornDownAtAStop => {
                    if let Some(values_of_the_node) = values.get_mut(&node) {
                        values_of_the_node.retain(|input, live| {
                            live.lifetime.is_kept() || inputs_kept.contains(input)
                        });
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
/// else what its pin type holds and, on a node that is no function or shader node and whose
/// input does not state that it takes a wire or a value, only a Channels input; on a port that
/// names no pin type, any input of a function node.
pub(crate) fn the_input_port_takes_a_value(node_kind: Option<NodeKind>, port: &PortInfo) -> bool {
    if port.is_a_setting() {
        return true;
    }
    match &port.pin {
        Some(pin) if !pin.pin_type.holds_a_stored_value() => false,
        Some(_) if the_node_reads_a_value_set_on_the_input_whole(node_kind, port) => true,
        None if matches!(node_kind, Some(NodeKind::Function { .. })) => true,
        _ => port.family.family == crate::core::descriptors::PortFamily::Channels,
    }
}

/// Whether a node running as `node_kind` reads a value set on its typed input `port` whole,
/// in any form its pin type takes: a function or shader node reads each so, and so does a
/// node whose input states that it takes a wire or a value, as a class node's field does; any
/// other node reads only what a Channels input carries.
pub(crate) fn the_node_reads_a_value_set_on_the_input_whole(
    node_kind: Option<NodeKind>,
    port: &PortInfo,
) -> bool {
    matches!(
        node_kind,
        Some(NodeKind::Function { .. } | NodeKind::Shader)
    ) || port.input_mode == Some(crate::core::descriptors::NodeInputMode::WireOrValue)
}

/// What an input a link feeds reads: no value of its own, and `wire` as where it comes from.
pub(crate) fn the_read_of_an_input_a_link_feeds() -> InputValueReadOnTheWire {
    InputValueReadOnTheWire {
        value: None,
        value_comes_from: Some(InputValueComesFromOnTheWire::Wire),
        member_values: None,
    }
}

/// What an input no link feeds reads: `read`, the value it reads and where that comes from, else
/// `null` from nowhere.
pub(crate) fn the_read_of_an_unfed_input(
    read: Option<(Value, InputValueComesFromOnTheWire)>,
) -> InputValueReadOnTheWire {
    match read {
        Some((value, value_comes_from)) => InputValueReadOnTheWire {
            value: Some(value),
            value_comes_from: Some(value_comes_from),
            member_values: None,
        },
        None => InputValueReadOnTheWire {
            value: Some(Value::Null),
            value_comes_from: None,
            member_values: None,
        },
    }
}

/// Whether `graph` says of a setting a node type the runtime owns declares that a link may end
/// at it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TakesAWireRendered<'node_type> {
    /// As the one rule answers for a setting of a node of this type, which is how `connect`
    /// judges a link into it.
    ByTheRuleOnANodeOf(&'node_type NodeClassImportPath),
    /// `false`: each is a config field a node type not yet typed describes as a setting, which
    /// is no port, so `connect` takes no link into it, as a view node's settings are.
    NoneOfThemTakesOne,
    /// Left out: `connect` judges a link into it by no setting's rule, as it judges one into a
    /// placed stream, by the Inputs its stream declares.
    LeftOut,
}

/// Each of `settings`, the settings a node type the runtime owns declares, as `graph` renders it
/// on a node holding `input_values`: reading the value the node holds, else the default the
/// setting declares, and saying whether a link may end at it as `takes_a_wire` says.
pub(crate) fn the_settings_rendered_reading_the_values_held(
    settings: &[PortDescriptor],
    input_values: &Map<String, Value>,
    takes_a_wire: TakesAWireRendered<'_>,
) -> Vec<GraphNodePortOnTheWire> {
    settings
        .iter()
        .map(|setting| {
            let setting_info = PortInfo::from(setting);
            let mut rendered = GraphNodePortOnTheWire::from(&setting_info);
            rendered.takes_a_wire = match takes_a_wire {
                TakesAWireRendered::ByTheRuleOnANodeOf(node_type) => {
                    Some(setting_info.takes_a_wire_on_a_node_of(node_type))
                }
                TakesAWireRendered::NoneOfThemTakesOne => Some(false),
                TakesAWireRendered::LeftOut => None,
            };
            let read = input_values
                .get(&setting.name)
                .filter(|held| !held.is_null())
                .map(|held| (held.clone(), InputValueComesFromOnTheWire::Saved))
                .or_else(|| {
                    setting
                        .pin_default
                        .clone()
                        .map(|default| (default, InputValueComesFromOnTheWire::Default))
                });
            rendered.value_read = Some(the_read_of_an_unfed_input(read));
            rendered
        })
        .collect()
}

/// The value input `port` of `node` in `graph` reads, `fed` saying what the links into it feed:
/// wire, while a link feeds the whole input; else its live value, its saved one, which is its config field's for a config field the node
/// exposes as an input, or its declared default; else nothing. A bundle input reads each member
/// so alone, as `member_values`, `wire` for a member a link ends at. `None` for an input that takes no stored value and no link
/// feeds.
pub(crate) fn the_value_an_input_reads(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
    fed: &WhatTheLinksIntoAnInputFeed,
) -> Option<InputValueReadOnTheWire> {
    let port = &*port.typed_by_its_setting_in(&node.ports.inputs, &node.pins);
    if let Some(members) = the_bundle_members_of(port)
        && !node.is_an_exposed_config_field_input(&port.name)
    {
        if !fed.anything() && !the_input_port_takes_a_value(node.node_kind(), port) {
            return None;
        }
        let live = graph
            .live_input_values()
            .value_of(node.node_uuid, &port.name);
        let held = ValuesABundleInputHolds {
            live: live.as_ref(),
            saved: node.pins.get(&port.name),
            input_default: port.pin_default.as_ref(),
        };
        return Some(InputValueReadOnTheWire {
            value: None,
            value_comes_from: None,
            member_values: Some(held.each_member_read(members, fed)),
        });
    }
    if fed.the_whole_input() {
        return Some(the_read_of_an_input_a_link_feeds());
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
    Some(the_read_of_an_unfed_input(read))
}

/// The value input `port` of `node` in `graph` reads while no link feeds it, as its node is
/// handed it: what [`the_value_an_input_reads`] renders, and for a bundle input the bundle its
/// members read. `None` when it reads nothing.
fn the_value_an_unlinked_input_is_handed(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
) -> Option<Value> {
    let port = &*port.typed_by_its_setting_in(&node.ports.inputs, &node.pins);
    if let Some(members) = the_bundle_members_of(port) {
        if !the_input_port_takes_a_value(node.node_kind(), port) {
            return None;
        }
        let live = graph
            .live_input_values()
            .value_of(node.node_uuid, &port.name);
        return ValuesABundleInputHolds {
            live: live.as_ref(),
            saved: node.pins.get(&port.name),
            input_default: port.pin_default.as_ref(),
        }
        .the_bundle_read(members);
    }
    the_value_an_input_reads(graph, node, port, &WhatTheLinksIntoAnInputFeed::default())?
        .value
        .filter(|value| !value.is_null())
}

/// The value each input of `node` in `graph` reads while no link feeds it, by input: its live
/// value, else its saved one, else its declared default. A setting read at setup that a wire
/// feeds reads instead the value its wire last carried, once it has carried one, and a
/// connection input the node its wire brings. An input that takes no stored value, a config
/// field the node exposes as an input, which reads the node's config, and an input with none
/// of these are left out.
pub(crate) fn the_unlinked_input_values(graph: &Graph, node: &GraphNode) -> Map<String, Value> {
    node.ports
        .inputs
        .iter()
        .filter(|port| !node.is_an_exposed_config_field_input(&port.name))
        .filter_map(|port| {
            the_value_a_wire_into_the_setting_carried(graph, node, port)
                .or_else(|| the_node_a_connection_wire_brings(node, port))
                .or_else(|| the_value_an_unlinked_input_is_handed(graph, node, port))
                .map(|value| (port.name.clone(), value))
        })
        .collect()
}

/// The node reference a wire into connection input `port` of `node` was lowered to, which the
/// node reads as the node its wire brings; `None` for any other input, and while no wire
/// reaches it. A connection holds no stored value of its own,
/// so this is the one value it is handed.
fn the_node_a_connection_wire_brings(node: &GraphNode, port: &PortInfo) -> Option<Value> {
    if !port.takes_a_connection() {
        return None;
    }
    node.pins
        .get(&port.name)
        .filter(|reference| !reference.is_null())
        .cloned()
}

/// The value setting `setting` of `node` in `graph` reads while no wire has carried it one: its
/// live value, else its saved one, else its declared default; `None` with none of the three.
pub(crate) fn the_value_a_setting_reads_with_no_wire(
    graph: &Graph,
    node: &GraphNode,
    setting: &str,
) -> Option<Value> {
    the_value_an_unlinked_input_is_handed(graph, node, node.input_port(setting)?)
}

/// The value the wire into `port` of `node` in `graph` last carried that the node is built
/// with, when `port` is a setting read at setup a link feeds; `None` otherwise, and while its
/// wire has carried none.
fn the_value_a_wire_into_the_setting_carried(
    graph: &Graph,
    node: &GraphNode,
    port: &PortInfo,
) -> Option<Value> {
    if !port.is_a_setting_read_at_setup()
        || !WhatTheLinksIntoAnInputFeed::of(graph, &node.id, &port.name).the_whole_input()
    {
        return None;
    }
    graph
        .values_wires_carry_into_settings()
        .the_value_its_node_is_built_with(node.node_uuid, &port.name)
}

/// The `InputValueDidChange` announcing what input `input` of `node` in `graph` reads now, under
/// the input's current name; none when the node has no such input. A bundle input announces a
/// change of each member, or of `member` alone when one is named, each naming its member.
pub(crate) fn the_input_value_change(
    graph: &Graph,
    node: &GraphNode,
    input: &str,
    member: Option<&str>,
) -> Vec<RuntimeEvent> {
    let Some(port) = node.input_port(input) else {
        return Vec::new();
    };
    let value_read = the_value_an_input_reads(
        graph,
        node,
        port,
        &WhatTheLinksIntoAnInputFeed::of(graph, &node.id, &port.name),
    );
    let Some(member_values) = value_read
        .as_ref()
        .and_then(|read| read.member_values.clone())
    else {
        return vec![RuntimeEvent::InputValueDidChange {
            node: node.node_uuid,
            input: port.name.clone(),
            member: None,
            value_read,
        }];
    };
    member_values
        .into_iter()
        .filter(|read| member.is_none_or(|member| read.member == member))
        .map(|read| RuntimeEvent::InputValueDidChange {
            node: node.node_uuid,
            input: port.name.clone(),
            member: Some(read.member),
            value_read: Some(InputValueReadOnTheWire {
                value: read.value,
                value_comes_from: read.value_comes_from,
                member_values: None,
            }),
        })
        .collect()
}

/// The pin values `node` of `graph` reads, each value set live on one of its inputs over the
/// saved one; on a bundle input, each member set live over that member alone.
pub(crate) fn the_pin_values_with_the_live_ones_over_them(
    graph: &Graph,
    node: &GraphNode,
) -> Map<String, Value> {
    let mut pin_values = node.pins.clone();
    for (input, live) in graph.live_input_values().values_of_the_node(node.node_uuid) {
        let is_a_bundle = node
            .input_port(&input)
            .and_then(the_bundle_members_of)
            .is_some();
        let read = if is_a_bundle {
            the_live_members_over_the_held_ones(pin_values.get(&input), live)
        } else {
            live
        };
        pin_values.insert(input, read);
    }
    pin_values
}

/// One value a live set makes: the node, by UUID, its input, the member of a bundle input it
/// names, and the value, `None` taking the input's or the member's live value off. A set of no
/// value on a Trigger input fires it once.
#[derive(Debug, Clone, PartialEq)]
pub struct InputValueToSetLive {
    /// The node, by UUID.
    pub node: Uuid,
    /// The input, by name.
    pub input: String,
    /// The member of a bundle input the value is set on, by name; `None` sets the whole input.
    pub member: Option<String>,
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
    /// The value at `set_index` fires a Trigger input of a node that does not run, so the fire
    /// would reach no read.
    TheNodeDoesNotRun {
        /// The index of the refused fire among those asked.
        set_index: usize,
        /// Why the node does not run, as [`crate::core::Error::NodeOfAStreamIsNotRunning`].
        refusal: crate::core::Error,
    },
    /// A helper or island reading one of the values, or a node one fires, was not handed it.
    NotHandedOver(crate::core::Error),
}

/// Whether a live set of `member` of an input, or of the whole input when `member` is `None`,
/// replaces a value an earlier set of the same transaction made on that input, the earlier sets
/// having set `members_set_before`, each `None` for the whole input: a set of the whole input
/// replaces anything set on it, and a set of a member replaces the whole input and that member,
/// while sets of two members of one bundle replace neither.
pub fn a_live_set_replaces_an_earlier_one(
    members_set_before: &[Option<&str>],
    member: Option<&str>,
) -> bool {
    members_set_before
        .iter()
        .any(|member_set| member.is_none() || member_set.is_none() || *member_set == member)
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
