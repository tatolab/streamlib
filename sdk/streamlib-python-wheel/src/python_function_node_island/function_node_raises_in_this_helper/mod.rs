// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;

/// A function node's call that failed, as the graph shows it and the log
/// records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionNodeRaiseDescribed {
    /// What the call raised, in the words the graph shows on the node.
    pub(crate) raising_because: String,
    /// The Python traceback of the raise; `None` for a call refused outside
    /// Python.
    pub(crate) traceback: Option<String>,
}

/// The first raise of a run of a function node's raises, noted for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FunctionNodeRaiseNewlyNoted {
    pub(crate) function_node_id: String,
    pub(crate) raise: FunctionNodeRaiseDescribed,
}

#[derive(Debug, Default)]
struct FunctionNodeRaisesNoted {
    raising_because_by_island_input_port_and_node: BTreeMap<(String, String), String>,
    raises_newly_noted_since_last_taken: Vec<FunctionNodeRaiseNewlyNoted>,
}

/// What each function node this helper's islands call raised at its last call,
/// by the input port its island feeds and the node's id; a node absent from it
/// ran cleanly at its last call, or has not been called.
#[derive(Debug, Default)]
pub(crate) struct FunctionNodeRaisesInThisHelper {
    noted: Mutex<FunctionNodeRaisesNoted>,
}

impl FunctionNodeRaisesInThisHelper {
    /// Note that `function_node_id`, called by the island feeding
    /// `island_input_port`, raised. The words of the first raise of a run of
    /// them are kept, and a raise of a node no island of this helper finds
    /// raising is held for the log until taken; `describe_the_raise` is called
    /// only when the island holds no raise of the node.
    pub(crate) fn note_that_it_raised(
        &self,
        island_input_port: &str,
        function_node_id: &str,
        describe_the_raise: impl FnOnce() -> FunctionNodeRaiseDescribed,
    ) {
        let mut noted = self.noted.lock();
        let key = (island_input_port.to_string(), function_node_id.to_string());
        if noted
            .raising_because_by_island_input_port_and_node
            .contains_key(&key)
        {
            return;
        }
        let another_island_finds_it_raising = noted
            .raising_because_by_island_input_port_and_node
            .keys()
            .any(|(_, raising_function_node_id)| raising_function_node_id == function_node_id);
        let raise = describe_the_raise();
        noted
            .raising_because_by_island_input_port_and_node
            .insert(key, raise.raising_because.clone());
        if !another_island_finds_it_raising {
            noted
                .raises_newly_noted_since_last_taken
                .push(FunctionNodeRaiseNewlyNoted {
                    function_node_id: function_node_id.to_string(),
                    raise,
                });
        }
    }

    /// Note that `function_node_id`, called by the island feeding
    /// `island_input_port`, ran cleanly, ending a run of raises there.
    pub(crate) fn note_that_it_ran_cleanly(&self, island_input_port: &str, function_node_id: &str) {
        let mut noted = self.noted.lock();
        if !noted
            .raising_because_by_island_input_port_and_node
            .is_empty()
        {
            noted
                .raising_because_by_island_input_port_and_node
                .remove(&(island_input_port.to_string(), function_node_id.to_string()));
        }
    }

    /// Forget each raise of the island feeding `island_input_port`, which no
    /// longer feeds it, or is replaced.
    pub(crate) fn forget_the_island_feeding(&self, island_input_port: &str) {
        self.noted
            .lock()
            .raising_because_by_island_input_port_and_node
            .retain(|(input_port, _), _| input_port != island_input_port);
    }

    /// What each function node still raising raised, by its id: the words of
    /// the island feeding the first input port, by name, that calls it.
    pub(crate) fn why_each_function_node_is_raising(&self) -> BTreeMap<String, String> {
        let mut raising_because_by_function_node = BTreeMap::new();
        for ((_, function_node_id), raising_because) in self
            .noted
            .lock()
            .raising_because_by_island_input_port_and_node
            .iter()
        {
            raising_because_by_function_node
                .entry(function_node_id.clone())
                .or_insert_with(|| raising_because.clone());
        }
        raising_because_by_function_node
    }

    /// Each first raise of a run noted since the last take, in the order noted.
    pub(crate) fn take_each_raise_newly_noted(&self) -> Vec<FunctionNodeRaiseNewlyNoted> {
        std::mem::take(&mut self.noted.lock().raises_newly_noted_since_last_taken)
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
        describe_the_raise: impl FnOnce() -> FunctionNodeRaiseDescribed,
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
