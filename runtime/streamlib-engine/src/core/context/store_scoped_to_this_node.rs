// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! What a node keeps in its project's store, under its own UUID: values that outlive its run, a
//! Stop and a restart of the runtime, readable at once and committed by the store's one debounced
//! writer, so a node may write at whatever rate it changes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};

use parking_lot::Mutex;
use serde_json::Value as JsonValue;
use streamlib_project_repository::{ProjectStore, ProjectStoreTableName};
use uuid::Uuid;

use crate::core::error::{Error, Result};

/// The table every node's own keys live in, each key prefixed by the node's UUID.
const NODE_STORE_TABLE: ProjectStoreTableName = ProjectStoreTableName::named("node_store");

/// The keys one placed node keeps in its project's store; no other node reaches them. It holds
/// the store only while its project does, so a handle that outlives its project's close keeps no
/// store file open and refuses every call.
#[derive(Clone, Debug)]
pub struct StoreScopedToThisNode {
    project_store_while_its_project_is_open: Weak<ProjectStore>,
    node_uuid: Uuid,
}

impl StoreScopedToThisNode {
    pub(crate) fn of_the_node(project_store: Arc<ProjectStore>, node_uuid: Uuid) -> Self {
        Self {
            project_store_while_its_project_is_open: Arc::downgrade(&project_store),
            node_uuid,
        }
    }

    /// The value this node keeps under `key`, the latest written.
    pub fn read(&self, key: &str) -> Result<Option<JsonValue>> {
        self.the_project_store()?
            .read(NODE_STORE_TABLE, &self.the_key_in_the_table(key))
            .map_err(|refusal| Error::Runtime(refusal.to_string()))
    }

    /// Keep `value` under `key`: readable at once, on disk within the store's debounce window.
    pub fn write(&self, key: &str, value: &JsonValue) -> Result<()> {
        self.the_project_store()?
            .write(NODE_STORE_TABLE, &self.the_key_in_the_table(key), value)
            .map_err(|refusal| Error::Runtime(refusal.to_string()))
    }

    /// Stop keeping `key`.
    pub fn remove(&self, key: &str) -> Result<()> {
        self.the_project_store()?
            .remove(NODE_STORE_TABLE, &self.the_key_in_the_table(key));
        Ok(())
    }

    /// Commit every write the project's store holds waiting, without waiting for its window.
    pub(crate) fn commit_every_waiting_write_now(&self) -> Result<()> {
        self.the_project_store()?
            .commit_every_waiting_write_now()
            .map_err(|refusal| Error::Runtime(refusal.to_string()))
    }

    fn the_project_store(&self) -> Result<Arc<ProjectStore>> {
        self.project_store_while_its_project_is_open
            .upgrade()
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "the project of the node {} closed, and its store with it",
                    self.node_uuid
                ))
            })
    }

    fn the_key_in_the_table(&self, key: &str) -> String {
        format!("{}/{key}", self.node_uuid)
    }
}

/// Every project store open in this process, by resolved project directory: a store's file admits
/// one open database at a time, so runtimes in one process that hold the same project share it,
/// and a store closes under this lock so no runtime opens its file while it is still closing.
static PROJECT_STORE_OPEN_IN_THIS_PROCESS_BY_RESOLVED_DIRECTORY: LazyLock<
    Mutex<HashMap<PathBuf, Arc<ProjectStore>>>,
> = LazyLock::new(Default::default);

/// Every project store this runtime holds, one per resolved project directory, shared with any
/// other runtime in the process that holds the same project.
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

impl Drop for ProjectStoresOpenInThisRuntime {
    fn drop(&mut self) {
        let held_by_this_runtime: Vec<(PathBuf, Arc<ProjectStore>)> = self
            .project_store_by_resolved_directory
            .get_mut()
            .drain()
            .collect();
        for (resolved_project_directory, project_store) in held_by_this_runtime {
            let_go_of_a_project_store(&resolved_project_directory, project_store);
        }
    }
}

