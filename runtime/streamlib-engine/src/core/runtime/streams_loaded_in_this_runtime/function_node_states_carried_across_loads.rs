// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The state of every stateful function node of a stream, by node UUID, as the
//! runtime carries it across a helper restart, a promotion, a reload, a Stop and
//! a restart of the runtime.
//!
//! A helper writes each stateful node's state on its own spawn's board, the
//! one writer an iceoryx2 board admits. What a stream holds here is where each
//! node's state is written now, and what was read of it when the helper
//! writing it went away; a new helper of the stream resumes each node from
//! that. Each state read off a board is kept in the node's own keys of its
//! project's store, so a reload and a restart resume it; Stop forgets it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::core::context::StoreScopedToThisNode;
use crate::core::error::Result;
use crate::iceoryx2::HelperPlacedProcessorLossCounts;

const FUNCTION_NODE_STATE_STORE_KEY: &str = "function_node_state";

/// One stateful function node's state: its state record's fields, and a value
/// for each, in field order.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionNodeState {
    /// The state record's fields, in the order the values travel in.
    pub state_fields: Vec<String>,
    /// One value per field.
    pub state_values: Vec<f64>,
}

/// One function node's state as its project's store keeps it: each value as its f64 bit
/// pattern, so a NaN, an infinity and a negative zero come back exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FunctionNodeStateAsItsStoreKeepsIt {
    state_fields: Vec<String>,
    state_values_as_f64_bits: Vec<u64>,
}

impl FunctionNodeStateAsItsStoreKeepsIt {
    fn of(state: &FunctionNodeState) -> Self {
        Self {
            state_fields: state.state_fields.clone(),
            state_values_as_f64_bits: state
                .state_values
                .iter()
                .map(|value| value.to_bits())
                .collect(),
        }
    }

    fn state(self) -> FunctionNodeState {
        FunctionNodeState {
            state_fields: self.state_fields,
            state_values: self
                .state_values_as_f64_bits
                .into_iter()
                .map(f64::from_bits)
                .collect(),
        }
    }
}

/// The keys each node of one stream keeps in its project's store, by node UUID.
pub type TheStoreOfEachNodeOfAStream =
    Arc<dyn Fn(Uuid) -> Result<StoreScopedToThisNode> + Send + Sync>;

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

    /// Read the state its writer last wrote, and keep it past that writer; answers whether
    /// a state was read.
    fn let_its_writer_go(&mut self) -> bool {
        let Some(written) = self
            .written_now_on
            .take()
            .and_then(|written_now_on| written_now_on.state_written())
        else {
            return false;
        };
        self.state_read_when_its_last_writer_went_away = Some(written);
        true
    }
}

/// The state of every stateful function node of one stream, by node UUID.
#[derive(Default)]
pub struct FunctionNodeStatesCarriedForOneStream {
    by_function_node_uuid: Mutex<BTreeMap<Uuid, FunctionNodeStateCarried>>,
    the_store_of_each_node: Mutex<Option<TheStoreOfEachNodeOfAStream>>,
}

