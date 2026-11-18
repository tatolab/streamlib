// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde_json::Value as JsonValue;

use super::{JsonSerializableComponent, ObservableNodeState};
use crate::core::nodes::NodeState;

/// How the helpers evaluating a function node ran it: whether compiled, as the
/// last helper to finish its setup reported it, and what its calls raised, as
/// each helper calling it last reported, shown on the node's own state.
pub struct HowItsHelpersRanAFunctionNode {
    ran_compiled: Mutex<Option<bool>>,
    raising_because_by_helper_number: Mutex<BTreeMap<u64, String>>,
    function_node_state: Arc<ObservableNodeState>,
}

impl std::fmt::Debug for HowItsHelpersRanAFunctionNode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HowItsHelpersRanAFunctionNode")
            .field("ran_compiled", &self.ran_compiled())
            .field(
                "raising_because_by_helper_number",
                &*self.raising_because_by_helper_number.lock(),
            )
            .finish_non_exhaustive()
    }
}

impl Default for HowItsHelpersRanAFunctionNode {
    fn default() -> Self {
        Self::showing_its_raises_on(Arc::new(ObservableNodeState::new(NodeState::Pending)))
    }
}

impl HowItsHelpersRanAFunctionNode {
    /// The report of a function node whose raises `graph` shows as the
    /// `process_raising_because` of `function_node_state`.
    pub fn showing_its_raises_on(function_node_state: Arc<ObservableNodeState>) -> Self {
        Self {
            ran_compiled: Mutex::new(None),
            raising_because_by_helper_number: Mutex::new(BTreeMap::new()),
            function_node_state,
        }
    }

    /// Note how a helper evaluating the node ran it: compiled to native code,
    /// or as Python in its interpreter.
    pub fn note_it_ran_compiled(&self, ran_compiled: bool) {
        *self.ran_compiled.lock() = Some(ran_compiled);
    }

    /// Whether the node ran compiled; `None` until a helper evaluating it has
    /// set up.
    pub fn ran_compiled(&self) -> Option<bool> {
        *self.ran_compiled.lock()
    }

    /// Note what the node's calls in the helper numbered `helper_number` raised,
    /// or `None` once they run cleanly there; the node shows the raise of the
    /// lowest-numbered helper still reporting one.
    pub fn note_why_a_helper_finds_it_raising(
        &self,
        helper_number: u64,
        raising_because: Option<String>,
    ) {
        let mut raising_because_by_helper_number = self.raising_because_by_helper_number.lock();
        match raising_because {
            Some(raising_because) => {
                raising_because_by_helper_number.insert(helper_number, raising_because);
            }
            None => {
                raising_because_by_helper_number.remove(&helper_number);
            }
        }
        self.function_node_state
            .note_why_process_is_raising(raising_because_by_helper_number.values().next().cloned());
    }
}

/// Each function node one reader's islands call, by its node id, and the
/// number its helper's raise reports are kept under on each node's report.
#[derive(Debug)]
pub struct HowOneReadersHelperRanEachFunctionNode {
    helper_number: u64,
    how_each_function_node_ran: Mutex<HashMap<String, Arc<HowItsHelpersRanAFunctionNode>>>,
}

impl Default for HowOneReadersHelperRanEachFunctionNode {
    fn default() -> Self {
        static NEXT_HELPER_NUMBER: AtomicU64 = AtomicU64::new(0);
        Self {
            helper_number: NEXT_HELPER_NUMBER.fetch_add(1, Ordering::Relaxed),
            how_each_function_node_ran: Mutex::new(HashMap::new()),
        }
    }
}

impl HowOneReadersHelperRanEachFunctionNode {
    /// Report how the helper runs each of `function_nodes`, by node id, to the
    /// report given beside it.
    pub fn report_each_function_node_to(
        &self,
        function_nodes: impl IntoIterator<Item = (String, Arc<HowItsHelpersRanAFunctionNode>)>,
    ) {
        self.how_each_function_node_ran
            .lock()
            .extend(function_nodes);
    }

    /// Note whether the helper ran `function_node_id` compiled; one the reader's
    /// islands do not call is passed over.
    pub fn note_it_ran_compiled(&self, function_node_id: &str, ran_compiled: bool) {
        if let Some(how_it_ran) = self.how_each_function_node_ran.lock().get(function_node_id) {
            how_it_ran.note_it_ran_compiled(ran_compiled);
        }
    }

