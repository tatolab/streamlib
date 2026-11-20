// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The values Value wires carry into settings read at setup, held apart from the graph like the
//! values set live: each value a wire's reader hands in waits for the next rebuild of its node,
//! a burst of them on one setting waiting as its last, and each node is built with the value its
//! wire last carried that it built with.

use std::collections::{BTreeMap, HashMap};

use parking_lot::Mutex;
use serde_json::Value;
use uuid::Uuid;

use super::the_value_a_setting_reads_with_no_wire;
use crate::core::graph::{Graph, LinkUniqueId};

/// One setting a wire feeds: its node's UUID and its name.
type WiredSetting = (Uuid, String);

/// The values the Value wires into one stream's settings read at setup carry.
#[derive(Debug, Default)]
pub(crate) struct ValuesWiresCarryIntoSettings {
    held: Mutex<ValuesHeld>,
    arrival: tokio::sync::Notify,
}

#[derive(Debug, Default)]
struct ValuesHeld {
    /// The value each wired setting's node was last built with, by setting.
    built_with: HashMap<WiredSetting, Value>,
    /// The value each wired setting's wire carried since its node was last built, by setting.
    waiting: BTreeMap<WiredSetting, Value>,
    /// The value each wired setting's node did not build with, until its wire carries another.
    refused: HashMap<WiredSetting, Value>,
}

impl ValuesHeld {
    /// Forget what each setting `holds` keeps.
    fn forget_each_setting_that(&mut self, holds: impl Fn(&WiredSetting) -> bool) {
        self.built_with.retain(|setting, _| !holds(setting));
        self.waiting.retain(|setting, _| !holds(setting));
        self.refused.retain(|setting, _| !holds(setting));
    }
}

/// A value a wire carried into one setting that its node is to be rebuilt with, and the value it
/// was built with before.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ValueAWireCarriedIntoASetting {
    /// The node's UUID.
    pub node: Uuid,
    /// The setting.
    pub setting: String,
    /// The value the wire carried.
    pub value: Value,
    /// The value its node was built with before; `None` when its wire had carried none.
    pub built_with_before: Option<Value>,
}

impl ValuesWiresCarryIntoSettings {
    /// Hold `value`, which the wire into setting `setting` of `node` carried, for the next
    /// rebuild of `node`, in place of any it carried since its last; a value its node was last
    /// built with, or last refused, waits for nothing.
    pub(crate) fn carried(&self, node: Uuid, setting: &str, value: Value) {
        let key = (node, setting.to_owned());
        let mut held = self.held.lock();
        if held.refused.get(&key) == Some(&value) {
            return;
        }
        held.refused.remove(&key);
        let already_waiting_or_built = match held.waiting.get(&key) {
            Some(waiting) => waiting == &value,
            None => held.built_with.get(&key) == Some(&value),
        };
        if already_waiting_or_built {
            return;
        }
        held.waiting.insert(key, value);
        drop(held);
        self.arrival.notify_one();
    }

    /// Hold `value` as what setting `setting` of `node` was built with while its wire has
    /// carried nothing yet: the value it reads with no wire, so a wire carrying the same value
    /// rebuilds nothing.
    pub(crate) fn built_with_unless_its_wire_carried_one(
        &self,
        node: Uuid,
        setting: &str,
        value: Value,
    ) {
        self.held
            .lock()
            .built_with
            .entry((node, setting.to_owned()))
            .or_insert(value);
    }

    /// The value the wire into setting `setting` of `node` carried that its node is built with;
    /// `None` when its wire has carried none.
    pub(crate) fn the_value_its_node_is_built_with(
        &self,
        node: Uuid,
        setting: &str,
    ) -> Option<Value> {
        self.held
            .lock()
            .built_with
            .get(&(node, setting.to_owned()))
            .cloned()
    }

    /// Wait until a wire has carried a value that waits for its node's rebuild.
    pub(crate) async fn a_value_waits(&self) {
        self.arrival.notified().await;
    }