impl std::fmt::Debug for FunctionNodeStatesCarriedForOneStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FunctionNodeStatesCarriedForOneStream")
            .field(
                "function_node_uuids",
                &self.by_function_node_uuid.lock().keys().collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl FunctionNodeStatesCarriedForOneStream {
    /// Keep each state in the node's own keys that `the_store_of_each_node` reaches, for
    /// this load of the stream and after.
    pub fn keep_each_state_in(&self, the_store_of_each_node: TheStoreOfEachNodeOfAStream) {
        *self.the_store_of_each_node.lock() = Some(the_store_of_each_node);
    }

    /// The state the node `function_node_uuid` resumes from, when its stream carries one,
    /// or its store keeps one, of the same `state_fields`; `None` when it starts from its
    /// state record's defaults.
    pub fn the_state_a_function_node_resumes_from(
        &self,
        function_node_uuid: Uuid,
        state_fields: &[String],
    ) -> Option<Vec<f64>> {
        self.the_current_state_of(function_node_uuid)
            .filter(|state| state.state_fields == state_fields)
            .map(|state| state.state_values)
    }

    /// Note that the helper whose board `loss_counts` reads now writes the
    /// state of the node `function_node_uuid`, numbered
    /// `function_node_breadcrumb_number` on its setup command; the state its earlier
    /// writer left stays until the new one writes.
    pub fn note_the_helper_writing_the_state_of_a_function_node(
        &self,
        function_node_uuid: Uuid,
        state_fields: Vec<String>,
        loss_counts: &Arc<HelperPlacedProcessorLossCounts>,
        function_node_breadcrumb_number: u32,
    ) {
        let mut by_function_node_uuid = self.by_function_node_uuid.lock();
        let carried = by_function_node_uuid
            .entry(function_node_uuid)
            .or_insert_with(|| FunctionNodeStateCarried {
                state_read_when_its_last_writer_went_away: self
                    .the_state_the_store_keeps_for(function_node_uuid),
                written_now_on: None,
            });
        if carried.let_its_writer_go() {
            self.keep_the_state_in_the_store(function_node_uuid, carried);
        }
        carried.written_now_on = Some(FunctionNodeStateWrittenOnAHelpersBoard {
            state_fields,
            loss_counts_of_the_helper_writing_it: Arc::downgrade(loss_counts),
            function_node_breadcrumb_number,
        });
    }

    /// Read the state of every node the helper whose board `loss_counts` reads
    /// last wrote, and keep it past that board, in memory and in the store: the
    /// helper went away, or a new spawn's board replaces its own.
    pub fn keep_every_state_the_helper_wrote(
        &self,
        loss_counts: &Arc<HelperPlacedProcessorLossCounts>,
    ) {
        for (function_node_uuid, carried) in self.by_function_node_uuid.lock().iter_mut() {
            if carried
                .written_now_on
                .as_ref()
                .is_some_and(|written_now_on| {
                    written_now_on.is_written_by_the_helper_of(loss_counts)
                })
                && carried.let_its_writer_go()
            {
                self.keep_the_state_in_the_store(*function_node_uuid, carried);
            }
        }
    }

    /// The state of the node `function_node_uuid` as it stands, or as its store keeps it;
    /// `None` before anything wrote or kept one.
    pub fn the_current_state_of(&self, function_node_uuid: Uuid) -> Option<FunctionNodeState> {
        if let Some(carried) = self.by_function_node_uuid.lock().get(&function_node_uuid) {
            return carried.current_state();
        }
        self.the_state_the_store_keeps_for(function_node_uuid)
    }

    /// Keep the state of every node as it stands in the store, as the stream ends.
    pub fn keep_every_state_in_the_store(&self) {
        for (function_node_uuid, carried) in self.by_function_node_uuid.lock().iter() {
            self.keep_the_state_in_the_store(*function_node_uuid, carried);
        }
    }

    /// Forget every state, in memory and in the store, as the stream's playback ends.
    pub fn forget_every_state(&self) {
        let carried_by_function_node_uuid = std::mem::take(&mut *self.by_function_node_uuid.lock());
        for function_node_uuid in carried_by_function_node_uuid.into_keys() {
            self.forget_the_state_in_the_store(function_node_uuid);
        }
    }

    fn the_store_of_the_node(&self, function_node_uuid: Uuid) -> Option<StoreScopedToThisNode> {
        let the_store_of_each_node = self.the_store_of_each_node.lock().clone()?;
        match the_store_of_each_node(function_node_uuid) {
            Ok(store) => Some(store),
            Err(not_opened) => {
                tracing::warn!(
                    "the store of function node {function_node_uuid} did not open, so its state \
                     lives only while this runtime runs: {not_opened}"
                );
                None
            }
        }
    }

    fn the_state_the_store_keeps_for(&self, function_node_uuid: Uuid) -> Option<FunctionNodeState> {
        let kept = match self
            .the_store_of_the_node(function_node_uuid)?
            .read(FUNCTION_NODE_STATE_STORE_KEY)
        {
            Ok(kept) => kept?,
            Err(not_read) => {
                tracing::warn!(
                    "function node {function_node_uuid}'s kept state did not read, so it \
                     starts from its defaults: {not_read}"
                );
                return None;
            }
        };
        match serde_json::from_value::<FunctionNodeStateAsItsStoreKeepsIt>(kept) {
            Ok(kept) => Some(kept.state()),
            Err(not_a_state) => {
                tracing::warn!(
                    "function node {function_node_uuid}'s kept state is not a state record's \
                     fields and values, so it starts from its defaults: {not_a_state}"
                );
                None
            }
        }
    }

    fn keep_the_state_in_the_store(
        &self,
        function_node_uuid: Uuid,
        carried: &FunctionNodeStateCarried,
    ) {
        let Some(state) = carried.current_state() else {
            return;
        };
        let Some(store) = self.the_store_of_the_node(function_node_uuid) else {
            return;
        };
        let written = serde_json::to_value(FunctionNodeStateAsItsStoreKeepsIt::of(&state))
            .map_err(|not_serialized| not_serialized.to_string())
            .and_then(|state| {
                store
                    .write(FUNCTION_NODE_STATE_STORE_KEY, &state)
                    .map_err(|not_written| not_written.to_string())
            });
        if let Err(not_kept) = written {
            tracing::warn!(
                "function node {function_node_uuid}'s state was not kept in its project's \
                 store, so a reload starts it from its defaults: {not_kept}"
            );
        }
    }

    fn forget_the_state_in_the_store(&self, function_node_uuid: Uuid) {
        let Some(store) = self.the_store_of_the_node(function_node_uuid) else {
            return;
        };
        if let Err(not_removed) = store.remove(FUNCTION_NODE_STATE_STORE_KEY) {
            tracing::warn!(
                "function node {function_node_uuid}'s state returns to its defaults at Stop and \
                 its project's store did not forget it: {not_removed}"
            );
        }
    }
}

/// The function-node states of every stream this runtime loaded, by stream id,
/// kept across each load of a stream.
#[derive(Default)]
pub struct FunctionNodeStatesCarriedForEveryStream {
    by_stream_id: Mutex<HashMap<String, Arc<FunctionNodeStatesCarriedForOneStream>>>,
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

    /// Forget every state of the stream `stream_id`, as its playback ends or its record
    /// is forgotten.
    pub fn forget_every_state_of_the_stream(&self, stream_id: &str) {
        if let Some(states) = self.by_stream_id.lock().get(stream_id).cloned() {
            states.forget_every_state();
        }
    }

    /// Keep the state of every node of the stream `stream_id`, which has ended, in its store.
    pub fn keep_the_states_of_a_stream_that_ended(&self, stream_id: &str) {
        if let Some(states) = self.by_stream_id.lock().get(stream_id).cloned() {
            states.keep_every_state_in_the_store();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context::ProjectStoresOpenInThisRuntime;

    fn the_lfo_at(phase: f64) -> FunctionNodeState {
        FunctionNodeState {
            state_fields: vec!["phase".to_string()],
            state_values: vec![phase],
        }
    }

    struct StatesOfAStreamOfAProjectOnDisk {
        _project_directory: tempfile::TempDir,
        project_stores: Arc<ProjectStoresOpenInThisRuntime>,
        states: FunctionNodeStatesCarriedForOneStream,
        the_store_of_each_node: TheStoreOfEachNodeOfAStream,
    }

    fn the_states_of_a_stream_of_a_project_on_disk() -> StatesOfAStreamOfAProjectOnDisk {
        let project_directory = tempfile::tempdir().expect("a temporary project");
        let project_stores = Arc::new(ProjectStoresOpenInThisRuntime::default());
        let the_store_of_each_node: TheStoreOfEachNodeOfAStream = {
            let project_stores = Arc::clone(&project_stores);
            let project_directory = project_directory.path().to_path_buf();
            Arc::new(move |node_uuid| {
                Ok(StoreScopedToThisNode::of_the_node(
                    project_stores.the_store_of_the_project_at(&project_directory)?,
                    node_uuid,
                ))
            })
        };
        let states = FunctionNodeStatesCarriedForOneStream::default();
        states.keep_each_state_in(Arc::clone(&the_store_of_each_node));
        StatesOfAStreamOfAProjectOnDisk {
            _project_directory: project_directory,
            project_stores,
            states,
            the_store_of_each_node,
        }
    }

    impl StatesOfAStreamOfAProjectOnDisk {
        fn keep_in_the_store(&self, node_uuid: Uuid, state: &FunctionNodeState) {
            (self.the_store_of_each_node)(node_uuid)
                .expect("the store opens")
                .write(
                    FUNCTION_NODE_STATE_STORE_KEY,
                    &serde_json::to_value(FunctionNodeStateAsItsStoreKeepsIt::of(state))
                        .expect("serializes"),
                )
                .expect("writes");
        }

        fn the_state_the_store_keeps(&self, node_uuid: Uuid) -> Option<FunctionNodeState> {
            (self.the_store_of_each_node)(node_uuid)
                .expect("the store opens")
                .read(FUNCTION_NODE_STATE_STORE_KEY)
                .expect("reads")
                .map(|kept| {
                    serde_json::from_value::<FunctionNodeStateAsItsStoreKeepsIt>(kept)
                        .expect("a state")
                        .state()
                })
        }

        fn note_a_helper_writing(&self, node_uuid: Uuid) {
            self.states
                .note_the_helper_writing_the_state_of_a_function_node(
                    node_uuid,
                    vec!["phase".to_string()],
                    &Arc::new(HelperPlacedProcessorLossCounts::default()),
                    1,
                );
        }
    }

    #[test]
    fn a_node_resumes_from_the_state_its_store_keeps_only_when_its_fields_are_the_same() {
        let stream = the_states_of_a_stream_of_a_project_on_disk();
        let lfo = Uuid::new_v4();
        stream.keep_in_the_store(lfo, &the_lfo_at(0.25));
        assert_eq!(
            stream
                .states
                .the_state_a_function_node_resumes_from(lfo, &["phase".to_string()]),
            Some(vec![0.25])
        );
        assert_eq!(
            stream
                .states
                .the_state_a_function_node_resumes_from(lfo, &["angle".to_string()]),
            None,
            "a state record whose fields changed starts from its defaults"
        );
        assert_eq!(
            stream
                .states
                .the_state_a_function_node_resumes_from(Uuid::new_v4(), &["phase".to_string()]),
            None
        );
    }

    #[test]
    fn stop_forgets_every_state_in_memory_and_in_the_store() {
        let stream = the_states_of_a_stream_of_a_project_on_disk();
        let lfo = Uuid::new_v4();
        stream.keep_in_the_store(lfo, &the_lfo_at(0.25));
        stream.note_a_helper_writing(lfo);

        stream.states.forget_every_state();
        assert_eq!(stream.the_state_the_store_keeps(lfo), None);
        assert_eq!(stream.states.the_current_state_of(lfo), None);
    }

    #[test]
    fn a_stream_loaded_again_resumes_each_state_its_last_load_kept_in_the_store() {
        let stream = the_states_of_a_stream_of_a_project_on_disk();
        let lfo = Uuid::new_v4();
        stream.keep_in_the_store(lfo, &the_lfo_at(0.5));
        stream.note_a_helper_writing(lfo);
        stream.states.keep_every_state_in_the_store();
        stream
            .project_stores
            .commit_every_waiting_write_of_every_store_now();
        let StatesOfAStreamOfAProjectOnDisk {
            _project_directory: project_directory,
            project_stores,
            states,
            the_store_of_each_node,
        } = stream;
        drop((states, the_store_of_each_node, project_stores));

        let project_stores_of_the_restarted_runtime =
            Arc::new(ProjectStoresOpenInThisRuntime::default());
        let loaded_again = FunctionNodeStatesCarriedForOneStream::default();
        loaded_again.keep_each_state_in({
            let project_stores = Arc::clone(&project_stores_of_the_restarted_runtime);
            let project_directory = project_directory.path().to_path_buf();
            Arc::new(move |node_uuid| {
                Ok(StoreScopedToThisNode::of_the_node(
                    project_stores.the_store_of_the_project_at(&project_directory)?,
                    node_uuid,
                ))
            })
        });
        assert_eq!(
            loaded_again.the_current_state_of(lfo),
            Some(the_lfo_at(0.5)),
            "a restarted runtime reads the state back off the project's database"
        );
    }

    #[test]
    fn a_state_the_store_keeps_reads_back_bit_for_bit() {
        let stream = the_states_of_a_stream_of_a_project_on_disk();
        let lfo = Uuid::new_v4();
        let state = FunctionNodeState {
            state_fields: vec!["phase".into(), "held".into(), "floor".into()],
            state_values: vec![0.1 + 0.2, f64::NEG_INFINITY, -0.0],
        };
        stream.keep_in_the_store(lfo, &state);
        let read_back = stream
            .states
            .the_current_state_of(lfo)
            .expect("a kept state");
        assert_eq!(read_back.state_fields, state.state_fields);
        assert_eq!(
            read_back
                .state_values
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            state
                .state_values
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn a_stream_given_no_store_carries_its_states_only_in_memory() {
        let states = FunctionNodeStatesCarriedForOneStream::default();
        let lfo = Uuid::new_v4();
        states.note_the_helper_writing_the_state_of_a_function_node(
            lfo,
            vec!["phase".to_string()],
            &Arc::new(HelperPlacedProcessorLossCounts::default()),
            1,
        );
        states.forget_every_state();
        assert_eq!(states.the_current_state_of(lfo), None);
    }

    #[test]
    fn a_stream_loaded_again_carries_the_same_states() {
        let every_stream = FunctionNodeStatesCarriedForEveryStream::default();
        assert!(Arc::ptr_eq(
            &every_stream.of_the_stream("my-stream"),
            &every_stream.of_the_stream("my-stream")
        ));
    }
}
