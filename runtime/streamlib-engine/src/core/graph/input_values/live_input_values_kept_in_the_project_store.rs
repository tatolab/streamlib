// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The live values of a stream's `kept` inputs, mirrored into one table of its project's store
//! under the node's UUID and the input, so each outlives a Stop, the project's close and a
//! restart of the runtime, and is read back when the project opens. The stream's live values
//! stay the truth; the table only mirrors them.

use std::path::Path;
use std::sync::Arc;

use serde_json::Value;
use streamlib_project_repository::ProjectStore;
use uuid::Uuid;

use crate::core::context::{
    LIVE_INPUT_VALUES_TABLE, ProjectStoresOpenInThisRuntime, key_of_the_node,
    the_node_a_key_leads_with,
};

/// One stream's project's store, where the live values of its `kept` inputs are mirrored. A
/// store that does not open, read or take a write is logged and leaves the live value as set:
/// it lives on while the stream runs and is gone after a restart.
#[derive(Debug)]
pub(crate) struct LiveInputValuesKeptInTheProjectStore {
    project_stores: Arc<ProjectStoresOpenInThisRuntime>,
    project_directory: Arc<Path>,
}

impl LiveInputValuesKeptInTheProjectStore {
    /// The store of the project at `project_directory`, opened through `project_stores` on use.
    pub(crate) fn of_the_project_at(
        project_stores: Arc<ProjectStoresOpenInThisRuntime>,
        project_directory: Arc<Path>,
    ) -> Self {
        Self {
            project_stores,
            project_directory,
        }
    }

    /// Keep `value` as the live value of input `input` of `node`, answering whether the store
    /// holds it now.
    pub(crate) fn keep(&self, node: Uuid, input: &str, value: &Value) -> bool {
        let Some(project_store) = self.the_project_store() else {
            return false;
        };
        match project_store.write(
            LIVE_INPUT_VALUES_TABLE,
            &key_of_the_node(node, input),
            value,
        ) {
            Ok(_) => true,
            Err(not_kept) => {
                tracing::error!(
                    %node,
                    input,
                    "a kept input's live value was not written to the project's store, so a \
                     restart of the runtime does not bring it back: {not_kept}"
                );
                false
            }
        }
    }

    /// Stop keeping a live value of input `input` of `node`.
    pub(crate) fn forget(&self, node: Uuid, input: &str) {
        if let Some(project_store) = self.the_project_store_when_the_project_has_one() {
            project_store.remove(LIVE_INPUT_VALUES_TABLE, &key_of_the_node(node, input));
        }
    }

    /// Each live value the store keeps for an input of a node `is_held` holds, as `(node, input,
    /// value)`.
    pub(crate) fn every_value_kept_for(
        &self,
        is_held: impl Fn(Uuid) -> bool,
    ) -> Vec<(Uuid, String, Value)> {
        let Some(project_store) = self.the_project_store_when_the_project_has_one() else {
            return Vec::new();
        };
        let keys = project_store
            .keys_in(LIVE_INPUT_VALUES_TABLE)
            .unwrap_or_else(|not_listed| {
                tracing::error!(
                    "the kept live values of the project's inputs did not list: {not_listed}"
                );
                Vec::new()
            });
        keys.into_iter()
            .filter_map(|key| {
                let (node, input) = the_node_a_key_leads_with(&key)?;
                if !is_held(node) {
                    return None;
                }
                match project_store.read(LIVE_INPUT_VALUES_TABLE, &key) {
                    Ok(value) => value.map(|value| (node, input.to_owned(), value)),
                    Err(not_read) => {
                        tracing::error!(
                            %node,
                            input,
                            "a kept input's live value was not read back from the project's \
                             store, so the input reads its saved value: {not_read}"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    /// The project's store, opened; `None`, with no store made, for a project that has none
    /// yet and so keeps no value.
    fn the_project_store_when_the_project_has_one(&self) -> Option<Arc<ProjectStore>> {
        ProjectStore::exists_in_the_project_at(&self.project_directory)
            .then(|| self.the_project_store())
            .flatten()
    }

    fn the_project_store(&self) -> Option<Arc<ProjectStore>> {
        self.project_stores
            .the_store_of_the_project_at(&self.project_directory)
            .inspect_err(|not_opened| {
                tracing::error!(
                    project_directory = %self.project_directory.display(),
                    "the project's store did not open, so the kept live values of its inputs \
                     are not mirrored there: {not_opened}"
                );
            })
            .ok()
    }
}
