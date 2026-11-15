// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Keeping a project's `.venv` current with the uv the runtime unit carries.
//! A venv whose interpreter does not run is removed and made again by
//! `uv sync`, which takes the Python the project's `.python-version` pins from
//! the machine or fetches it; one whose interpreter runs is synced again only
//! when what it was synced from changed. Each first-party module the runtime
//! unit carries is linked into the project's `.tatolab/first-party-imports/`
//! and declared in its `pyproject.toml` as a uv path source through that link,
//! so any `uv sync`, the runtime's or a person's, keeps it and its requirements.
//! When the unit carries third-party wheels and a Python, the project links the
//! wheels into `.tatolab/carried-wheels` and names that link as a uv find-links
//! source, and each sync installs the carried Python unless the machine has one
//! and syncs offline first, reaching an index only when the offline sync fails.

mod packages_declared_in_the_projects_pyproject;
mod project_venv_sync_stamp;
mod project_venv_sync_with_the_runtime_unit_uv;

pub(crate) use project_venv_sync_stamp::every_first_party_module_wheel_in;
#[cfg(test)]
pub(crate) use project_venv_sync_stamp::record_the_venv_synced_with_what_the_project_holds_now;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::core::child_process_lifetime::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::compiler::compiler_ops::processor_interpreter_spawn_host::{
    HowABoundedChildProcessRunEnded, remove_every_python_variable_named_in,
    run_a_child_process_to_its_exit_within,
};
use crate::core::error::Error;
use crate::core::runtime::project_stream_action_queue::StreamActionVenvSyncReport;
use crate::core::runtime::what_interrupted_a_load;
use packages_declared_in_the_projects_pyproject::{
    declare_each_first_party_module_in_the_project, declare_the_carried_wheels_in_the_project,
    declare_the_core_nodes_project_in_the_project,
};
use project_venv_sync_stamp::{
    PROJECT_VENV_SYNC_STAMP_FILE_NAME, every_first_party_module_is_installed_in,
    the_venv_sync_stamp_of,
};
use project_venv_sync_with_the_runtime_unit_uv::{
    WhatAFailedSyncLeaves, sync_the_venv_with_the_runtime_unit_uv, the_uv_runs_tried_in_turn,
};
use streamlib_consumer_rhi::runtime_unit_layout::{
    carried_python_downloads_directory_beside_the_lend, carried_wheels_directory_beside_the_lend,
    core_nodes_project_directory_beside_the_lend, first_party_modules_directory_beside_the_lend,
};

/// What the runtime unit carries for every project's `.venv`, found beside its lend.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PackagesTheRuntimeUnitCarries {
    /// Each first-party module wheel, in name order.
    pub(crate) first_party_module_wheels: Vec<PathBuf>,
    /// The folder of third-party wheels the core modules require, when the unit carries one.
    pub(crate) carried_wheels_directory: Option<PathBuf>,
    /// The uv Python mirror holding the CPython build the unit carries, when it carries one.
    pub(crate) carried_python_downloads_directory: Option<PathBuf>,
    /// The Core Nodes project every project resolves as an import, when the unit carries it.
    pub(crate) core_nodes_project_directory: Option<PathBuf>,
}

impl PackagesTheRuntimeUnitCarries {
    /// What the unit whose lend is at `lend_directory` carries beside it.
    pub(crate) fn beside_the_lend(lend_directory: &Path) -> Self {
        let a_directory_that_is_there =
            |directory: Option<PathBuf>| directory.filter(|directory| directory.is_dir());
        Self {
            first_party_module_wheels: first_party_modules_directory_beside_the_lend(
                lend_directory,
            )
            .map(|directory| every_first_party_module_wheel_in(&directory))
            .unwrap_or_default(),
            carried_wheels_directory: a_directory_that_is_there(
                carried_wheels_directory_beside_the_lend(lend_directory),
            ),
            carried_python_downloads_directory: a_directory_that_is_there(
                carried_python_downloads_directory_beside_the_lend(lend_directory),
            ),
            core_nodes_project_directory: a_directory_that_is_there(
                core_nodes_project_directory_beside_the_lend(lend_directory),
            ),
        }
    }
}

/// The interpreter a project's streams run in: `<project>/.venv/bin/python`,
/// taken as it is spelled, never resolved through its symlink.
pub(crate) fn the_projects_venv_interpreter(project_directory: &Path) -> PathBuf {
    project_directory.join(".venv").join("bin").join("python")
}

