// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::sync::Arc;

use uuid::Uuid;

use streamlib_runtime_client_contract::local_api_wire_contract::{
    DescribeRuntimeToolResult, ListSamplesToolResult,
};

use super::operations::RuntimeOperations;
use super::{
    CreateProjectRequest, MachineName, ProjectCreationQueued, ProjectOpenOrReloadRequest,
    ProjectPausedOrResumed, Runner, StreamAction, StreamActionQueued, StreamListing,
};
use crate::core::ProcessorDescriptor;
use crate::core::context::StreamClockReading;
use crate::core::error::Result;
use crate::core::processors::InstalledModuleOfANodeType;
use crate::core::pubsub::LoadedStreamIdentity;

/// What the local API reaches a runtime through: the runtime's description, the
/// stream a call names, a stream's node catalog, and the stream
/// actions.
///
/// `open_project`, `reload_project`, `close_project`, `remove_project` and the
/// `start_playback` and `stop_playback` calls answer at once, their actions running on the project's stream action queue; every
/// other call may block on the stream table, so the local API calls each from a
/// blocking task.
///
/// Implemented by [`Runner`], and by the control plane's test stubs.
pub trait OperationsOnTheStreamsLoadedInThisRuntime: Send + Sync {
    /// This machine's name, the first part of every address.
    fn this_machines_name(&self) -> &MachineName;

    /// [`Runner::describe_this_runtime`].
    fn describe_this_runtime(&self) -> DescribeRuntimeToolResult;

    /// The operations on the loaded stream `stream_id` names. Refused with
    /// [`Error::NotFound`](crate::core::error::Error::NotFound) naming the loaded streams when it is not loaded.
    fn runtime_operations_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Arc<dyn RuntimeOperations>>;

    /// The identity, and so the event topic, of the stream `stream_id` names as it is loaded now,
    /// with the same [`Error::NotFound`](crate::core::error::Error::NotFound) refusal; a stream
    /// loaded again answers a new one.
    fn loaded_stream_identity_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<LoadedStreamIdentity>;

    /// The node types the loaded stream `stream_id` names resolves — the
    /// native ones, then the ones described in its own interpreter — with the
    /// same [`Error::NotFound`](crate::core::error::Error::NotFound) refusal.
    fn node_catalog_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<ProcessorDescriptor>>;

    /// The node types described in the own interpreter of the loaded stream
    /// `stream_id` names that no installed module declares, with the same
    /// [`Error::NotFound`](crate::core::error::Error::NotFound) refusal.
    fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<ProcessorDescriptor>>;

    /// The node types installed modules declare in the `tatolab.nodes`
    /// entry points of the loaded stream `stream_id` names, each with the
    /// distribution declaring it, with the same
    /// [`Error::NotFound`](crate::core::error::Error::NotFound) refusal.
    fn node_types_installed_modules_declare_in_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<(ProcessorDescriptor, InstalledModuleOfANodeType)>>;

    /// Run the project's graph generator `generator` over `promoted_settings` as a fragment
    /// named `fragment_name`, or its own name, in the own interpreter of the loaded stream
    /// `stream_id` names, answering the fragment node it is stored as, its nodes and links by
    /// name; refused with the project's own traceback, and as
    /// [`Error::NotFound`](crate::core::error::Error::NotFound) for a stream not loaded.
    fn run_a_graph_generator_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
        generator: &str,
        fragment_name: Option<&str>,
        promoted_settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value>;

    /// The ids of the loaded streams, in order.
    fn ids_of_the_loaded_streams(&self) -> Vec<String>;

    /// [`Runner::open_project`].
    fn open_project(&self, request: ProjectOpenOrReloadRequest) -> Result<StreamActionQueued>;

    /// [`Runner::reload_project`].
    fn reload_project(&self, request: ProjectOpenOrReloadRequest) -> Result<StreamActionQueued>;

    /// [`Runner::close_project`].
    fn close_project(&self, project_directory: &Path) -> Result<StreamActionQueued>;

    /// [`Runner::start_playback`].
    fn start_playback(&self, project_directory: &Path) -> Result<StreamActionQueued>;

    /// [`Runner::make_the_projects_venv_current`].
    fn make_the_projects_venv_current(
        &self,
        project_directory: &Path,
    ) -> Result<StreamActionQueued>;

    /// [`Runner::create_project`].
    fn create_project(&self, request: CreateProjectRequest) -> Result<ProjectCreationQueued>;

    /// [`Runner::list_samples`].
    fn list_samples(&self) -> ListSamplesToolResult;

    /// [`Runner::describe_stream_action`].
    fn describe_stream_action(&self, action_id: &str) -> Result<StreamAction>;

    /// [`Runner::cancel_stream_action`].
    fn cancel_stream_action(&self, action_id: &str) -> Result<StreamAction>;

