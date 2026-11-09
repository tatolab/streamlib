// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Keeping a project's `.venv` current with the uv the runtime unit carries.
//! A venv whose interpreter does not run is removed and made again by
//! `uv sync`, which takes the Python the project's `.python-version` pins from
//! the machine or fetches it; one whose interpreter runs is synced again only
//! when what it was synced from changed. Each sync installs the first-party
//! modules the runtime unit carries into the venv, never into the project's
//! `pyproject.toml` or `uv.lock`.

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
use crate::core::runtime::what_interrupted_a_load;

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
/// first-party modules it carries changed since its last sync, and after each
/// sync every module of `first_party_module_folders` is installed into it.
/// A sync is unbounded — uv's own network timeouts end a stalled download —
/// and cut short whenever `is_interrupted_by_the_host` reports true.
pub(crate) fn make_the_projects_venv_current_with_the_runtime_unit_uv(
    project_directory: &Path,
    runtime_unit_uv: Option<&Path>,
    first_party_module_folders: &[PathBuf],
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<(), ProjectVenvNotMade> {
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
    let sync_stamp_now = the_venv_sync_stamp_of(project_directory, first_party_module_folders);
    let Some(why_the_interpreter_does_not_run) = why_the_interpreter_does_not_run else {
        let synced_with_what_it_holds_now = sync_stamp_now.as_deref()
            == std::fs::read_to_string(venv.join(PROJECT_VENV_SYNC_STAMP_FILE_NAME))
                .ok()
                .as_deref();
        let Some(runtime_unit_uv) = runtime_unit_uv.filter(|uv| uv.is_file()) else {
            return Ok(());
        };
        let current = synced_with_what_it_holds_now
            && every_first_party_module_is_installed_in(&venv, first_party_module_folders);
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
        return sync_the_venv_and_install_the_first_party_modules(
            project_directory,
            runtime_unit_uv,
            first_party_module_folders,
            is_interrupted_by_the_host,
            WhatAFailedSyncLeaves::TheVenvAsItWas,
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
    remove_the_venv(&venv)?;
    sync_the_venv_and_install_the_first_party_modules(
        project_directory,
        runtime_unit_uv,
        first_party_module_folders,
        is_interrupted_by_the_host,
        WhatAFailedSyncLeaves::NoVenv,
    )
}

/// The file in a project's `.venv` naming what its last sync was made from.
const PROJECT_VENV_SYNC_STAMP_FILE_NAME: &str = "tatolab-runtime-sync";

/// The files of a project whose change makes its `.venv` sync again.
const PROJECT_FILES_A_VENV_IS_SYNCED_FROM: [&str; 3] =
    ["pyproject.toml", "uv.lock", ".python-version"];

/// What a project's `.venv` would be synced from now: the SHA-256 over its
/// `pyproject.toml`, `uv.lock` and `.python-version`, each import's
/// `pyproject.toml`, this runtime's version and the folder of each first-party
/// module with its `pyproject.toml`. `None` when the project
/// holds no `pyproject.toml`, which uv syncs nothing from.
fn the_venv_sync_stamp_of(
    project_directory: &Path,
    first_party_module_folders: &[PathBuf],
) -> Option<String> {
    use sha2::{Digest, Sha256};
    if !project_directory.join("pyproject.toml").is_file() {
        return None;
    }
    let mut stamp = Sha256::new();
    stamp.update(env!("CARGO_PKG_VERSION").as_bytes());
    for file_name in PROJECT_FILES_A_VENV_IS_SYNCED_FROM {
        stamp.update(file_name.as_bytes());
        stamp.update(std::fs::read(project_directory.join(file_name)).unwrap_or_default());
    }
    for import_pyproject in each_import_pyproject_of_the_project_in(project_directory) {
        stamp.update(import_pyproject.as_os_str().as_encoded_bytes());
        stamp.update(std::fs::read(&import_pyproject).unwrap_or_default());
    }
    for module_folder in first_party_module_folders {
        stamp.update(module_folder.as_os_str().as_encoded_bytes());
        stamp.update(std::fs::read(module_folder.join("pyproject.toml")).unwrap_or_default());
    }
    Some(
        stamp
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

/// Each folder in `first_party_modules_directory` holding a `pyproject.toml`,
/// in name order; none when the directory is not there.
pub(crate) fn every_first_party_module_folder_in(
    first_party_modules_directory: &Path,
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(first_party_modules_directory) else {
        return Vec::new();
    };
    let mut module_folders: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|folder| folder.join("pyproject.toml").is_file())
        .collect();
    module_folders.sort();
    module_folders
}

/// Whether `venv` holds a `.dist-info` of every module of
/// `first_party_module_folders`; a `uv sync` or `uv run` outside the runtime
/// takes out each package the project's lock does not name.
fn every_first_party_module_is_installed_in(
    venv: &Path,
    first_party_module_folders: &[PathBuf],
) -> bool {
    let Ok(library_entries) = std::fs::read_dir(venv.join("lib")) else {
        return first_party_module_folders.is_empty();
    };
    let installed_dist_info_names: Vec<String> = library_entries
        .filter_map(|entry| entry.ok())
        .filter_map(|python_directory| {
            std::fs::read_dir(python_directory.path().join("site-packages")).ok()
        })
        .flatten()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".dist-info"))
        .map(|name| name.to_ascii_lowercase())
        .collect();
    first_party_module_folders.iter().all(|module_folder| {
        the_dist_info_prefix_of_the_module_in(module_folder).is_some_and(|prefix| {
            installed_dist_info_names
                .iter()
                .any(|installed| installed.starts_with(&prefix))
        })
    })
}

/// `<name>-` as a `.dist-info` directory spells the distribution that
/// `module_folder`'s `pyproject.toml` names: lowercase, each `-` and `.` an `_`.
fn the_dist_info_prefix_of_the_module_in(module_folder: &Path) -> Option<String> {
    let pyproject: toml::Table = std::fs::read_to_string(module_folder.join("pyproject.toml"))
        .ok()?
        .parse()
        .ok()?;
    let name = pyproject.get("project")?.get("name")?.as_str()?;
    Some(format!(
        "{}-",
        name.to_ascii_lowercase().replace(['-', '.'], "_")
    ))
}

/// Write into the project's `.venv` what it is synced from now, so a load
/// finding the same runs no uv.
pub(crate) fn record_the_venv_synced_with_what_the_project_holds_now(
    project_directory: &Path,
    first_party_module_folders: &[PathBuf],
) -> std::io::Result<()> {
    match the_venv_sync_stamp_of(project_directory, first_party_module_folders) {
        Some(sync_stamp) => std::fs::write(
            project_directory
                .join(".venv")
                .join(PROJECT_VENV_SYNC_STAMP_FILE_NAME),
            sync_stamp,
        ),
        None => Ok(()),
    }
}

/// The `pyproject.toml` of each folder in the project's `imports/`, in name
/// order: uv reads each one as a path source of the project.
fn each_import_pyproject_of_the_project_in(project_directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(
        project_directory.join(streamlib_project_repository::PROJECT_IMPORTS_DIRECTORY),
    ) else {
        return Vec::new();
    };
    let mut import_pyprojects: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path().join("pyproject.toml")))
        .filter(|import_pyproject| import_pyproject.is_file())
        .collect();
    import_pyprojects.sort();
    import_pyprojects
}