/// Refuse `project_directory` unless it is an absolute path: a project is
/// named by its absolute path.
fn refuse_a_project_directory_not_named_by_its_absolute_path(
    project_directory: &Path,
) -> Result<(), Error> {
    if !project_directory.is_absolute() {
        return Err(Error::Configuration(format!(
            "the project directory `{}` is not an absolute path; a project is named by its \
             absolute path",
            project_directory.display()
        )));
    }
    Ok(())
}

/// How long `.venv/bin/python -I -c ""` may take to show that the venv's
/// interpreter runs.
pub(crate) const PROJECT_VENV_INTERPRETER_PROBE_BOUND: Duration = Duration::from_secs(10);

/// Why a venv is not made by a runtime given no uv.
pub(crate) const THE_RUNTIME_UNIT_CARRIES_NO_UV: &str =
    "the runtime unit carries no uv; rebuild it with `cargo xtask build-runtime`";

/// The variables besides `PYTHON*` a sync is started without, so uv makes the
/// project's own `.venv` from its `.python-version`.
const VARIABLES_A_PROJECT_VENV_SYNC_IS_STARTED_WITHOUT: [&str; 2] =
    ["VIRTUAL_ENV", "UV_PROJECT_ENVIRONMENT"];

/// What a sync's standard-error lines are logged under.
const PROJECT_VENV_SYNC_STANDARD_ERROR_LOG_LABEL: &str = "project-venv-uv-sync";

/// What the probe of a venv's interpreter is named in the runtime's log.
const PROJECT_VENV_INTERPRETER_PROBE_LOG_LABEL: &str = "project-venv-interpreter-probe";

/// Why a project's `.venv` was not made.
#[derive(Debug)]
pub(crate) enum ProjectVenvNotMade {
    /// Refused for the directory the caller gave, or cut short by the
    /// runtime's shutdown: nothing is recorded for it.
    Refused(Error),
    /// The venv could not be made; the stream is recorded failed for this.
    Failed {
        /// uv's own words, or why uv never ran.
        failed_because: String,
    },
}

impl From<ProjectVenvNotMade> for Error {
    fn from(venv_not_made: ProjectVenvNotMade) -> Self {
        match venv_not_made {
            ProjectVenvNotMade::Refused(refusal) => refusal,
            ProjectVenvNotMade::Failed { failed_because } => Error::Runtime(failed_because),
        }
    }
}

