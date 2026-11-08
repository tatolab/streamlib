// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The state of every stateful function node of a stream, by node name, as the
//! runtime carries it across a helper restart, a promotion, a reload and a
//! restart of the runtime.
//!
//! A helper writes each stateful node's state on its own spawn's board, the
//! one writer an iceoryx2 board admits. What a stream holds here is where each
//! node's state is written now, and what was read of it when the helper
//! writing it went away; a new helper of the stream resumes each node from
//! that, and the stream's record keeps it past the stream's end.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock, Weak};

use parking_lot::Mutex;

use crate::iceoryx2::HelperPlacedProcessorLossCounts;

/// One stateful function node's state: its state record's fields, and a value
/// for each, in field order.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionNodeState {
    /// The state record's fields, in the order the values travel in.
    pub state_fields: Vec<String>,
    /// One value per field.
    pub state_values: Vec<f64>,
}

/// Where a function node's state is written now: its entry on the board of
/// the helper calling it.
struct FunctionNodeStateWrittenOnAHelpersBoard {
    state_fields: Vec<String>,
    loss_counts_of_the_helper_writing_it: Weak<HelperPlacedProcessorLossCounts>,
    function_node_breadcrumb_number: u32,
}

impl FunctionNodeStateWrittenOnAHelpersBoard {
    fn state_written(&self) -> Option<FunctionNodeState> {
        let loss_counts = self.loss_counts_of_the_helper_writing_it.upgrade()?;
        let entry = loss_counts
            .function_node_state_its_helper_wrote(self.function_node_breadcrumb_number)?;
        let state_values = entry.state_values_written()?;
        (state_values.len() == self.state_fields.len()).then(|| FunctionNodeState {
            state_fields: self.state_fields.clone(),
            state_values: state_values.to_vec(),
        })
    }

    fn is_written_by_the_helper_of(
        &self,
        loss_counts: &Arc<HelperPlacedProcessorLossCounts>,
    ) -> bool {
        std::ptr::eq(
            self.loss_counts_of_the_helper_writing_it.as_ptr(),
            Arc::as_ptr(loss_counts),
        )
    }
}

/// One function node's state as its stream carries it.
#[derive(Default)]
struct FunctionNodeStateCarried {
    state_read_when_its_last_writer_went_away: Option<FunctionNodeState>,
    written_now_on: Option<FunctionNodeStateWrittenOnAHelpersBoard>,
}

impl FunctionNodeStateCarried {
    fn current_state(&self) -> Option<FunctionNodeState> {
        self.written_now_on
            .as_ref()
            .and_then(FunctionNodeStateWrittenOnAHelpersBoard::state_written)
            .or_else(|| self.state_read_when_its_last_writer_went_away.clone())
    }

    fn let_its_writer_go(&mut self) {
        if let Some(written) = self
            .written_now_on
            .take()
            .and_then(|written_now_on| written_now_on.state_written())
        {
            self.state_read_when_its_last_writer_went_away = Some(written);
        }
    }
}

/// The state of every stateful function node of one stream, by node name.
#[derive(Default)]
pub struct FunctionNodeStatesCarriedForOneStream {
    by_function_node_name: Mutex<BTreeMap<String, FunctionNodeStateCarried>>,
}

impl std::fmt::Debug for FunctionNodeStatesCarriedForOneStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FunctionNodeStatesCarriedForOneStream")
            .field("states", &self.every_current_state())
            .finish()
    }
}

impl FunctionNodeStatesCarriedForOneStream {
    /// The state the node named `function_node_name` resumes from, when its
    /// stream carries one of the same `state_fields`; `None` when it starts
    /// from its state record's defaults.
    pub fn the_state_a_function_node_resumes_from(
        &self,
        function_node_name: &str,
        state_fields: &[String],
    ) -> Option<Vec<f64>> {
        self.by_function_node_name
            .lock()
            .get(function_node_name)?
            .current_state()
            .filter(|state| state.state_fields == state_fields)
            .map(|state| state.state_values)
    }