/// What a sync that fails leaves of the project's `.venv`.
#[derive(Clone, Copy)]
enum WhatAFailedSyncLeaves {
    /// The venv was made from nothing, so its half-made remains are removed.
    NoVenv,
    /// The venv's interpreter ran before the sync, so it is kept for the next one.
    TheVenvAsItWas,
}

/// `uv sync` in `project_directory`, then `uv pip install` of every
/// first-party module into the venv, then the stamp naming what it was made
/// from, taken after the sync since it may write `uv.lock`; each refused in
/// uv's own words.
fn sync_the_venv_and_install_the_first_party_modules(
    project_directory: &Path,
    runtime_unit_uv: &Path,
    first_party_module_folders: &[PathBuf],
    is_interrupted_by_the_host: &dyn Fn() -> bool,
    what_a_failed_sync_leaves: WhatAFailedSyncLeaves,
) -> Result<(), ProjectVenvNotMade> {
    let project = project_directory.display();
    let venv = project_directory.join(".venv");
    let leave_what_a_failed_sync_leaves = || match what_a_failed_sync_leaves {
        WhatAFailedSyncLeaves::NoVenv => remove_the_venv(&venv),
        WhatAFailedSyncLeaves::TheVenvAsItWas => Ok(()),
    };
    let an_interrupt_refusal = || {
        ProjectVenvNotMade::Refused(Error::Runtime(format!(
            "the .venv of {project} was not made: {}",
            what_interrupted_a_load()
        )))
    };
    let mut steps = vec![(
        "uv sync",
        project_venv_sync_command(runtime_unit_uv, project_directory),
    )];
    if !first_party_module_folders.is_empty() {
        steps.push((
            "uv export",
            project_lock_constraints_export_command(runtime_unit_uv, project_directory),
        ));
        steps.push((
            "uv pip install",
            first_party_module_install_command(
                runtime_unit_uv,
                project_directory,
                first_party_module_folders,
            ),
        ));
    }
    for (step, mut command) in steps {
        let run = run_a_child_process_to_its_exit_within(
            &mut command,
            PROJECT_VENV_SYNC_STANDARD_ERROR_LOG_LABEL,
            Duration::MAX,
            is_interrupted_by_the_host,
        )
        .map_err(|spawn_failure| {
            let failed = ProjectVenvNotMade::Failed {
                failed_because: format!(
                    "`{} {}` could not be started in {project}: {spawn_failure}",
                    runtime_unit_uv.display(),
                    step.trim_start_matches("uv ")
                ),
            };
            leave_what_a_failed_sync_leaves().err().unwrap_or(failed)
        })?;
        if run.exited_successfully() {
            continue;
        }
        let quoted_standard_error = run.standard_error_as_a_refusal_quotes_it();
        let failed_because = match run.ended {
            HowABoundedChildProcessRunEnded::Exited
            | HowABoundedChildProcessRunEnded::BoundElapsed => format!(
                "`{step}` failed for {}: it exited with {}. {quoted_standard_error}",
                venv.display(),
                run.exit_status_rendered()
            ),
            HowABoundedChildProcessRunEnded::InterruptedByTheHost => {
                leave_what_a_failed_sync_leaves()?;
                return Err(an_interrupt_refusal());
            }
        };
        leave_what_a_failed_sync_leaves()?;
        return Err(ProjectVenvNotMade::Failed { failed_because });
    }
    tracing::info!("`uv sync` made {}", venv.display());
    if let Err(not_written) = record_the_venv_synced_with_what_the_project_holds_now(
        project_directory,
        first_party_module_folders,
    ) {
        tracing::warn!(
            venv = %venv.display(),
            %not_written,
            "the venv's sync stamp was not written, so its next load syncs it again"
        );
    }
    Ok(())
}