    /// Wake the wait for a value when one waits, as a paused stream resumes.
    pub(crate) fn wake_for_each_value_waiting(&self) {
        if !self.held.lock().waiting.is_empty() {
            self.arrival.notify_one();
        }
    }

    /// Take every value waiting for its node's rebuild, each now what its node is built with.
    pub(crate) fn take_each_value_waiting(&self) -> Vec<ValueAWireCarriedIntoASetting> {
        let mut held = self.held.lock();
        let waiting = std::mem::take(&mut held.waiting);
        waiting
            .into_iter()
            .map(|((node, setting), value)| {
                let built_with_before = held
                    .built_with
                    .insert((node, setting.clone()), value.clone());
                ValueAWireCarriedIntoASetting {
                    node,
                    setting,
                    value,
                    built_with_before,
                }
            })
            .collect()
    }

    /// Hold each value waiting for a rebuild of one of `nodes` as what it is built with, as
    /// those nodes are about to be built: they are built with it, and rebuild for nothing.
    pub(crate) fn build_each_of_the_nodes_with_what_waits(&self, nodes: &[Uuid]) {
        let mut held = self.held.lock();
        let waiting = std::mem::take(&mut held.waiting);
        for ((node, setting), value) in waiting {
            if nodes.contains(&node) {
                held.built_with.insert((node, setting), value);
            } else {
                held.waiting.insert((node, setting), value);
            }
        }
    }

    /// Put back what each of `refused` was built with before, each value it carried held as
    /// refused until its wire carries another.
    pub(crate) fn put_back(&self, refused: &[ValueAWireCarriedIntoASetting]) {
        let mut held = self.held.lock();
        for carried in refused {
            let key = (carried.node, carried.setting.clone());
            match &carried.built_with_before {
                Some(before) => held.built_with.insert(key.clone(), before.clone()),
                None => held.built_with.remove(&key),
            };
            held.refused.insert(key, carried.value.clone());
        }
    }

    /// Forget every value carried into a setting of `node`, as it leaves the graph.
    pub(crate) fn forget_the_node(&self, node: Uuid) {
        self.held
            .lock()
            .forget_each_setting_that(|(of, _)| *of == node);
    }

    /// Forget every value carried into setting `setting` of `node`, as its wire leaves it; when
    /// its node was built with a value the wire carried other than `read_with_no_wire`, what
    /// the setting reads now, that value waits for the node's rebuild.
    pub(crate) fn its_wire_left(
        &self,
        node: Uuid,
        setting: &str,
        read_with_no_wire: Option<Value>,
    ) {
        let key = (node, setting.to_owned());
        let mut held = self.held.lock();
        let built_with = held.built_with.get(&key).cloned();
        held.forget_each_setting_that(|wired| *wired == key);
        let read_with_no_wire = read_with_no_wire.unwrap_or(Value::Null);
        if built_with.is_none_or(|built_with| built_with == read_with_no_wire) {
            return;
        }
        held.waiting.insert(key, read_with_no_wire);
        drop(held);
        self.arrival.notify_one();
    }
}

/// Forget what link `link_id` of `graph` carried into the setting read at setup it feeds, if it
/// feeds one, as it leaves the graph; a node built with what it carried is rebuilt with the
/// setting's own value.
pub(crate) fn forget_what_a_link_into_a_setting_carried(graph: &Graph, link_id: &LinkUniqueId) {
    let Some(to) = graph
        .traversal()
        .e(link_id)
        .first()
        .map(|link| link.to_port().clone())
    else {
        return;
    };
    if !graph.is_a_setting_read_at_setup(to.node_run_id(), to.port_name()) {
        return;
    }
    let Some(node) = graph.traversal().v(to.node_run_id()).first() else {
        return;
    };
    let read_with_no_wire = the_value_a_setting_reads_with_no_wire(graph, node, to.port_name());
    graph.values_wires_carry_into_settings().its_wire_left(
        node.node_uuid,
        to.port_name(),
        read_with_no_wire,
    );
}