    /// [`Runner::stream_actions_under_way`].
    fn stream_actions_under_way(&self) -> Vec<StreamAction>;

    /// [`Runner::listen_for_each_stream_action_change`].
    fn listen_for_each_stream_action_change(&self) -> tokio::sync::broadcast::Receiver<String>;

    /// [`Runner::forget_the_stream_of_a_deleted_stream_asset`].
    fn forget_the_stream_of_a_deleted_stream_asset(
        &self,
        project: Uuid,
        stream_asset: Uuid,
    ) -> Result<Option<String>>;

    /// [`Runner::stop_playback`].
    fn stop_playback(&self, project_directory: &Path) -> Result<StreamActionQueued>;

    /// [`Runner::remove_project`].
    fn remove_project(&self, project_directory: &Path) -> Result<StreamActionQueued>;

    /// [`Runner::delete_project`].
    fn delete_project(&self, project_directory: &Path) -> Result<StreamActionQueued>;

    /// [`Runner::pause_playback`].
    fn pause_playback(&self, project_directory: &Path) -> Result<ProjectPausedOrResumed>;

    /// [`Runner::resume_playback`].
    fn resume_playback(&self, project_directory: &Path) -> Result<ProjectPausedOrResumed>;

    /// [`Runner::show_the_stream_of_a_stream_asset_under`].
    fn show_the_stream_of_a_stream_asset_under(
        &self,
        project: Uuid,
        stream_asset: Uuid,
        display_name: &str,
    ) -> Option<String>;

    /// [`Runner::load_the_stream_of_a_created_stream_asset`].
    fn load_the_stream_of_a_created_stream_asset(
        &self,
        project_directory: &Path,
        stream_asset: Uuid,
    ) -> Option<StreamActionQueued>;

    /// [`Runner::bring_the_streams_the_project_runs_as_its_own_in_line_with_what_it_places`].
    fn bring_the_streams_the_project_runs_as_its_own_in_line_with_what_it_places(
        &self,
        project_directory: &Path,
    ) -> Option<StreamActionQueued>;

    /// [`Runner::list_streams`].
    fn list_streams(&self) -> Vec<StreamListing>;

    /// [`Runner::set_stream_clock_scale`].
    fn set_stream_clock_scale(&self, stream_id: &str, scale: f64) -> Result<StreamClockReading>;

    /// [`Runner::lock_the_python_requirements_of_the_project_in`].
    fn lock_the_python_requirements_of_the_project_in(
        &self,
        project_directory: &Path,
    ) -> std::result::Result<(), String>;

    /// [`Runner::the_core_nodes_project_directory_this_runtime_carries`].
    fn the_core_nodes_project_directory_this_runtime_carries(&self) -> Option<PathBuf>;

    /// [`Runner::record_first_party_package_moves_in`].
    fn record_first_party_package_moves_in(
        &self,
        history: Arc<dyn super::ProjectHistoryRecordingFirstPartyPackageMoves>,
    );
}

impl OperationsOnTheStreamsLoadedInThisRuntime for Runner {
    fn this_machines_name(&self) -> &MachineName {
        self.machine_name()
    }

    fn describe_this_runtime(&self) -> DescribeRuntimeToolResult {
        Runner::describe_this_runtime(self)
    }

    fn runtime_operations_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Arc<dyn RuntimeOperations>> {
        self.loaded_stream_of_the_id(stream_id)
            .map(|stream| stream as Arc<dyn RuntimeOperations>)
    }

    fn loaded_stream_identity_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<LoadedStreamIdentity> {
        self.loaded_stream_of_the_id(stream_id)
            .map(|stream| stream.loaded_stream_identity().clone())
    }