/// `<runtime_unit_uv> lock` in `project_directory`, refused in uv's own words when the
/// requirements its `pyproject.toml` declares cannot be resolved.
pub(crate) fn lock_the_projects_python_requirements_with_the_runtime_unit_uv(
    project_directory: &Path,
    runtime_unit_uv: &Path,
) -> Result<(), String> {
    let mut command = project_venv_uv_command(runtime_unit_uv, project_directory);
    command.arg("lock");
    let run = run_a_child_process_to_its_exit_within(
        &mut command,
        PROJECT_VENV_SYNC_STANDARD_ERROR_LOG_LABEL,
        Duration::MAX,
        &|| false,
    )
    .map_err(|spawn_failure| {
        format!(
            "`{} lock` could not be started in {}: {spawn_failure}",
            runtime_unit_uv.display(),
            project_directory.display()
        )
    })?;
    if run.exited_successfully() {
        return Ok(());
    }
    Err(format!(
        "`uv lock` failed in {}: it exited with {}. {}",
        project_directory.display(),
        run.exit_status_rendered(),
        run.standard_error_as_a_refusal_quotes_it()
    ))
}

/// `<runtime_unit_uv> pip install --python <the project's venv interpreter>
/// <each first-party module folder>` in `project_directory`, started as a
/// sync is.
fn first_party_module_install_command(
    runtime_unit_uv: &Path,
    project_directory: &Path,
    first_party_module_folders: &[PathBuf],
) -> Command {
    let mut command = project_venv_uv_command(runtime_unit_uv, project_directory);
    command
        .args(["pip", "install", "--python"])
        .arg(the_projects_venv_interpreter(project_directory))
        .arg("--constraint")
        .arg(the_project_lock_constraints_file(project_directory))
        .args(first_party_module_folders);
    command
}