/// Make the `.venv` of the project in `project_directory` current with
/// `runtime_unit_uv`: one whose interpreter does not run is removed and made
/// again by `uv sync`, one whose interpreter runs is synced again when the
/// project's `pyproject.toml` or `uv.lock`, the runtime's version or the
/// first-party modules it carries changed since its last sync. Each wheel of
/// `first_party_module_wheels` is declared in the project first, so the sync
/// installs it.
/// A sync is unbounded — uv's own network timeouts end a stalled download —
/// cut short whenever `is_interrupted_by_the_host` reports true, and reported
/// line by line to `report_of_the_venv_sync` when one is given.
pub(crate) fn make_the_projects_venv_current_with_the_runtime_unit_uv(
    project_directory: &Path,
    runtime_unit_uv: Option<&Path>,
    packages_the_runtime_unit_carries: &PackagesTheRuntimeUnitCarries,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
    report_of_the_venv_sync: Option<StreamActionVenvSyncReport>,
) -> Result<(), ProjectVenvNotMade> {
    let first_party_module_wheels = &packages_the_runtime_unit_carries.first_party_module_wheels;
    let project = project_directory.display();
    refuse_a_project_directory_not_named_by_its_absolute_path(project_directory)
        .map_err(ProjectVenvNotMade::Refused)?;
    if !project_directory.is_dir() {
        return Err(ProjectVenvNotMade::Refused(Error::Configuration(format!(
            "the project directory {project} does not exist"
        ))));
    }
    let an_interrupt_refusal = || {
        ProjectVenvNotMade::Refused(Error::Runtime(format!(
            "the .venv of {project} was not made: {}",
            what_interrupted_a_load()
        )))
    };
    declare_each_first_party_module_in_the_project(project_directory, first_party_module_wheels)?;
    declare_the_core_nodes_project_in_the_project(
        project_directory,
        packages_the_runtime_unit_carries
            .core_nodes_project_directory
            .as_deref(),
    )?;
    declare_the_carried_wheels_in_the_project(
        project_directory,
        packages_the_runtime_unit_carries
            .carried_wheels_directory
            .as_deref(),
    )?;
    let interpreter = the_projects_venv_interpreter(project_directory);
    let venv = project_directory.join(".venv");
    let why_the_interpreter_does_not_run = match probe_the_venv_interpreter(
        &interpreter,
        project_directory,
        is_interrupted_by_the_host,
    ) {
        VenvInterpreterProbe::Runs => None,
        VenvInterpreterProbe::CutShortByTheHost => return Err(an_interrupt_refusal()),
        VenvInterpreterProbe::DoesNotRun { why } => Some(why),
    };
    let sync_stamp_now =
        the_venv_sync_stamp_of(project_directory, packages_the_runtime_unit_carries);
    let Some(why_the_interpreter_does_not_run) = why_the_interpreter_does_not_run else {
        let synced_with_what_it_holds_now = sync_stamp_now.as_deref()
            == std::fs::read_to_string(venv.join(PROJECT_VENV_SYNC_STAMP_FILE_NAME))
                .ok()
                .as_deref();
        let Some(runtime_unit_uv) = runtime_unit_uv.filter(|uv| uv.is_file()) else {
            return Ok(());
        };
        let current = synced_with_what_it_holds_now
            && every_first_party_module_is_installed_in(&venv, first_party_module_wheels);
        if current || sync_stamp_now.is_none() {
            return Ok(());
        }
        tracing::info!(
            "{} was synced with another pyproject.toml, uv.lock, runtime or first-party \
             module, or a sync outside the runtime took a first-party module out, so it is \
             synced again with {}",
            venv.display(),
            runtime_unit_uv.display()
        );
        return sync_the_venv_with_the_runtime_unit_uv(
            project_directory,
            runtime_unit_uv,
            packages_the_runtime_unit_carries,
            is_interrupted_by_the_host,
            WhatAFailedSyncLeaves::TheVenvAsItWas,
            report_of_the_venv_sync,
        );
    };
    let Some(runtime_unit_uv) = runtime_unit_uv.filter(|uv| uv.is_file()) else {
        return Err(ProjectVenvNotMade::Failed {
            failed_because: format!(
                "{why_the_interpreter_does_not_run}, and {THE_RUNTIME_UNIT_CARRIES_NO_UV}"
            ),
        });
    };
    if sync_stamp_now.is_none() {
        return Err(ProjectVenvNotMade::Failed {
            failed_because: format!(
                "{why_the_interpreter_does_not_run}, and {project} holds no pyproject.toml to \
                 make its .venv from: uv makes a project's .venv from the project's own \
                 pyproject.toml, and in a folder without one it would sync the project above it"
            ),
        });
    }
    if is_interrupted_by_the_host() {
        return Err(an_interrupt_refusal());
    }
    tracing::info!(
        "{why_the_interpreter_does_not_run}, so {} is made again with {}",
        venv.display(),
        runtime_unit_uv.display()
    );
    remove_what_stands_at(&venv)?;
    sync_the_venv_with_the_runtime_unit_uv(
        project_directory,
        runtime_unit_uv,
        packages_the_runtime_unit_carries,
        is_interrupted_by_the_host,
        WhatAFailedSyncLeaves::NoVenv,
        report_of_the_venv_sync,
    )
}

/// `<runtime_unit_uv> lock` in `project_directory`, each package the unit carries declared in
/// the project first, offline first when the unit carries wheels; refused in uv's own words
/// when the requirements its `pyproject.toml` declares cannot be resolved.
pub(crate) fn lock_the_projects_python_requirements_with_the_runtime_unit_uv(
    project_directory: &Path,
    runtime_unit_uv: &Path,
    packages_the_runtime_unit_carries: &PackagesTheRuntimeUnitCarries,
) -> Result<(), String> {
    declare_each_first_party_module_in_the_project(
        project_directory,
        &packages_the_runtime_unit_carries.first_party_module_wheels,
    )
    .map_err(|not_declared| Error::from(not_declared).to_string())?;
    declare_the_core_nodes_project_in_the_project(
        project_directory,
        packages_the_runtime_unit_carries
            .core_nodes_project_directory
            .as_deref(),
    )
    .map_err(|not_declared| Error::from(not_declared).to_string())?;
    declare_the_carried_wheels_in_the_project(
        project_directory,
        packages_the_runtime_unit_carries
            .carried_wheels_directory
            .as_deref(),
    )
    .map_err(|not_declared| Error::from(not_declared).to_string())?;
    let runs = the_uv_runs_tried_in_turn(
        runtime_unit_uv,
        project_directory,
        "lock",
        packages_the_runtime_unit_carries,
    );
    let last_run = runs.len() - 1;
    for (run_index, (step, mut command)) in runs.into_iter().enumerate() {
        let run = run_a_child_process_to_its_exit_within(
            &mut command,
            PROJECT_VENV_SYNC_STANDARD_ERROR_LOG_LABEL,
            Duration::MAX,
            &|| false,
        )
        .map_err(|spawn_failure| {
            format!(
                "`{} {}` could not be started in {}: {spawn_failure}",
                runtime_unit_uv.display(),
                step.trim_start_matches("uv "),
                project_directory.display()
            )
        })?;
        if run.exited_successfully() {
            return Ok(());
        }
        if run_index < last_run {
            tracing::info!(
                "`{step}` could not lock {} from what the runtime carries and uv's cache, so it \
                 locks from the project's indexes: {}",
                project_directory.display(),
                run.standard_error_as_a_refusal_quotes_it()
            );
            continue;
        }
        return Err(format!(
            "`{step}` failed in {}: it exited with {}. {}",
            project_directory.display(),
            run.exit_status_rendered(),
            run.standard_error_as_a_refusal_quotes_it()
        ));
    }
    Ok(())
}