    /// Note that the helper whose board `loss_counts` reads now writes the
    /// state of the node named `function_node_name`, numbered
    /// `function_node_breadcrumb_number` on its setup command; the state its
    /// earlier writer left stays until the new one writes.
    pub fn note_the_helper_writing_the_state_of_a_function_node(
        &self,
        function_node_name: &str,
        state_fields: Vec<String>,
        loss_counts: &Arc<HelperPlacedProcessorLossCounts>,
        function_node_breadcrumb_number: u32,
    ) {
        let mut by_function_node_name = self.by_function_node_name.lock();
        let carried = by_function_node_name
            .entry(function_node_name.to_string())
            .or_default();
        carried.let_its_writer_go();
        carried.written_now_on = Some(FunctionNodeStateWrittenOnAHelpersBoard {
            state_fields,
            loss_counts_of_the_helper_writing_it: Arc::downgrade(loss_counts),
            function_node_breadcrumb_number,
        });
    }

    /// Read the state of every node the helper whose board `loss_counts` reads
    /// last wrote, and keep it past that board: the helper went away, or a new
    /// spawn's board replaces its own.
    pub fn keep_every_state_the_helper_wrote(
        &self,
        loss_counts: &Arc<HelperPlacedProcessorLossCounts>,
    ) {
        for carried in self.by_function_node_name.lock().values_mut() {
            if carried
                .written_now_on
                .as_ref()
                .is_some_and(|written_now_on| {
                    written_now_on.is_written_by_the_helper_of(loss_counts)
                })
            {
                carried.let_its_writer_go();
            }
        }
    }

    /// The state of the node named `function_node_name` as it stands; `None`
    /// before anything wrote or carried one.
    pub fn the_current_state_of(&self, function_node_name: &str) -> Option<FunctionNodeState> {
        self.by_function_node_name
            .lock()
            .get(function_node_name)?
            .current_state()
    }

    /// The state of every node as it stands, by node name.
    pub fn every_current_state(&self) -> BTreeMap<String, FunctionNodeState> {
        self.by_function_node_name
            .lock()
            .iter()
            .filter_map(|(function_node_name, carried)| {
                Some((function_node_name.clone(), carried.current_state()?))
            })
            .collect()
    }

    fn carrying(states_by_function_node_name: BTreeMap<String, FunctionNodeState>) -> Self {
        Self {
            by_function_node_name: Mutex::new(
                states_by_function_node_name
                    .into_iter()
                    .map(|(function_node_name, state)| {
                        (
                            function_node_name,
                            FunctionNodeStateCarried {
                                state_read_when_its_last_writer_went_away: Some(state),
                                written_now_on: None,
                            },
                        )
                    })
                    .collect(),
            ),
        }
    }
}

/// What records each stream's function-node states once the stream ends.
pub type RecordTheFunctionNodeStatesOfAStreamAtItsEnd =
    Arc<dyn Fn(&str, BTreeMap<String, FunctionNodeState>) + Send + Sync>;

/// The function-node states of every stream this runtime loaded, by stream id,
/// kept across each load of a stream and recorded at each stream's end.
#[derive(Default)]
pub struct FunctionNodeStatesCarriedForEveryStream {
    by_stream_id: Mutex<HashMap<String, Arc<FunctionNodeStatesCarriedForOneStream>>>,
    record_the_states_of_a_stream_at_its_end:
        OnceLock<RecordTheFunctionNodeStatesOfAStreamAtItsEnd>,
}

impl FunctionNodeStatesCarriedForEveryStream {
    /// The states the stream `stream_id` carries, the same for every load of it.
    pub fn of_the_stream(&self, stream_id: &str) -> Arc<FunctionNodeStatesCarriedForOneStream> {
        Arc::clone(
            self.by_stream_id
                .lock()
                .entry(stream_id.to_string())
                .or_default(),
        )
    }

    /// Carry `recorded` for the stream `stream_id`, read off its record, unless
    /// this runtime already carries states for it.
    pub fn carry_the_recorded_states_unless_some_are_carried(
        &self,
        stream_id: &str,
        recorded: BTreeMap<String, FunctionNodeState>,
    ) {
        self.by_stream_id
            .lock()
            .entry(stream_id.to_string())
            .or_insert_with(|| Arc::new(FunctionNodeStatesCarriedForOneStream::carrying(recorded)));
    }

