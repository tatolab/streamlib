// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;

/// What each function node this helper's islands call raised at its last call,
/// by the input port its island feeds and the node's id; a node absent from it
/// ran cleanly at its last call, or has not been called.
#[derive(Debug, Default)]
pub(crate) struct FunctionNodeRaisesInThisHelper {
    raising_because_by_island_input_port_and_node: Mutex<BTreeMap<(String, String), String>>,
}

impl FunctionNodeRaisesInThisHelper {
    /// Note that `function_node_id`, called by the island feeding
    /// `island_input_port`, raised; the words of the first raise of a run of
    /// them are kept.
    pub(crate) fn note_that_it_raised(
        &self,
        island_input_port: &str,
        function_node_id: &str,
        describe_the_raise: impl FnOnce() -> String,
    ) {
        self.raising_because_by_island_input_port_and_node
            .lock()
            .entry((island_input_port.to_string(), function_node_id.to_string()))
            .or_insert_with(describe_the_raise);
    }

    /// Note that `function_node_id`, called by the island feeding
    /// `island_input_port`, ran cleanly, ending a run of raises there.
    pub(crate) fn note_that_it_ran_cleanly(&self, island_input_port: &str, function_node_id: &str) {
        let mut raising_because = self.raising_because_by_island_input_port_and_node.lock();
        if !raising_because.is_empty() {
            raising_because.remove(&(island_input_port.to_string(), function_node_id.to_string()));
        }
    }

    /// Forget each raise of the island feeding `island_input_port`, which no
    /// longer feeds it.
    pub(crate) fn forget_the_island_feeding(&self, island_input_port: &str) {
        self.raising_because_by_island_input_port_and_node
            .lock()
            .retain(|(input_port, _), _| input_port != island_input_port);
    }

    /// What each function node still raising raised, by its id: the words of
    /// the island feeding the first input port, by name, that calls it.
    pub(crate) fn why_each_function_node_is_raising(&self) -> BTreeMap<String, String> {
        let mut raising_because_by_function_node = BTreeMap::new();
        for ((_, function_node_id), raising_because) in self
            .raising_because_by_island_input_port_and_node
            .lock()
            .iter()
        {
            raising_because_by_function_node
                .entry(function_node_id.clone())
                .or_insert_with(|| raising_because.clone());
        }
        raising_because_by_function_node
    }
}

/// Where an island notes each raise of the function nodes it calls: this
/// helper's record, under the input port the island feeds.
#[derive(Clone)]
pub(crate) struct WhereAnIslandNotesTheRaisesOfItsFunctionNodes {
    pub(crate) function_node_raises_in_this_helper: Arc<FunctionNodeRaisesInThisHelper>,
    pub(crate) island_input_port: String,
}

impl WhereAnIslandNotesTheRaisesOfItsFunctionNodes {
    pub(crate) fn note_that_it_raised(
        &self,
        function_node_id: &str,
        describe_the_raise: impl FnOnce() -> String,
    ) {
        self.function_node_raises_in_this_helper
            .note_that_it_raised(
                &self.island_input_port,
                function_node_id,
                describe_the_raise,
            );
    }

    pub(crate) fn note_that_it_ran_cleanly(&self, function_node_id: &str) {
        self.function_node_raises_in_this_helper
            .note_that_it_ran_cleanly(&self.island_input_port, function_node_id);
    }
}

#[cfg(test)]
mod tests;
