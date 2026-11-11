// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a node keeps in its project's store, under its own UUID: values that outlive its run, a
//! Stop and a restart of the runtime, readable at once and committed by the store's one debounced
//! writer, so a node may write at whatever rate it changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value as JsonValue;
use streamlib_project_repository::{ProjectStore, ProjectStoreTableName};
use uuid::Uuid;

use crate::core::error::{Error, Result};

/// The table every node's own keys live in, each key prefixed by the node's UUID.
const NODE_STORE_TABLE: ProjectStoreTableName = ProjectStoreTableName::named("node_store");

/// The keys one placed node keeps in its project's store; no other node reaches them.
#[derive(Clone)]
pub struct StoreScopedToThisNode {
    project_store: Arc<ProjectStore>,
    node_uuid: Uuid,
}

impl StoreScopedToThisNode {
    pub(crate) fn of_the_node(project_store: Arc<ProjectStore>, node_uuid: Uuid) -> Self {
        Self {
            project_store,
            node_uuid,
        }
    }

    /// The value this node keeps under `key`, the latest written.
    pub fn read(&self, key: &str) -> Result<Option<JsonValue>> {
        self.project_store
            .read(NODE_STORE_TABLE, &self.the_key_in_the_table(key))
            .map_err(|refusal| Error::Runtime(refusal.to_string()))
    }

    /// Keep `value` under `key`: readable at once, on disk within the store's debounce window.
    pub fn write(&self, key: &str, value: &JsonValue) -> Result<()> {
        self.project_store
            .write(NODE_STORE_TABLE, &self.the_key_in_the_table(key), value)
            .map_err(|refusal| Error::Runtime(refusal.to_string()))
    }

    /// Stop keeping `key`.
    pub fn remove(&self, key: &str) {
        self.project_store
            .remove(NODE_STORE_TABLE, &self.the_key_in_the_table(key));
    }

    /// Commit every write the project's store holds waiting, without waiting for its window.
    pub(crate) fn commit_every_waiting_write_now(&self) -> Result<()> {
        self.project_store
            .commit_every_waiting_write_now()
            .map_err(|refusal| Error::Runtime(refusal.to_string()))
    }

    fn the_key_in_the_table(&self, key: &str) -> String {
        format!("{}/{key}", self.node_uuid)
    }
}

/// Every project store this runtime has opened, one per resolved project directory, since a
/// store's file admits one open database per process.
#[derive(Default)]
pub struct ProjectStoresOpenInThisRuntime {
    project_store_by_resolved_directory: Mutex<HashMap<PathBuf, Arc<ProjectStore>>>,
}

impl std::fmt::Debug for ProjectStoresOpenInThisRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectStoresOpenInThisRuntime")
            .field(
                "project_directories",
                &self
                    .project_store_by_resolved_directory
                    .lock()
                    .keys()
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ProjectStoresOpenInThisRuntime {
    /// The store of the project at `project_directory`, opened on its first use.
    pub fn the_store_of_the_project_at(
        &self,
        project_directory: &Path,
    ) -> Result<Arc<ProjectStore>> {
        let resolved_project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        let mut project_store_by_resolved_directory =
            self.project_store_by_resolved_directory.lock();
        if let Some(open) = project_store_by_resolved_directory.get(&resolved_project_directory) {
            return Ok(Arc::clone(open));
        }
        let opened = Arc::new(
            ProjectStore::open_in_the_project_at(&resolved_project_directory)
                .map_err(|refusal| Error::Runtime(refusal.to_string()))?,
        );
        project_store_by_resolved_directory.insert(resolved_project_directory, Arc::clone(&opened));
        Ok(opened)
    }

    /// Commit every write each open store holds waiting, as the runtime shuts down.
    pub(crate) fn commit_every_waiting_write_of_every_store_now(&self) {
        let open_stores: Vec<(PathBuf, Arc<ProjectStore>)> = self
            .project_store_by_resolved_directory
            .lock()
            .iter()
            .map(|(directory, store)| (directory.clone(), Arc::clone(store)))
            .collect();
        for (project_directory, store) in open_stores {
            if let Err(not_committed) = store.commit_every_waiting_write_now() {
                tracing::error!(
                    project_directory = %project_directory.display(),
                    "the runtime shut down and the project's store did not commit what waited: \
                     {not_committed}"
                );
            }
        }
    }
}
