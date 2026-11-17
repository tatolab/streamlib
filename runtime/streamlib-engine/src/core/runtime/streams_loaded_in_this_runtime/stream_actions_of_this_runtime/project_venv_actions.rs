// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Runner {
    /// Lock the Python requirements the `pyproject.toml` in `project_directory` declares with
    /// the uv this runtime was handed, refused in uv's own words when they cannot be resolved.
    /// A runtime never handed a uv locks nothing, and the project's next load resolves them.
    pub fn lock_the_python_requirements_of_the_project_in(
        &self,
        project_directory: &Path,
    ) -> std::result::Result<(), String> {
        match self
            .engine_resources_shared_by_every_stream
            .runtime_unit_uv_executable
            .get()
        {
            Some(runtime_unit_uv) => {
                lock_the_projects_python_requirements_with_the_runtime_unit_uv(
                    project_directory,
                    runtime_unit_uv,
                    &self.packages_this_runtime_unit_carries(),
                )
            }
            None => Ok(()),
        }
    }

    /// Where the Core Nodes project the runtime unit carries is, when this runtime was handed a
    /// lend and its unit carries one.
    pub fn the_core_nodes_project_directory_this_runtime_carries(&self) -> Option<PathBuf> {
        self.packages_this_runtime_unit_carries()
            .core_nodes_project_directory
    }

    /// What the runtime unit carries beside this runtime's lend, nothing when it was handed no
    /// lend.
    pub(super) fn packages_this_runtime_unit_carries(&self) -> PackagesTheRuntimeUnitCarries {
        self.engine_resources_shared_by_every_stream
            .node_interpreter_lend_directory
            .get()
            .map(PackagesTheRuntimeUnitCarries::beside_the_lend)
            .unwrap_or_default()
    }

    /// Make the `.venv` of the project in `project_directory` current with the
    /// uv this runtime was handed and the first-party modules beside its lend,
    /// its sync reported to `report_of_the_venv_sync` when one is given.
    pub(super) fn make_the_projects_venv(
        &self,
        project_directory: &Path,
        is_interrupted: &dyn Fn() -> bool,
        report_of_the_venv_sync: Option<StreamActionVenvSyncReport>,
    ) -> std::result::Result<(), ProjectVenvNotMade> {
        let runtime_unit_uv = self
            .engine_resources_shared_by_every_stream
            .runtime_unit_uv_executable
            .get();
        if let Some(runtime_unit_uv) = runtime_unit_uv.filter(|uv| uv.is_file()) {
            let history = self
                .engine_resources_shared_by_every_stream
                .project_history_recording_first_party_package_moves
                .lock()
                .clone();
            move_the_projects_first_party_packages_to_the_runtime_version(
                project_directory,
                runtime_unit_uv,
                env!("CARGO_PKG_VERSION"),
                history.as_deref(),
                is_interrupted,
            )?;
        }
        make_the_projects_venv_current_with_the_runtime_unit_uv(
            project_directory,
            runtime_unit_uv,
            &self.packages_this_runtime_unit_carries(),
            is_interrupted,
            report_of_the_venv_sync,
        )
    }

    /// Queue the making current of the `.venv` of the project in `project_directory` on its
    /// stream action queue, answering the action's id at once: the venv is made, or synced
    /// again when what it was synced with changed, as a load makes it, and no stream is read,
    /// loaded, started or stopped. Refused, with nothing queued, for a directory not named by
    /// its absolute path.
    pub fn make_the_projects_venv_current(
        &self,
        project_directory: &Path,
    ) -> Result<StreamActionQueued> {
        let project_directory = the_absolute_project_directory_resolved(project_directory)?;
        let queue_of_each_project = &self.stream_actions.stream_action_queue_of_each_project;
        let action_id = queue_of_each_project.next_action_id(
            self.engine_resources_shared_by_every_stream
                .runtime_id
                .as_str(),
        );
        let action = StreamAction {
            action_id: action_id.clone(),
            project_directory: project_directory.clone(),
            kind: StreamActionKind::MakeTheVenvCurrent,
            state: StreamActionState::Queued,
            step: None,
            venv_sync: None,
            streams: Vec::new(),
            failed_because: None,
        };
        Ok(queue_of_each_project.queue_the_stream_action(
            action,
            Box::new(move |runner: &Runner| {
                runner.make_the_projects_venv_current_for_its_stream_action(
                    &action_id,
                    &project_directory,
                );
            }),
            &self.tokio_handle(),
        ))
    }

    /// Run the making current of the venv the stream action `action_id` queued of the project
    /// in `project_directory`, and finish the action, failed for why the venv was not made.
    pub(super) fn make_the_projects_venv_current_for_its_stream_action(
        &self,
        action_id: &str,
        project_directory: &Path,
    ) {
        let progress = self.progress_of_the_stream_action(action_id);
        progress.is_on(StreamActionStep::MakingTheVenv);
        let made = self.make_the_projects_venv(
            project_directory,
            &|| progress.is_interrupted(),
            Some(progress.report_of_the_next_venv_sync()),
        );
        progress.finish(match made {
            Ok(()) => Ok(Vec::new()),
            Err(ProjectVenvNotMade::Refused(refusal)) => Err(refusal.to_string()),
            Err(ProjectVenvNotMade::Failed { failed_because }) => Err(failed_because),
        });
    }
}