    fn node_catalog_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.loaded_stream_of_the_id(stream_id)
            .map(|stream| stream.node_types_this_stream_resolves().node_catalog())
    }

    fn node_types_described_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<ProcessorDescriptor>> {
        self.loaded_stream_of_the_id(stream_id).map(|stream| {
            stream
                .node_types_this_stream_resolves()
                .descriptors_described_in_this_streams_processor_interpreter_no_installed_module_declares()
        })
    }

    fn node_types_installed_modules_declare_in_the_stream_a_call_names(
        &self,
        stream_id: &str,
    ) -> Result<Vec<(ProcessorDescriptor, InstalledModuleOfANodeType)>> {
        self.loaded_stream_of_the_id(stream_id).map(|stream| {
            stream
                .node_types_this_stream_resolves()
                .node_types_installed_modules_declare()
        })
    }

    fn run_a_graph_generator_in_the_interpreter_of_the_stream_a_call_names(
        &self,
        stream_id: &str,
        generator: &str,
        fragment_name: Option<&str>,
        promoted_settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let stream = self.loaded_stream_of_the_id(stream_id)?;
        let stream_environment = stream.stream_environment().ok_or_else(|| {
            crate::core::error::Error::Configuration(format!(
                "stream `{stream_id}` was loaded with no stream environment, so it has no \
                 interpreter to run graph generator `{generator}` in"
            ))
        })?;
        let _entered_this_streams_log_route = stream.log_route().enter_on_this_thread();
        crate::core::compiler::compiler_ops::graph_generator_run_in_the_projects_interpreter::run_a_graph_generator_in_the_projects_interpreter(
            stream_environment,
            generator,
            fragment_name,
            promoted_settings,
        )
    }

    fn ids_of_the_loaded_streams(&self) -> Vec<String> {
        Runner::ids_of_the_loaded_streams(self)
    }

    fn open_project(&self, request: ProjectOpenOrReloadRequest) -> Result<StreamActionQueued> {
        Runner::open_project(self, request)
    }

    fn reload_project(&self, request: ProjectOpenOrReloadRequest) -> Result<StreamActionQueued> {
        Runner::reload_project(self, request)
    }

    fn close_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        Runner::close_project(self, project_directory)
    }

    fn start_playback(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        Runner::start_playback(self, project_directory)
    }

    fn make_the_projects_venv_current(
        &self,
        project_directory: &Path,
    ) -> Result<StreamActionQueued> {
        Runner::make_the_projects_venv_current(self, project_directory)
    }

    fn create_project(&self, request: CreateProjectRequest) -> Result<ProjectCreationQueued> {
        Runner::create_project(self, request)
    }
    fn list_samples(&self) -> ListSamplesToolResult {
        Runner::list_samples(self)
    }

    fn describe_stream_action(&self, action_id: &str) -> Result<StreamAction> {
        Runner::describe_stream_action(self, action_id)
    }

    fn cancel_stream_action(&self, action_id: &str) -> Result<StreamAction> {
        Runner::cancel_stream_action(self, action_id)
    }

    fn stream_actions_under_way(&self) -> Vec<StreamAction> {
        Runner::stream_actions_under_way(self)
    }

    fn listen_for_each_stream_action_change(&self) -> tokio::sync::broadcast::Receiver<String> {
        Runner::listen_for_each_stream_action_change(self)
    }

    fn forget_the_stream_of_a_deleted_stream_asset(
        &self,
        project: Uuid,
        stream_asset: Uuid,
    ) -> Result<Option<String>> {
        Runner::forget_the_stream_of_a_deleted_stream_asset(self, project, stream_asset)
    }

    fn stop_playback(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        Runner::stop_playback(self, project_directory)
    }

    fn pause_playback(&self, project_directory: &Path) -> Result<ProjectPausedOrResumed> {
        Runner::pause_playback(self, project_directory)
    }

    fn resume_playback(&self, project_directory: &Path) -> Result<ProjectPausedOrResumed> {
        Runner::resume_playback(self, project_directory)
    }

    fn remove_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        Runner::remove_project(self, project_directory)
    }

    fn show_the_stream_of_a_stream_asset_under(
        &self,
        project: Uuid,
        stream_asset: Uuid,
        display_name: &str,
    ) -> Option<String> {
        Runner::show_the_stream_of_a_stream_asset_under(self, project, stream_asset, display_name)
    }

    fn load_the_stream_of_a_created_stream_asset(
        &self,
        project_directory: &Path,
        stream_asset: Uuid,
    ) -> Option<StreamActionQueued> {
        Runner::load_the_stream_of_a_created_stream_asset(self, project_directory, stream_asset)
    }

    fn bring_the_streams_the_project_runs_as_its_own_in_line_with_what_it_places(
        &self,
        project_directory: &Path,
    ) -> Option<StreamActionQueued> {
        Runner::bring_the_streams_the_project_runs_as_its_own_in_line_with_what_it_places(
            self,
            project_directory,
        )
    }

    fn delete_project(&self, project_directory: &Path) -> Result<StreamActionQueued> {
        Runner::delete_project(self, project_directory)
    }

    fn list_streams(&self) -> Vec<StreamListing> {
        Runner::list_streams(self)
    }

    fn set_stream_clock_scale(&self, stream_id: &str, scale: f64) -> Result<StreamClockReading> {
        Runner::set_stream_clock_scale(self, stream_id, scale)
    }

    fn lock_the_python_requirements_of_the_project_in(
        &self,
        project_directory: &Path,
    ) -> std::result::Result<(), String> {
        Runner::lock_the_python_requirements_of_the_project_in(self, project_directory)
    }

    fn the_core_nodes_project_directory_this_runtime_carries(&self) -> Option<PathBuf> {
        Runner::the_core_nodes_project_directory_this_runtime_carries(self)
    }

    fn record_first_party_package_moves_in(
        &self,
        history: Arc<dyn super::ProjectHistoryRecordingFirstPartyPackageMoves>,
    ) {
        Runner::record_first_party_package_moves_in(self, history)
    }
}
