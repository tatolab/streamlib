// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The values Value wires carry into settings, held apart from the graph like the values set
//! live: each setting a wire feeds reads the value that wire last carried, and each node a wire
//! carried a new value into, or took a wire off, waits to be handed what its inputs read now. The
//! node is handed it as it is handed a value set on the setting, so no wire, and no wire taken
//! off, rebuilds a node.

use std::collections::{BTreeSet, HashMap};

use parking_lot::Mutex;
use serde_json::Value;
use uuid::Uuid;

use crate::core::graph::{Graph, LinkUniqueId};

/// One setting a wire feeds: its node's UUID and its name.
type WiredSetting = (Uuid, String);

/// The values the Value wires into one stream's settings carry.
#[derive(Debug, Default)]
pub(crate) struct ValuesWiresCarryIntoSettings {
    held: Mutex<ValuesHeld>,
    arrival: tokio::sync::Notify,
}

#[derive(Debug, Default)]
struct ValuesHeld {
    /// The value each wired setting's wire last carried, by setting.
    carried: HashMap<WiredSetting, Value>,
    /// Each node a wire carried a new value into, or took a wire off, since it was last handed
    /// what its inputs read.
    nodes_to_hand: BTreeSet<Uuid>,
}

impl ValuesWiresCarryIntoSettings {
    /// Hold `value`, which the wire into setting `setting` of `node` carried, as what the setting
    /// reads, and leave `node` to be handed it; a value equal to the one the wire last carried
    /// changes nothing.
    pub(crate) fn carried(&self, node: Uuid, setting: &str, value: Value) {
        let mut held = self.held.lock();
        let key = (node, setting.to_owned());
        if held.carried.get(&key) == Some(&value) {
            return;
        }
        held.carried.insert(key, value);
        held.nodes_to_hand.insert(node);
        drop(held);
        self.arrival.notify_one();
    }

    /// The value the wire into setting `setting` of `node` last carried; `None` when it has
    /// carried none.
    pub(crate) fn the_value_its_wire_carried(&self, node: Uuid, setting: &str) -> Option<Value> {
        self.held
            .lock()
            .carried
            .get(&(node, setting.to_owned()))
            .cloned()
    }

    /// Wait until a node is left to be handed what its inputs read.
    pub(crate) async fn a_node_waits_for_its_values(&self) {
        self.arrival.notified().await;
    }

    /// Take each node left to be handed what its inputs read, in UUID order.
    pub(crate) fn take_each_node_waiting_for_its_values(&self) -> Vec<Uuid> {
        std::mem::take(&mut self.held.lock().nodes_to_hand)
            .into_iter()
            .collect()
    }

    /// Forget every value carried into a setting of `node`, as it leaves the graph.
    pub(crate) fn forget_the_node(&self, node: Uuid) {
        let mut held = self.held.lock();
        held.carried.retain(|(of, _), _| *of != node);
        held.nodes_to_hand.remove(&node);
    }

    /// Forget the value the wire into setting `setting` of `node` carried, as its wire leaves
    /// it, and leave `node` to be handed what the setting reads with no wire, when the wire had
    /// carried one.
    pub(super) fn its_wire_left(&self, node: Uuid, setting: &str) {
        let mut held = self.held.lock();
        if held.carried.remove(&(node, setting.to_owned())).is_none() {
            return;
        }
        held.nodes_to_hand.insert(node);
        drop(held);
        self.arrival.notify_one();
    }
}

/// Forget what link `link_id` of `graph` carried into the setting it feeds, if it feeds one, as
/// it leaves the graph: its node is handed the setting's own value, its live value, else its
/// saved one, else its default.
pub(crate) fn forget_what_a_link_into_a_setting_carried(graph: &Graph, link_id: &LinkUniqueId) {
    let Some(to) = graph
        .traversal()
        .e(link_id)
        .first()
        .map(|link| link.to_port().clone())
    else {
        return;
    };
    let Some(node) = graph.traversal().v(to.node_run_id()).first() else {
        return;
    };
    graph
        .values_wires_carry_into_settings()
        .its_wire_left(node.node_uuid, to.port_name());
}