/// Remove every key of the `node_store` table whose node UUID `node_uuids_held` leaves out.
fn drop_the_keys_of_every_node_not_held(
    project_store: &ProjectStore,
    resolved_project_directory: &Path,
    node_uuids_held: &HashSet<Uuid>,
) {
    let keys = match project_store.keys_in(NODE_STORE_TABLE) {
        Ok(keys) => keys,
        Err(not_listed) => {
            tracing::warn!(
                project_directory = %resolved_project_directory.display(),
                "the project closed and its store's node keys did not list, so the keys of \
                 nodes it no longer holds stay: {not_listed}"
            );
            return;
        }
    };
    for key in keys {
        let node_uuid_of_the_key = key
            .split_once('/')
            .and_then(|(node_uuid, _)| node_uuid.parse::<Uuid>().ok());
        if node_uuid_of_the_key.is_some_and(|node_uuid| !node_uuids_held.contains(&node_uuid)) {
            project_store.remove(NODE_STORE_TABLE, &key);
        }
    }
}

/// Drop this runtime's hold on the store of the project at `resolved_project_directory`, closing
/// it under the process-wide lock when no other runtime in this process holds it.
fn let_go_of_a_project_store(
    resolved_project_directory: &Path,
    held_by_this_runtime: Arc<ProjectStore>,
) {
    let mut project_store_open_in_this_process_by_resolved_directory =
        PROJECT_STORE_OPEN_IN_THIS_PROCESS_BY_RESOLVED_DIRECTORY.lock();
    drop(held_by_this_runtime);
    let no_one_else_holds_it = project_store_open_in_this_process_by_resolved_directory
        .get(resolved_project_directory)
        .is_some_and(|open| Arc::strong_count(open) == 1);
    if no_one_else_holds_it {
        project_store_open_in_this_process_by_resolved_directory.remove(resolved_project_directory);
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
        let mut project_store_open_in_this_process_by_resolved_directory =
            PROJECT_STORE_OPEN_IN_THIS_PROCESS_BY_RESOLVED_DIRECTORY.lock();
        let open_in_this_process = match project_store_open_in_this_process_by_resolved_directory
            .get(&resolved_project_directory)
        {
            Some(open_in_another_runtime) => Arc::clone(open_in_another_runtime),
            None => {
                let opened = Arc::new(
                    ProjectStore::open_in_the_project_at(&resolved_project_directory)
                        .map_err(|refusal| Error::Runtime(refusal.to_string()))?,
                );
                project_store_open_in_this_process_by_resolved_directory
                    .insert(resolved_project_directory.clone(), Arc::clone(&opened));
                opened
            }
        };
        project_store_by_resolved_directory.insert(
            resolved_project_directory,
            Arc::clone(&open_in_this_process),
        );
        Ok(open_in_this_process)
    }

    /// Commit what the store of the project at `project_directory` holds waiting and let it go,
    /// as its project closes, closing it unless another runtime in this process still holds it;
    /// a runtime that holds it alone first drops the keys of every node
    /// `node_uuids_the_project_holds` leaves out, when it is known.
    pub(crate) fn close_the_store_of_the_project_at(
        &self,
        project_directory: &Path,
        node_uuids_the_project_holds: Option<&HashSet<Uuid>>,
    ) {
        let resolved_project_directory = std::fs::canonicalize(project_directory)
            .unwrap_or_else(|_| project_directory.to_path_buf());
        let Some(held_by_this_runtime) = self
            .project_store_by_resolved_directory
            .lock()
            .remove(&resolved_project_directory)
        else {
            return;
        };
        if let Some(node_uuids_the_project_holds) = node_uuids_the_project_holds {
            let project_store_open_in_this_process_by_resolved_directory =
                PROJECT_STORE_OPEN_IN_THIS_PROCESS_BY_RESOLVED_DIRECTORY.lock();
            let no_other_runtime_holds_it =
                project_store_open_in_this_process_by_resolved_directory
                    .get(&resolved_project_directory)
                    .is_some_and(|open| {
                        Arc::ptr_eq(open, &held_by_this_runtime) && Arc::strong_count(open) == 2
                    });
            if no_other_runtime_holds_it {
                drop_the_keys_of_every_node_not_held(
                    &held_by_this_runtime,
                    &resolved_project_directory,
                    node_uuids_the_project_holds,
                );
            }
        }
        if let Err(not_committed) = held_by_this_runtime.commit_every_waiting_write_now() {
            tracing::error!(
                project_directory = %resolved_project_directory.display(),
                "the project closed and its store did not commit what waited: {not_committed}"
            );
        }
        let_go_of_a_project_store(&resolved_project_directory, held_by_this_runtime);
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

#[cfg(test)]
#[path = "store_scoped_to_this_node_tests.rs"]
mod store_scoped_to_this_node_tests;