    /// Forget every state of the stream `stream_id`, once its record is.
    pub fn forget_the_stream(&self, stream_id: &str) {
        self.by_stream_id.lock().remove(stream_id);
    }

    /// Record each stream's states at its end through `record`; refused, keeping
    /// the first, a second time.
    pub fn record_the_states_of_each_stream_at_its_end_through(
        &self,
        record: RecordTheFunctionNodeStatesOfAStreamAtItsEnd,
    ) -> bool {
        self.record_the_states_of_a_stream_at_its_end
            .set(record)
            .is_ok()
    }

    /// Record the states of the stream `stream_id`, which has ended.
    pub fn record_the_states_of_a_stream_that_ended(&self, stream_id: &str) {
        let Some(record) = self.record_the_states_of_a_stream_at_its_end.get() else {
            return;
        };
        let Some(states) = self.by_stream_id.lock().get(stream_id).cloned() else {
            return;
        };
        record(stream_id, states.every_current_state());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(state_values: &[f64]) -> FunctionNodeState {
        FunctionNodeState {
            state_fields: vec!["phase".to_string()],
            state_values: state_values.to_vec(),
        }
    }

    #[test]
    fn a_node_resumes_from_the_state_its_stream_carries_only_when_its_fields_are_the_same() {
        let carried = FunctionNodeStatesCarriedForOneStream::carrying(BTreeMap::from([(
            "lfo".to_string(),
            state(&[0.25]),
        )]));
        assert_eq!(
            carried.the_state_a_function_node_resumes_from("lfo", &["phase".to_string()]),
            Some(vec![0.25])
        );
        assert_eq!(
            carried.the_state_a_function_node_resumes_from("lfo", &["angle".to_string()]),
            None,
            "a state record whose fields changed starts from its defaults"
        );
        assert_eq!(
            carried.the_state_a_function_node_resumes_from("lag", &["phase".to_string()]),
            None
        );
    }

    #[test]
    fn a_stream_loaded_again_carries_the_same_states_and_a_record_never_replaces_them() {
        let every_stream = FunctionNodeStatesCarriedForEveryStream::default();
        every_stream.carry_the_recorded_states_unless_some_are_carried(
            "my-stream",
            BTreeMap::from([("lfo".to_string(), state(&[0.25]))]),
        );
        every_stream.carry_the_recorded_states_unless_some_are_carried(
            "my-stream",
            BTreeMap::from([("lfo".to_string(), state(&[0.75]))]),
        );
        assert!(Arc::ptr_eq(
            &every_stream.of_the_stream("my-stream"),
            &every_stream.of_the_stream("my-stream")
        ));
        assert_eq!(
            every_stream
                .of_the_stream("my-stream")
                .the_current_state_of("lfo"),
            Some(state(&[0.25]))
        );
        every_stream.forget_the_stream("my-stream");
        assert_eq!(
            every_stream
                .of_the_stream("my-stream")
                .the_current_state_of("lfo"),
            None
        );
    }

    #[test]
    fn a_streams_states_are_recorded_at_its_end_through_what_records_them() {
        let every_stream = FunctionNodeStatesCarriedForEveryStream::default();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let recorded_by_the_hook = Arc::clone(&recorded);
        assert!(
            every_stream.record_the_states_of_each_stream_at_its_end_through(Arc::new(
                move |stream_id, states| recorded_by_the_hook
                    .lock()
                    .push((stream_id.to_string(), states))
            ))
        );
        every_stream.carry_the_recorded_states_unless_some_are_carried(
            "my-stream",
            BTreeMap::from([("lfo".to_string(), state(&[0.5]))]),
        );
        every_stream.record_the_states_of_a_stream_that_ended("my-stream");
        every_stream.record_the_states_of_a_stream_that_ended("a-stream-never-loaded");
        assert_eq!(
            *recorded.lock(),
            vec![(
                "my-stream".to_string(),
                BTreeMap::from([("lfo".to_string(), state(&[0.5]))])
            )]
        );
    }
}
