// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Fixtures shared by the project venv's tests.

use super::project_venv_sync_stamp::{
    the_dist_info_prefix_of_the_module_wheel, the_distribution_name_of_the_module_wheel,
};
use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::core::test_support::{
    a_temporary_directory_at_owner_only_mode, write_an_executable_script_from_a_child_process,
};
use streamlib_consumer_rhi::runtime_unit_layout::carried_python_downloads_metadata_file_in;
use streamlib_project_repository::TatolabProjectFile;

mod carried_packages_and_lock_tests;
mod venv_made_and_synced_again_tests;

/// A project directory, and a stub uv beside it that writes each run's
/// arguments and working directory to `uv-runs` and then runs `body`.
struct ProjectAndAStubUv {
    project_directory: PathBuf,
    stub_uv: PathBuf,
    uv_runs: PathBuf,
    _directory_removed_on_drop: tempfile::TempDir,
}

/// A stub uv body that makes `.venv/bin/python` an interpreter that runs.
const MAKES_A_VENV_WHOSE_INTERPRETER_RUNS: &str = "mkdir -p .venv/bin && printf '#!/bin/sh\\nexit 0\\n' > .venv/bin/python && chmod 755 .venv/bin/python";

impl ProjectAndAStubUv {
    fn whose_uv_runs(body: &str) -> Self {
        let temporary_directory =
            a_temporary_directory_at_owner_only_mode().expect("a temporary directory");
        let project_directory = temporary_directory.path().join("project");
        std::fs::create_dir(&project_directory).expect("the project directory");
        std::fs::write(
            project_directory.join("pyproject.toml"),
            "[project]\nname = \"p\"\n",
        )
        .expect("the project's pyproject.toml");
        let uv_runs = temporary_directory.path().join("uv-runs");
        let stub_uv = temporary_directory.path().join("uv");
        write_an_executable_script_from_a_child_process(
            &stub_uv,
            &format!(
                "#!/bin/sh\necho \"$* in $(pwd)\" >> '{}'\n{body}\n",
                uv_runs.display()
            ),
        );
        Self {
            project_directory,
            stub_uv,
            uv_runs,
            _directory_removed_on_drop: temporary_directory,
        }
    }

    fn venv(&self) -> PathBuf {
        self.project_directory.join(".venv")
    }

    fn interpreter(&self) -> PathBuf {
        the_projects_venv_interpreter(&self.project_directory)
    }

    /// Give the project a `.venv/bin/python` that is the script `body`.
    fn given_a_venv_interpreter_running(&self, body: &str) {
        std::fs::create_dir_all(self.venv().join("bin")).expect("the venv's bin directory");
        write_an_executable_script_from_a_child_process(
            &self.interpreter(),
            &format!("#!/bin/sh\n{body}\n"),
        );
    }

    fn make_the_venv(&self) -> Result<(), ProjectVenvNotMade> {
        make_the_projects_venv_current_with_the_runtime_unit_uv(
            &self.project_directory,
            Some(&self.stub_uv),
            &PackagesTheRuntimeUnitCarries::default(),
            &|| false,
            None,
        )
    }

    fn make_the_venv_with_the_first_party_modules(
        &self,
        first_party_module_wheels: &[PathBuf],
    ) -> Result<(), ProjectVenvNotMade> {
        make_the_projects_venv_current_with_the_runtime_unit_uv(
            &self.project_directory,
            Some(&self.stub_uv),
            &PackagesTheRuntimeUnitCarries {
                first_party_module_wheels: first_party_module_wheels.to_vec(),
                ..PackagesTheRuntimeUnitCarries::default()
            },
            &|| false,
            None,
        )
    }

    /// Record the venv as synced with what the project holds now.
    fn given_the_venv_synced_with_what_the_project_holds_now(&self) {
        std::fs::write(
            self.venv().join(PROJECT_VENV_SYNC_STAMP_FILE_NAME),
            the_venv_sync_stamp_of(
                &self.project_directory,
                &PackagesTheRuntimeUnitCarries::default(),
            )
            .unwrap(),
        )
        .unwrap();
    }

    fn the_project_directory_resolved(&self) -> String {
        std::fs::canonicalize(&self.project_directory)
            .expect("the project directory resolves")
            .display()
            .to_string()
    }

    /// Each run of the stub uv, as `<arguments> in <working directory>`.
    fn uv_runs(&self) -> Vec<String> {
        std::fs::read_to_string(&self.uv_runs)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn one_sync_in_the_project(&self) -> Vec<String> {
        vec![format!(
            "sync in {}",
            std::fs::canonicalize(&self.project_directory)
                .expect("the project directory resolves")
                .display()
        )]
    }
}

fn the_reason_it_failed(venv_not_made: Result<(), ProjectVenvNotMade>) -> String {
    match venv_not_made {
        Err(ProjectVenvNotMade::Failed { failed_because }) => failed_because,
        other => panic!("the venv was to fail to be made, and it was {other:?}"),
    }
}

fn the_refusal(venv_not_made: Result<(), ProjectVenvNotMade>) -> String {
    match venv_not_made {
        Err(ProjectVenvNotMade::Refused(refusal)) => refusal.to_string(),
        other => panic!("the venv was to be refused, and it was {other:?}"),
    }
}