/// What a probe of a venv's interpreter found.
enum VenvInterpreterProbe {
    /// It started and exited successfully within the probe's bound.
    Runs,
    /// It did not, for `why`.
    DoesNotRun { why: String },
    /// The runtime's host interrupted the probe, which found nothing.
    CutShortByTheHost,
}

/// Whether `interpreter` starts and exits successfully within
/// [`PROJECT_VENV_INTERPRETER_PROBE_BOUND`].
fn probe_the_venv_interpreter(
    interpreter: &Path,
    project_directory: &Path,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> VenvInterpreterProbe {
    let mut command = a_command_detached_from_the_terminal_in(interpreter, project_directory);
    command.args(["-I", "-c", ""]).stderr(Stdio::null());
    let interpreter = interpreter.display();
    let probe = match run_a_child_process_to_its_exit_within(
        &mut command,
        PROJECT_VENV_INTERPRETER_PROBE_LOG_LABEL,
        PROJECT_VENV_INTERPRETER_PROBE_BOUND,
        is_interrupted_by_the_host,
    ) {
        Ok(probe) => probe,
        Err(not_started) => {
            return VenvInterpreterProbe::DoesNotRun {
                why: format!("{interpreter} does not start: {not_started}"),
            };
        }
    };
    if probe.exited_successfully() {
        return VenvInterpreterProbe::Runs;
    }
    let why = match probe.ended {
        HowABoundedChildProcessRunEnded::Exited => {
            format!("{interpreter} exits with {}", probe.exit_status_rendered())
        }
        HowABoundedChildProcessRunEnded::BoundElapsed => format!(
            "{interpreter} did not exit within {}s",
            PROJECT_VENV_INTERPRETER_PROBE_BOUND.as_secs_f64()
        ),
        HowABoundedChildProcessRunEnded::InterruptedByTheHost => {
            return VenvInterpreterProbe::CutShortByTheHost;
        }
    };
    VenvInterpreterProbe::DoesNotRun { why }
}

/// `program` started in `project_directory` with nothing on its standard
/// input or output, in a process group of its own and given no descriptor
/// beyond its standard streams.
fn a_command_detached_from_the_terminal_in(program: &Path, project_directory: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(project_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    command
}

/// `runtime_unit_uv` started in `project_directory` without the variables
/// that would point it at another Python or another venv.
pub(crate) fn project_venv_uv_command(runtime_unit_uv: &Path, project_directory: &Path) -> Command {
    let mut command = a_command_detached_from_the_terminal_in(runtime_unit_uv, project_directory);
    command.stderr(Stdio::piped());
    remove_every_python_variable_named_in(
        &mut command,
        std::env::vars_os().map(|(variable_name, _)| variable_name),
    );
    for variable_name in VARIABLES_A_PROJECT_VENV_SYNC_IS_STARTED_WITHOUT {
        command.env_remove(variable_name);
    }
    command
}

/// Remove what stands at `path` — a directory, a file or a link, never what a
/// link points at — when it is there.
fn remove_what_stands_at(path: &Path) -> Result<(), ProjectVenvNotMade> {
    let removed = match path.symlink_metadata() {
        Err(not_there) if not_there.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(unreadable) => Err(unreadable),
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    };
    removed.map_err(|not_removed| ProjectVenvNotMade::Failed {
        failed_because: format!("{} could not be removed: {not_removed}", path.display()),
    })
}

#[cfg(test)]
mod tests;