    /// Note what `function_node_id` raised in the helper, or `None` once it runs
    /// cleanly there; one the reader's islands do not call is passed over.
    pub fn note_why_the_helper_finds_it_raising(
        &self,
        function_node_id: &str,
        raising_because: Option<String>,
    ) {
        if let Some(how_it_ran) = self.how_each_function_node_ran.lock().get(function_node_id) {
            how_it_ran.note_why_a_helper_finds_it_raising(self.helper_number, raising_because);
        }
    }

    /// Forget every raise the helper reported, as a helper that is gone no
    /// longer calls any node.
    pub fn forget_every_raise_the_helper_reported(&self) {
        for how_it_ran in self.how_each_function_node_ran.lock().values() {
            how_it_ran.note_why_a_helper_finds_it_raising(self.helper_number, None);
        }
    }
}

/// How the helpers evaluating a function node ran it, rendered by `graph` as
/// `ran_compiled`: `true` compiled, `false` as Python, `null` before any has
/// set up.
pub struct FunctionNodeRanCompiledComponent(pub Arc<HowItsHelpersRanAFunctionNode>);

impl JsonSerializableComponent for FunctionNodeRanCompiledComponent {
    fn json_key(&self) -> &'static str {
        "ran_compiled"
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::from(self.0.ran_compiled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_renders_null_until_a_helper_reports_then_how_the_last_ran_it() {
        let how_it_ran = Arc::new(HowItsHelpersRanAFunctionNode::default());
        let component = FunctionNodeRanCompiledComponent(Arc::clone(&how_it_ran));
        assert_eq!(component.to_json(), JsonValue::Null);
        how_it_ran.note_it_ran_compiled(false);
        assert_eq!(component.to_json(), JsonValue::Bool(false));
        how_it_ran.note_it_ran_compiled(true);
        assert_eq!(component.to_json(), JsonValue::Bool(true));
    }

    fn a_function_node_reported_by_two_readers() -> (
        Arc<ObservableNodeState>,
        HowOneReadersHelperRanEachFunctionNode,
        HowOneReadersHelperRanEachFunctionNode,
    ) {
        let function_node_state = Arc::new(ObservableNodeState::new(NodeState::Running));
        let how_it_ran = Arc::new(HowItsHelpersRanAFunctionNode::showing_its_raises_on(
            Arc::clone(&function_node_state),
        ));
        let first_reader = HowOneReadersHelperRanEachFunctionNode::default();
        let second_reader = HowOneReadersHelperRanEachFunctionNode::default();
        for reader in [&first_reader, &second_reader] {
            reader.report_each_function_node_to([("beat".to_string(), Arc::clone(&how_it_ran))]);
        }
        (function_node_state, first_reader, second_reader)
    }

    #[test]
    fn a_raise_a_readers_helper_reports_shows_on_the_function_node_until_it_runs_cleanly() {
        let (function_node_state, first_reader, _) = a_function_node_reported_by_two_readers();
        first_reader.note_why_the_helper_finds_it_raising(
            "beat",
            Some("raised RuntimeError: broke (nodes/beat.py, line 3)".to_string()),
        );
        assert_eq!(
            function_node_state.why_process_is_raising().as_deref(),
            Some("raised RuntimeError: broke (nodes/beat.py, line 3)")
        );
        first_reader.note_why_the_helper_finds_it_raising("beat", None);
        assert_eq!(function_node_state.why_process_is_raising(), None);
    }

    #[test]
    fn the_function_node_keeps_showing_a_raise_while_any_readers_helper_still_reports_one() {
        let (function_node_state, first_reader, second_reader) =
            a_function_node_reported_by_two_readers();
        first_reader.note_why_the_helper_finds_it_raising("beat", Some("first".to_string()));
        second_reader.note_why_the_helper_finds_it_raising("beat", Some("second".to_string()));
        first_reader.note_why_the_helper_finds_it_raising("beat", None);
        assert_eq!(
            function_node_state.why_process_is_raising().as_deref(),
            Some("second")
        );
        second_reader.forget_every_raise_the_helper_reported();
        assert_eq!(function_node_state.why_process_is_raising(), None);
    }

    #[test]
    fn a_raise_reported_for_a_node_the_readers_islands_do_not_call_is_passed_over() {
        let (function_node_state, first_reader, _) = a_function_node_reported_by_two_readers();
        first_reader.note_why_the_helper_finds_it_raising("warmth", Some("broke".to_string()));
        assert_eq!(function_node_state.why_process_is_raising(), None);
    }
}