/// The file in a project's `.venv` pinning every package its `uv.lock` holds,
/// which a first-party module's install is constrained by so it never changes
/// a version the project locked.
const PROJECT_LOCK_CONSTRAINTS_FILE_NAME: &str = "tatolab-project-lock-constraints.txt";

fn the_project_lock_constraints_file(project_directory: &Path) -> PathBuf {
    project_directory
        .join(".venv")
        .join(PROJECT_LOCK_CONSTRAINTS_FILE_NAME)
}

/// `<runtime_unit_uv> export` of the project's `uv.lock`, every package but the
/// project and its path sources, into its constraints file.
fn project_lock_constraints_export_command(
    runtime_unit_uv: &Path,
    project_directory: &Path,
) -> Command {
    let mut command = project_venv_uv_command(runtime_unit_uv, project_directory);
    command
        .args([
            "export",
            "--frozen",
            "--no-hashes",
            "--no-emit-project",
            "--no-emit-local",
            "--quiet",
            "--output-file",
        ])
        .arg(the_project_lock_constraints_file(project_directory));
    command
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

/// `<runtime_unit_uv> sync` in `project_directory`, started without the
/// variables that would point uv at another environment or Python.
fn project_venv_sync_command(runtime_unit_uv: &Path, project_directory: &Path) -> Command {
    let mut command = project_venv_uv_command(runtime_unit_uv, project_directory);
    command.arg("sync");
    command
}

/// `runtime_unit_uv` started in `project_directory` without the variables
/// that would point it at another Python or another venv.
fn project_venv_uv_command(runtime_unit_uv: &Path, project_directory: &Path) -> Command {
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

/// Remove `venv` whatever it is — a directory, a file or a link, never what a
/// link points at — when it is there.
fn remove_the_venv(venv: &Path) -> Result<(), ProjectVenvNotMade> {
    let removed = match venv.symlink_metadata() {
        Err(not_there) if not_there.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(unreadable) => Err(unreadable),
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(venv),
        Ok(_) => std::fs::remove_file(venv),
    };
    removed.map_err(|not_removed| ProjectVenvNotMade::Failed {
        failed_because: format!("{} could not be removed: {not_removed}", venv.display()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::{
        a_temporary_directory_at_owner_only_mode, write_an_executable_script_from_a_child_process,
    };
    use std::path::PathBuf;

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
                &[],
                &|| false,
            )
        }

        fn make_the_venv_with_the_first_party_modules(
            &self,
            first_party_module_folders: &[PathBuf],
        ) -> Result<(), ProjectVenvNotMade> {
            make_the_projects_venv_current_with_the_runtime_unit_uv(
                &self.project_directory,
                Some(&self.stub_uv),
                first_party_module_folders,
                &|| false,
            )
        }

        /// Record the venv as synced with what the project holds now.
        fn given_the_venv_synced_with_what_the_project_holds_now(&self) {
            std::fs::write(
                self.venv().join(PROJECT_VENV_SYNC_STAMP_FILE_NAME),
                the_venv_sync_stamp_of(&self.project_directory, &[]).unwrap(),
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

    #[test]
    fn a_venv_whose_interpreter_runs_and_was_synced_with_what_the_project_holds_is_untouched() {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 1");
        project.given_a_venv_interpreter_running("exit 0");
        project.given_the_venv_synced_with_what_the_project_holds_now();
        std::fs::write(project.venv().join("kept"), "kept").unwrap();

        project
            .make_the_venv()
            .expect("a venv whose interpreter runs");

        assert!(project.uv_runs().is_empty(), "{:?}", project.uv_runs());
        assert_eq!(
            std::fs::read_to_string(project.venv().join("kept")).unwrap(),
            "kept"
        );
    }

    #[test]
    fn a_missing_venv_is_made_by_one_uv_sync_in_the_project() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);

        project.make_the_venv().expect("the venv is made");

        assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
        assert!(project.interpreter().is_file());
    }

    #[test]
    fn a_venv_whose_interpreter_is_a_dangling_link_is_removed_and_made_again() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        std::fs::create_dir_all(project.venv().join("bin")).unwrap();
        std::os::unix::fs::symlink("/nonexistent/python3.12", project.interpreter()).unwrap();
        std::fs::write(project.venv().join("stale"), "stale").unwrap();

        project.make_the_venv().expect("the venv is made again");

        assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
        assert!(
            !project.venv().join("stale").exists(),
            "the old venv was not removed"
        );
        assert!(project.interpreter().is_file());
    }

    #[test]
    fn a_venv_that_links_elsewhere_has_its_link_removed_and_never_what_it_links_to() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        let linked_venv = project
            .project_directory
            .parent()
            .unwrap()
            .join("linked-venv");
        std::fs::create_dir_all(linked_venv.join("bin")).unwrap();
        std::fs::write(linked_venv.join("kept"), "kept").unwrap();
        write_an_executable_script_from_a_child_process(
            &linked_venv.join("bin").join("python"),
            "#!/bin/sh\nexit 3\n",
        );
        std::os::unix::fs::symlink(&linked_venv, project.venv()).unwrap();

        project.make_the_venv().expect("the venv is made again");

        assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
        assert!(
            !project
                .venv()
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(linked_venv.join("kept")).unwrap(),
            "kept"
        );
        assert!(linked_venv.join("bin").join("python").is_file());
    }

    #[test]
    fn a_folder_with_no_pyproject_fails_and_uv_never_runs_where_it_would_sync_the_project_above() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        std::fs::remove_file(project.project_directory.join("pyproject.toml")).unwrap();
        project.given_a_venv_interpreter_running("exit 3");

        let failed_because = the_reason_it_failed(project.make_the_venv());

        assert!(
            failed_because.contains("holds no pyproject.toml"),
            "{failed_because}"
        );
        assert!(project.uv_runs().is_empty(), "{:?}", project.uv_runs());
        assert!(
            project.interpreter().is_file(),
            "the venv was removed with nothing to make it"
        );
    }

    #[test]
    fn a_venv_whose_interpreter_exits_non_zero_is_made_again() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        project.given_a_venv_interpreter_running("exit 3");

        project.make_the_venv().expect("the venv is made again");

        assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
    }

    #[test]
    fn a_failed_sync_fails_in_uvs_own_words_and_removes_the_half_made_venv() {
        let project = ProjectAndAStubUv::whose_uv_runs(
            "mkdir -p .venv/bin\necho 'error: No solution found when resolving dependencies:' >&2\nexit 1",
        );

        let failed_because = the_reason_it_failed(project.make_the_venv());

        assert!(
            failed_because.contains("error: No solution found when resolving dependencies:"),
            "{failed_because}"
        );
        assert!(
            failed_because.contains("`uv sync` failed for"),
            "{failed_because}"
        );
        assert!(!project.venv().exists(), "the half-made venv was left");
    }

    #[test]
    fn a_runtime_given_no_uv_fails_saying_the_unit_carries_none_and_leaves_the_venv() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        project.given_a_venv_interpreter_running("exit 3");

        let failed_because =
            the_reason_it_failed(make_the_projects_venv_current_with_the_runtime_unit_uv(
                &project.project_directory,
                None,
                &[],
                &|| false,
            ));

        assert!(
            failed_because.contains(THE_RUNTIME_UNIT_CARRIES_NO_UV),
            "{failed_because}"
        );
        assert!(
            project.interpreter().is_file(),
            "a venv uv could not replace was removed"
        );
    }

    #[test]
    fn a_relative_or_missing_project_directory_is_refused_and_uv_never_runs() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        for project_directory in [
            PathBuf::from("relative/project"),
            project.project_directory.join("missing"),
        ] {
            the_refusal(make_the_projects_venv_current_with_the_runtime_unit_uv(
                &project_directory,
                Some(&project.stub_uv),
                &[],
                &|| false,
            ));
        }
        assert!(project.uv_runs().is_empty(), "{:?}", project.uv_runs());
    }

    #[test]
    fn a_sync_the_host_interrupts_is_refused_and_removes_the_half_made_venv() {
        let project = ProjectAndAStubUv::whose_uv_runs("mkdir -p .venv/bin\nsleep 30");
        let interrupted_after = std::time::Instant::now() + Duration::from_millis(300);

        let refusal = the_refusal(make_the_projects_venv_current_with_the_runtime_unit_uv(
            &project.project_directory,
            Some(&project.stub_uv),
            &[],
            &|| std::time::Instant::now() >= interrupted_after,
        ));

        assert!(refusal.contains("was not made: "), "{refusal}");
        assert!(!project.venv().exists(), "the half-made venv was left");
    }

    #[test]
    fn a_probe_the_host_interrupts_is_refused_and_never_fails_the_project() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        project.given_a_venv_interpreter_running("sleep 30");
        let interrupted_after = std::time::Instant::now() + Duration::from_millis(300);

        let refusal = the_refusal(make_the_projects_venv_current_with_the_runtime_unit_uv(
            &project.project_directory,
            None,
            &[],
            &|| std::time::Instant::now() >= interrupted_after,
        ));

        assert!(refusal.contains("was not made: "), "{refusal}");
        assert!(
            project.interpreter().is_file(),
            "an interrupted probe removed the venv"
        );
    }

    #[test]
    fn the_sync_is_started_without_the_variables_that_point_uv_elsewhere() {
        let command = project_venv_sync_command(
            Path::new("/opt/tatolab/bin/uv"),
            Path::new("/home/someone/app"),
        );
        let removed: Vec<&std::ffi::OsStr> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name)
            .collect();

        assert_eq!(command.get_program(), "/opt/tatolab/bin/uv");
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["sync"]);
        assert_eq!(
            command.get_current_dir(),
            Some(Path::new("/home/someone/app"))
        );
        for variable_name in ["VIRTUAL_ENV", "UV_PROJECT_ENVIRONMENT"] {
            assert!(
                removed.contains(&std::ffi::OsStr::new(variable_name)),
                "{removed:?}"
            );
        }
        assert!(
            command
                .get_envs()
                .all(|(name, value)| value.is_none()
                    || !name.to_string_lossy().starts_with("PYTHON")),
            "the sync is handed a PYTHON* variable"
        );
    }

    #[test]
    fn a_made_venv_records_what_it_was_synced_from_so_the_next_load_runs_no_uv() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);

        project.make_the_venv().expect("the venv is made");
        project.make_the_venv().expect("the venv is current");

        assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
    }

    #[test]
    fn a_running_venv_is_synced_again_once_the_projects_pyproject_changes() {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
        project.given_a_venv_interpreter_running("exit 0");
        project.given_the_venv_synced_with_what_the_project_holds_now();
        std::fs::write(
            project.project_directory.join("pyproject.toml"),
            "[project]\nname = \"p\"\ndependencies = [\"numpy\"]\n",
        )
        .unwrap();

        project.make_the_venv().expect("the venv is synced again");

        assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
        assert!(project.interpreter().is_file());
    }

    #[test]
    fn a_running_venv_is_synced_again_once_an_imports_pyproject_or_the_python_pin_changes() {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
        project.given_a_venv_interpreter_running("exit 0");
        let import_folder = project.project_directory.join("imports/robot-arm");
        std::fs::create_dir_all(&import_folder).unwrap();
        std::fs::write(
            import_folder.join("pyproject.toml"),
            "[project]\nname = \"robot-arm\"\n",
        )
        .unwrap();
        project.given_the_venv_synced_with_what_the_project_holds_now();

        std::fs::write(
            import_folder.join("pyproject.toml"),
            "[project]\nname = \"robot-arm\"\ndependencies = [\"pyserial\"]\n",
        )
        .unwrap();
        project.make_the_venv().expect("the venv is synced again");
        std::fs::write(project.project_directory.join(".python-version"), "3.13\n").unwrap();
        project.make_the_venv().expect("the venv is synced again");

        assert_eq!(project.uv_runs().len(), 2, "{:?}", project.uv_runs());
    }

    #[test]
    fn a_failed_sync_of_a_running_venv_fails_in_uvs_words_and_keeps_the_venv() {
        let project = ProjectAndAStubUv::whose_uv_runs(
            "echo 'error: Failed to fetch: `https://get.tatolab.com/simple/`' >&2\nexit 2",
        );
        project.given_a_venv_interpreter_running("exit 0");
        std::fs::write(
            project.venv().join(PROJECT_VENV_SYNC_STAMP_FILE_NAME),
            "stale",
        )
        .unwrap();

        let failed_because = the_reason_it_failed(project.make_the_venv());

        assert!(
            failed_because.contains("error: Failed to fetch"),
            "{failed_because}"
        );
        assert!(
            project.interpreter().is_file(),
            "a venv whose interpreter ran was removed"
        );
        assert_eq!(
            std::fs::read_to_string(project.venv().join(PROJECT_VENV_SYNC_STAMP_FILE_NAME))
                .unwrap(),
            "stale"
        );
    }

    #[test]
    fn each_first_party_module_is_installed_into_the_venv_after_its_sync() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        let modules = project.project_directory.parent().unwrap().join("imports");
        let ops = modules.join("tatolab-ops");
        std::fs::create_dir_all(&ops).unwrap();
        std::fs::write(
            ops.join("pyproject.toml"),
            "[project]\nname = \"tatolab-ops\"\n",
        )
        .unwrap();

        project
            .make_the_venv_with_the_first_party_modules(&[ops.clone()])
            .expect("the venv is made");

        let project_directory = project.the_project_directory_resolved();
        assert_eq!(
            project.uv_runs(),
            [
                format!("sync in {project_directory}"),
                format!(
                    "export --frozen --no-hashes --no-emit-project --no-emit-local --quiet \
                     --output-file {} in {project_directory}",
                    project
                        .venv()
                        .join(PROJECT_LOCK_CONSTRAINTS_FILE_NAME)
                        .display()
                ),
                format!(
                    "pip install --python {} --constraint {} {} in {project_directory}",
                    project.interpreter().display(),
                    project
                        .venv()
                        .join(PROJECT_LOCK_CONSTRAINTS_FILE_NAME)
                        .display(),
                    ops.display()
                ),
            ]
        );
    }

    #[test]
    fn a_running_venv_is_synced_again_once_a_first_party_module_changes() {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
        project.given_a_venv_interpreter_running("exit 0");
        project.given_the_venv_synced_with_what_the_project_holds_now();
        let ops = project
            .project_directory
            .parent()
            .unwrap()
            .join("tatolab-ops");
        std::fs::create_dir_all(&ops).unwrap();
        std::fs::write(
            ops.join("pyproject.toml"),
            "[project]\nname = \"tatolab-ops\"\n",
        )
        .unwrap();

        project
            .make_the_venv_with_the_first_party_modules(&[ops])
            .expect("the venv is synced again");

        assert_eq!(project.uv_runs().len(), 3, "{:?}", project.uv_runs());
    }

    #[test]
    fn a_running_venv_a_sync_outside_the_runtime_took_a_first_party_module_out_of_is_synced_again()
    {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
        project.given_a_venv_interpreter_running("exit 0");
        let ops = project
            .project_directory
            .parent()
            .unwrap()
            .join("tatolab-ops");
        std::fs::create_dir_all(&ops).unwrap();
        std::fs::write(
            ops.join("pyproject.toml"),
            "[project]\nname = \"tatolab-ops\"\n",
        )
        .unwrap();
        let site_packages = project.venv().join("lib/python3.12/site-packages");
        std::fs::create_dir_all(site_packages.join("tatolab_ops-0.68.41.dist-info")).unwrap();
        std::fs::write(
            project.venv().join(PROJECT_VENV_SYNC_STAMP_FILE_NAME),
            the_venv_sync_stamp_of(&project.project_directory, std::slice::from_ref(&ops)).unwrap(),
        )
        .unwrap();

        project
            .make_the_venv_with_the_first_party_modules(std::slice::from_ref(&ops))
            .expect("the venv is current");
        assert!(project.uv_runs().is_empty(), "{:?}", project.uv_runs());

        std::fs::remove_dir(site_packages.join("tatolab_ops-0.68.41.dist-info")).unwrap();
        project
            .make_the_venv_with_the_first_party_modules(&[ops])
            .expect("the venv is synced again");
        assert_eq!(project.uv_runs().len(), 3, "{:?}", project.uv_runs());
    }

    #[test]
    fn a_failed_first_party_install_fails_in_uvs_words() {
        let project = ProjectAndAStubUv::whose_uv_runs(&format!(
            "case \"$1\" in pip) echo 'error: tatolab-ops has no build backend' >&2; exit 1;; esac\n{MAKES_A_VENV_WHOSE_INTERPRETER_RUNS}"
        ));
        let ops = project
            .project_directory
            .parent()
            .unwrap()
            .join("tatolab-ops");
        std::fs::create_dir_all(&ops).unwrap();

        let failed_because =
            the_reason_it_failed(project.make_the_venv_with_the_first_party_modules(&[ops]));

        assert!(
            failed_because.contains("`uv pip install` failed for")
                && failed_because.contains("error: tatolab-ops has no build backend"),
            "{failed_because}"
        );
    }

    #[test]
    fn the_first_party_modules_are_each_folder_holding_a_pyproject_in_name_order() {
        let modules = tempfile::tempdir().unwrap();
        for folder in ["tatolab-ops", "a-module", "no-pyproject"] {
            std::fs::create_dir(modules.path().join(folder)).unwrap();
        }
        for folder in ["tatolab-ops", "a-module"] {
            std::fs::write(modules.path().join(folder).join("pyproject.toml"), "").unwrap();
        }
        std::fs::write(modules.path().join("pyproject.toml"), "").unwrap();

        assert_eq!(
            every_first_party_module_folder_in(modules.path()),
            [
                modules.path().join("a-module"),
                modules.path().join("tatolab-ops")
            ]
        );
        assert!(every_first_party_module_folder_in(&modules.path().join("missing")).is_empty());
    }

    #[test]
    fn a_lock_runs_uv_lock_in_the_project() {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
        lock_the_projects_python_requirements_with_the_runtime_unit_uv(
            &project.project_directory,
            &project.stub_uv,
        )
        .unwrap();
        assert_eq!(
            project.uv_runs(),
            vec![format!(
                "lock in {}",
                project.the_project_directory_resolved()
            )]
        );
    }

    #[test]
    fn a_lock_uv_cannot_resolve_is_refused_in_uvs_own_words() {
        let project = ProjectAndAStubUv::whose_uv_runs(
            "echo 'No solution found when resolving dependencies' >&2\nexit 1",
        );
        let refusal = lock_the_projects_python_requirements_with_the_runtime_unit_uv(
            &project.project_directory,
            &project.stub_uv,
        )
        .unwrap_err();
        assert!(refusal.contains("`uv lock` failed"), "{refusal}");
        assert!(
            refusal.contains("No solution found when resolving dependencies"),
            "{refusal}"
        );
    }
}
