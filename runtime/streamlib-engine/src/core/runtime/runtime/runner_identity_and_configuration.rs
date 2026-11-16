// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the engine reports about itself, and what its host hands it after construction.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use streamlib_runtime_client_contract::local_api_wire_contract::DescribeRuntimeToolResult;
use streamlib_runtime_client_contract::streamlib_runtime_directory::StreamlibRuntimeDirectory;

use crate::core::runtime::MachineName;
use crate::core::runtime::RuntimeUniqueId;
use crate::core::{Error, Result};
use crate::iceoryx2::Iceoryx2Node;

use super::{CreatedProjectFirstCommit, Runner};

impl Runner {
    /// Unique identifier for this runtime instance.
    pub fn runtime_id(&self) -> &RuntimeUniqueId {
        &self.engine_resources_shared_by_every_stream.runtime_id
    }

    /// The name this machine is addressed by, the first part of
    /// `<machine>/<stream id>/<node>/<port>`.
    pub fn machine_name(&self) -> &MachineName {
        &self.engine_resources_shared_by_every_stream.machine_name
    }

    /// The active segment of the runtime's own log, `None` when it keeps none.
    pub fn runtime_own_log_path(&self) -> Option<&std::path::Path> {
        self.engine_resources_shared_by_every_stream
            .runtime_own_log
            .as_ref()
            .and_then(|runtime_own_log| runtime_own_log.jsonl_log_path())
    }

    /// This runtime's version, build id, monotonic uptime and the folder its own log is written
    /// to.
    pub fn describe_this_runtime(&self) -> DescribeRuntimeToolResult {
        let engine_resources = &self.engine_resources_shared_by_every_stream;
        DescribeRuntimeToolResult {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            build_id: crate::core::helper_process_transport::ENGINE_BUILD_ID.to_owned(),
            up_for_ms: u64::try_from(
                engine_resources
                    .instant_this_engine_started
                    .elapsed()
                    .as_millis(),
            )
            .unwrap_or(u64::MAX),
            runtime_log_directory: self
                .runtime_own_log_path()
                .and_then(std::path::Path::parent)
                .map(std::path::Path::to_path_buf),
        }
    }

    /// The runtime directory this runtime resolved as it was built.
    pub fn runtime_directory(&self) -> &StreamlibRuntimeDirectory {
        &self
            .engine_resources_shared_by_every_stream
            .runtime_directory
    }

    /// This runtime's iceoryx2 node.
    pub fn iceoryx2_node(&self) -> &Iceoryx2Node {
        &self.engine_resources_shared_by_every_stream.iceoryx2_node
    }

    /// The engine's tokio runtime handle, which a host serving this runtime's
    /// local API runs it on.
    pub fn tokio_handle(&self) -> tokio::runtime::Handle {
        self.engine_resources_shared_by_every_stream
            .tokio_runtime_variant
            .handle()
    }

    /// Hand fds 1 and 2 back to the process now, for a caller about to leave
    /// this engine alive rather than drop it.
    pub fn stop_intercepting_the_standard_streams(&self) {
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "linux"))]
        self.engine_resources_shared_by_every_stream
            .process_logging_pathway_hold
            .stop_intercepting_the_standard_streams();
    }

    pub fn set_node_interpreter_lend_directory(&self, lend_directory: PathBuf) -> Result<()> {
        self.engine_resources_shared_by_every_stream
            .node_interpreter_lend_directory
            .set(lend_directory)
    }

    /// Hand this runtime the uv it makes every project's `.venv` with — the
    /// runtime unit's own. The host calls it once, before any load; a second
    /// call is refused. A runtime never handed one records failed every stream
    /// whose venv needs making.
    pub fn set_runtime_unit_uv_executable(&self, runtime_unit_uv: PathBuf) -> Result<()> {
        self.engine_resources_shared_by_every_stream
            .runtime_unit_uv_executable
            .set(runtime_unit_uv)
    }

    /// Hand this runtime what commits a project [`Runner::create_project`] wrote as the first
    /// commit of its history. The host calls it once, before any load; a second call is
    /// refused. A runtime never handed one creates projects with no history, which the
    /// project's first transaction starts.
    pub fn set_created_project_first_commit(
        &self,
        created_project_first_commit: CreatedProjectFirstCommit,
    ) -> Result<()> {
        self.engine_resources_shared_by_every_stream
            .created_project_first_commit
            .set(created_project_first_commit)
            .map_err(|_| {
                Error::Configuration(
                    "this runtime was already handed what commits a created project; it is \
                     handed once"
                        .to_string(),
                )
            })
    }

    /// Record each move of a project's first-party packages to this runtime's version in
    /// `history`, in place of the one handed before.
    pub fn record_first_party_package_moves_in(
        &self,
        history: Arc<dyn crate::core::runtime::ProjectHistoryRecordingFirstPartyPackageMoves>,
    ) {
        *self
            .engine_resources_shared_by_every_stream
            .project_history_recording_first_party_package_moves
            .lock() = Some(history);
    }

    /// Commit the project the scaffold wrote into `project_directory` as its history's first
    /// commit, with what the host handed; nothing without it.
    pub(crate) fn commit_the_first_revision_of_a_created_project(
        &self,
        project_directory: &Path,
    ) -> Result<()> {
        match self
            .engine_resources_shared_by_every_stream
            .created_project_first_commit
            .get()
        {
            Some(created_project_first_commit) => created_project_first_commit(project_directory)
                .map_err(|not_committed| {
                    Error::Runtime(format!(
                        "the project in {} was written, and its first commit was not: \
                         {not_committed}",
                        project_directory.display()
                    ))
                }),
            None => Ok(()),
        }
    }

    /// Offer every node the native service `offer` names, started at its first
    /// lease. The host offers each service once; a second offer of one name is
    /// refused.
    pub fn offer_shared_native_service(
        &self,
        offer: crate::core::shared_native_services::SharedNativeServiceOffer,
    ) -> Result<()> {
        self.engine_resources_shared_by_every_stream
            .shared_native_services
            .offer(offer)
    }
}
