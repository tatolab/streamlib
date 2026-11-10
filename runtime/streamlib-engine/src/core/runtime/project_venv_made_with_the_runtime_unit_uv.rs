// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Keeping a project's `.venv` current with the uv the runtime unit carries.
//! A venv whose interpreter does not run is removed and made again by
//! `uv sync`, which takes the Python the project's `.python-version` pins from
//! the machine or fetches it; one whose interpreter runs is synced again only
//! when what it was synced from changed. Each first-party module the runtime
//! unit carries is linked into the project's `.tatolab/first-party-imports/`
//! and declared in its `pyproject.toml` as a uv path source through that link,
//! so any `uv sync`, the runtime's or a person's, keeps it and its requirements.

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
use streamlib_project_repository::{
    FirstPartyModuleDeclaredByAProject, PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY,
    pyproject_with_each_first_party_module_as_a_uv_path_source,
};

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
/// and cut short whenever `is_interrupted_by_the_host` reports true.
pub(crate) fn make_the_projects_venv_current_with_the_runtime_unit_uv(
    project_directory: &Path,
    runtime_unit_uv: Option<&Path>,
    first_party_module_wheels: &[PathBuf],
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
    declare_each_first_party_module_in_the_project(project_directory, first_party_module_wheels)?;
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
    let sync_stamp_now = the_venv_sync_stamp_of(project_directory, first_party_module_wheels);
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
            first_party_module_wheels,
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
    remove_what_stands_at(&venv)?;
    sync_the_venv_with_the_runtime_unit_uv(
        project_directory,
        runtime_unit_uv,
        first_party_module_wheels,
        is_interrupted_by_the_host,
        WhatAFailedSyncLeaves::NoVenv,
    )
}

/// Link each wheel of `first_party_module_wheels` into the project's
/// [`PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY`], take out each link to a module
/// the runtime no longer carries, and declare each module in the project's
/// `pyproject.toml` as a uv path source through its link. Nothing is written
/// when the runtime carries no first-party module or the project holds no
/// `pyproject.toml`.
fn declare_each_first_party_module_in_the_project(
    project_directory: &Path,
    first_party_module_wheels: &[PathBuf],
) -> Result<(), ProjectVenvNotMade> {
    let pyproject_path = project_directory.join("pyproject.toml");
    if first_party_module_wheels.is_empty() || !pyproject_path.is_file() {
        return Ok(());
    }
    let not_declared_because = |why: String| ProjectVenvNotMade::Failed {
        failed_because: format!(
            "the first-party modules the runtime carries could not be declared in {}: {why}",
            pyproject_path.display()
        ),
    };
    let links_directory = project_directory.join(PROJECT_FIRST_PARTY_MODULE_LINKS_DIRECTORY);
    std::fs::create_dir_all(&links_directory).map_err(|not_made| {
        not_declared_because(format!(
            "{} was not made: {not_made}",
            links_directory.display()
        ))
    })?;
    let declared_modules: Vec<FirstPartyModuleDeclaredByAProject> = first_party_module_wheels
        .iter()
        .filter_map(|module_wheel| {
            Some(FirstPartyModuleDeclaredByAProject {
                distribution_name: the_distribution_name_of_the_module_wheel(module_wheel)?,
                link_file_name: module_wheel.file_name()?.to_str()?.to_owned(),
            })
        })
        .collect();
    for module_wheel in first_party_module_wheels {
        let Some(link_file_name) = module_wheel.file_name() else {
            continue;
        };
        let link = links_directory.join(link_file_name);
        if std::fs::read_link(&link).is_ok_and(|target| target == *module_wheel) {
            continue;
        }
        remove_what_stands_at(&link)?;
        std::os::unix::fs::symlink(module_wheel, &link).map_err(|not_linked| {
            not_declared_because(format!(
                "{} was not linked to {}: {not_linked}",
                link.display(),
                module_wheel.display()
            ))
        })?;
    }
    let links_the_runtime_no_longer_carries: Vec<PathBuf> = std::fs::read_dir(&links_directory)
        .map_err(|not_read| {
            not_declared_because(format!(
                "{} was not read: {not_read}",
                links_directory.display()
            ))
        })?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            !declared_modules
                .iter()
                .any(|module| entry.file_name() == module.link_file_name.as_str())
        })
        .map(|entry| entry.path())
        .collect();
    for link in links_the_runtime_no_longer_carries {
        remove_what_stands_at(&link)?;
    }
    let pyproject_as_it_was = std::fs::read_to_string(&pyproject_path)
        .map_err(|not_read| not_declared_because(format!("it was not read: {not_read}")))?;
    let pyproject = pyproject_with_each_first_party_module_as_a_uv_path_source(
        &pyproject_as_it_was,
        project_directory,
        &declared_modules,
    )
    .map_err(|refusal| not_declared_because(refusal.to_string()))?;
    if pyproject != pyproject_as_it_was {
        std::fs::write(&pyproject_path, pyproject).map_err(|not_written| {
            not_declared_because(format!("it was not written: {not_written}"))
        })?;
    }
    Ok(())
}

/// The file in a project's `.venv` naming what its last sync was made from.
const PROJECT_VENV_SYNC_STAMP_FILE_NAME: &str = "tatolab-runtime-sync";

/// The files of a project whose change makes its `.venv` sync again.
const PROJECT_FILES_A_VENV_IS_SYNCED_FROM: [&str; 3] =
    ["pyproject.toml", "uv.lock", ".python-version"];

/// What a project's `.venv` would be synced from now: the SHA-256 over its
/// `pyproject.toml`, `uv.lock` and `.python-version`, each import's
/// `pyproject.toml`, this runtime's version and the path, size and modification
/// time of each first-party module's wheel. `None` when the project
/// holds no `pyproject.toml`, which uv syncs nothing from.
fn the_venv_sync_stamp_of(
    project_directory: &Path,
    first_party_module_wheels: &[PathBuf],
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
    for module_wheel in first_party_module_wheels {
        stamp.update(module_wheel.as_os_str().as_encoded_bytes());
        if let Ok(wheel_metadata) = std::fs::metadata(module_wheel) {
            stamp.update(wheel_metadata.len().to_le_bytes());
            if let Ok(modified) = wheel_metadata.modified() {
                stamp.update(format!("{modified:?}").as_bytes());
            }
        }
    }
    Some(
        stamp
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

/// Each wheel file in `first_party_modules_directory`, in name order; none
/// when the directory is not there.
pub(crate) fn every_first_party_module_wheel_in(
    first_party_modules_directory: &Path,
) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(first_party_modules_directory) else {
        return Vec::new();
    };
    let mut module_wheels: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file() && path.extension().is_some_and(|extension| extension == "whl")
        })
        .collect();
    module_wheels.sort();
    module_wheels
}

/// Whether `venv` holds a `.dist-info` of every module of
/// `first_party_module_wheels`, which a command outside the runtime, such as
/// `uv pip uninstall`, may have taken out.
fn every_first_party_module_is_installed_in(
    venv: &Path,
    first_party_module_wheels: &[PathBuf],
) -> bool {
    let Ok(library_entries) = std::fs::read_dir(venv.join("lib")) else {
        return first_party_module_wheels.is_empty();
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
    first_party_module_wheels.iter().all(|module_wheel| {
        the_dist_info_prefix_of_the_module_wheel(module_wheel).is_some_and(|prefix| {
            installed_dist_info_names
                .iter()
                .any(|installed| installed.starts_with(&prefix))
        })
    })
}

/// `<name>-` as a `.dist-info` directory spells the distribution of
/// `module_wheel`: the wheel file name's own escaped name, lowercase.
fn the_dist_info_prefix_of_the_module_wheel(module_wheel: &Path) -> Option<String> {
    Some(format!(
        "{}-",
        the_escaped_distribution_name_of_the_module_wheel(module_wheel)?.to_ascii_lowercase()
    ))
}

/// The distribution `module_wheel` installs, from its file name: the wheel
/// spec writes the name's every `-` as `_`, and each first-party name is
/// `tatolab-<name>`, so the `-` spelling is restored.
fn the_distribution_name_of_the_module_wheel(module_wheel: &Path) -> Option<String> {
    Some(the_escaped_distribution_name_of_the_module_wheel(module_wheel)?.replace('_', "-"))
}

/// The distribution segment of `module_wheel`'s file name, before its first `-`.
fn the_escaped_distribution_name_of_the_module_wheel(module_wheel: &Path) -> Option<String> {
    let wheel_file_name = module_wheel.file_name()?.to_str()?;
    let (escaped_distribution_name, _) = wheel_file_name.split_once('-')?;
    Some(escaped_distribution_name.to_owned())
}

/// Write into the project's `.venv` what it is synced from now, so a load
/// finding the same runs no uv.
pub(crate) fn record_the_venv_synced_with_what_the_project_holds_now(
    project_directory: &Path,
    first_party_module_wheels: &[PathBuf],
) -> std::io::Result<()> {
    match the_venv_sync_stamp_of(project_directory, first_party_module_wheels) {
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

/// `uv sync` in `project_directory`, then the stamp naming what it was made
/// from, taken after the sync since it may write `uv.lock`; refused in uv's
/// own words.
fn sync_the_venv_with_the_runtime_unit_uv(
    project_directory: &Path,
    runtime_unit_uv: &Path,
    first_party_module_wheels: &[PathBuf],
    is_interrupted_by_the_host: &dyn Fn() -> bool,
    what_a_failed_sync_leaves: WhatAFailedSyncLeaves,
) -> Result<(), ProjectVenvNotMade> {
    let project = project_directory.display();
    let venv = project_directory.join(".venv");
    let leave_what_a_failed_sync_leaves = || match what_a_failed_sync_leaves {
        WhatAFailedSyncLeaves::NoVenv => remove_what_stands_at(&venv),
        WhatAFailedSyncLeaves::TheVenvAsItWas => Ok(()),
    };
    let an_interrupt_refusal = || {
        ProjectVenvNotMade::Refused(Error::Runtime(format!(
            "the .venv of {project} was not made: {}",
            what_interrupted_a_load()
        )))
    };
    let steps = [(
        "uv sync",
        project_venv_sync_command(runtime_unit_uv, project_directory),
    )];
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
        first_party_module_wheels,
    ) {
        tracing::warn!(
            venv = %venv.display(),
            %not_written,
            "the venv's sync stamp was not written, so its next load syncs it again"
        );
    }
    Ok(())
}

/// `<runtime_unit_uv> lock` in `project_directory`, each module of
/// `first_party_module_wheels` declared in the project first; refused in uv's
/// own words when the requirements its `pyproject.toml` declares cannot be
/// resolved.
pub(crate) fn lock_the_projects_python_requirements_with_the_runtime_unit_uv(
    project_directory: &Path,
    runtime_unit_uv: &Path,
    first_party_module_wheels: &[PathBuf],
) -> Result<(), String> {
    declare_each_first_party_module_in_the_project(project_directory, first_party_module_wheels)
        .map_err(|not_declared| Error::from(not_declared).to_string())?;
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
            first_party_module_wheels: &[PathBuf],
        ) -> Result<(), ProjectVenvNotMade> {
            make_the_projects_venv_current_with_the_runtime_unit_uv(
                &self.project_directory,
                Some(&self.stub_uv),
                first_party_module_wheels,
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

    /// A first-party module's wheel beside the project, of `distribution_name`.
    fn a_first_party_module_beside(
        project: &ProjectAndAStubUv,
        distribution_name: &str,
    ) -> PathBuf {
        let imports_directory = project
            .project_directory
            .parent()
            .unwrap()
            .join("runtime-unit-imports");
        std::fs::create_dir_all(&imports_directory).unwrap();
        let module_wheel = imports_directory.join(format!(
            "{}-0.68.51-py3-none-any.whl",
            distribution_name.replace('-', "_")
        ));
        std::fs::write(&module_wheel, "a wheel").unwrap();
        module_wheel
    }

    fn the_projects_pyproject(project: &ProjectAndAStubUv) -> toml::Table {
        std::fs::read_to_string(project.project_directory.join("pyproject.toml"))
            .unwrap()
            .parse()
            .unwrap()
    }

    #[test]
    fn each_first_party_module_is_linked_and_declared_so_one_uv_sync_installs_it() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        let ops = a_first_party_module_beside(&project, "tatolab-ops");

        project
            .make_the_venv_with_the_first_party_modules(std::slice::from_ref(&ops))
            .expect("the venv is made");

        assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
        assert_eq!(
            std::fs::read_link(
                project
                    .project_directory
                    .join(".tatolab/first-party-imports/tatolab_ops-0.68.51-py3-none-any.whl")
            )
            .unwrap(),
            ops
        );
        let pyproject = the_projects_pyproject(&project);
        assert_eq!(
            pyproject["project"]["dependencies"],
            toml::Value::try_from(["tatolab-ops"]).unwrap()
        );
        assert_eq!(
            pyproject["tool"]["uv"]["sources"]["tatolab-ops"]["path"].as_str(),
            Some(".tatolab/first-party-imports/tatolab_ops-0.68.51-py3-none-any.whl"),
            "a path source through the project's own link keeps uv.lock free of the runtime's path"
        );
    }

    #[test]
    fn a_link_to_another_runtimes_module_is_relinked_and_a_module_the_runtime_dropped_is_taken_out()
    {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        let ops = a_first_party_module_beside(&project, "tatolab-ops");
        let dropped = a_first_party_module_beside(&project, "tatolab-dropped");
        project
            .make_the_venv_with_the_first_party_modules(&[dropped.clone(), ops.clone()])
            .expect("the venv is made");
        let ops_link = project
            .project_directory
            .join(".tatolab/first-party-imports/tatolab_ops-0.68.51-py3-none-any.whl");
        std::fs::remove_file(&ops_link).unwrap();
        std::os::unix::fs::symlink(
            "/an/older/runtime/imports/tatolab_ops-0.68.51-py3-none-any.whl",
            &ops_link,
        )
        .unwrap();

        project
            .make_the_venv_with_the_first_party_modules(std::slice::from_ref(&ops))
            .expect("the venv is synced again");

        assert_eq!(std::fs::read_link(&ops_link).unwrap(), ops);
        assert!(
            !project
                .project_directory
                .join(".tatolab/first-party-imports/tatolab_dropped-0.68.51-py3-none-any.whl")
                .exists()
        );
        let pyproject = the_projects_pyproject(&project);
        assert_eq!(
            pyproject["project"]["dependencies"],
            toml::Value::try_from(["tatolab-ops"]).unwrap()
        );
        assert_eq!(
            pyproject["tool"]["uv"]["sources"]
                .as_table()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["tatolab-ops"]
        );
        assert_eq!(project.uv_runs().len(), 2, "{:?}", project.uv_runs());
    }

    #[test]
    fn a_runtime_carrying_no_first_party_module_leaves_the_pyproject_as_it_is() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);

        project.make_the_venv().expect("the venv is made");

        assert_eq!(
            std::fs::read_to_string(project.project_directory.join("pyproject.toml")).unwrap(),
            "[project]\nname = \"p\"\n"
        );
        assert!(!project.project_directory.join(".tatolab").exists());
    }

    #[test]
    fn a_running_venv_is_synced_again_once_a_first_party_module_changes() {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
        project.given_a_venv_interpreter_running("exit 0");
        project.given_the_venv_synced_with_what_the_project_holds_now();
        let ops = a_first_party_module_beside(&project, "tatolab-ops");

        project
            .make_the_venv_with_the_first_party_modules(&[ops])
            .expect("the venv is synced again");

        assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
    }

    #[test]
    fn a_running_venv_a_command_outside_the_runtime_took_a_first_party_module_out_of_is_synced_again()
     {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
        project.given_a_venv_interpreter_running("exit 0");
        let ops = a_first_party_module_beside(&project, "tatolab-ops");
        let site_packages = project.venv().join("lib/python3.12/site-packages");
        std::fs::create_dir_all(site_packages.join("tatolab_ops-0.68.41.dist-info")).unwrap();
        declare_each_first_party_module_in_the_project(
            &project.project_directory,
            std::slice::from_ref(&ops),
        )
        .unwrap();
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
        assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
    }

    #[test]
    fn the_first_party_modules_are_each_wheel_in_name_order() {
        let modules = tempfile::tempdir().unwrap();
        for file_name in [
            "tatolab_ops-0.68.51-py3-none-any.whl",
            "tatolab_h264-0.68.51-cp312-abi3-manylinux_2_28_x86_64.whl",
            "README.md",
        ] {
            std::fs::write(modules.path().join(file_name), "").unwrap();
        }
        std::fs::create_dir(modules.path().join("tatolab-camera")).unwrap();

        assert_eq!(
            every_first_party_module_wheel_in(modules.path()),
            [
                modules
                    .path()
                    .join("tatolab_h264-0.68.51-cp312-abi3-manylinux_2_28_x86_64.whl"),
                modules.path().join("tatolab_ops-0.68.51-py3-none-any.whl")
            ]
        );
        assert!(every_first_party_module_wheel_in(&modules.path().join("missing")).is_empty());
    }

    #[test]
    fn a_wheel_names_its_distribution_and_its_dist_info_prefix() {
        let module_wheel =
            Path::new("/imports/tatolab_h264-0.68.51-cp312-abi3-manylinux_2_28_x86_64.whl");
        assert_eq!(
            the_distribution_name_of_the_module_wheel(module_wheel).as_deref(),
            Some("tatolab-h264")
        );
        assert_eq!(
            the_dist_info_prefix_of_the_module_wheel(module_wheel).as_deref(),
            Some("tatolab_h264-")
        );
    }

    #[test]
    fn a_running_venv_is_synced_again_once_a_first_party_wheel_is_rebuilt() {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
        let ops = a_first_party_module_beside(&project, "tatolab-ops");
        let stamp_before =
            the_venv_sync_stamp_of(&project.project_directory, std::slice::from_ref(&ops));
        std::fs::write(&ops, "a rebuilt wheel of another size").unwrap();
        assert_ne!(
            the_venv_sync_stamp_of(&project.project_directory, std::slice::from_ref(&ops)),
            stamp_before
        );
    }

    #[test]
    fn a_lock_runs_uv_lock_in_the_project() {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
        lock_the_projects_python_requirements_with_the_runtime_unit_uv(
            &project.project_directory,
            &project.stub_uv,
            &[],
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
    fn a_lock_declares_each_first_party_module_before_uv_locks() {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
        let ops = a_first_party_module_beside(&project, "tatolab-ops");

        lock_the_projects_python_requirements_with_the_runtime_unit_uv(
            &project.project_directory,
            &project.stub_uv,
            &[ops],
        )
        .unwrap();

        assert_eq!(
            the_projects_pyproject(&project)["tool"]["uv"]["sources"]["tatolab-ops"]["path"]
                .as_str(),
            Some(".tatolab/first-party-imports/tatolab_ops-0.68.51-py3-none-any.whl")
        );
        assert_eq!(project.uv_runs().len(), 1, "{:?}", project.uv_runs());
    }

    #[test]
    fn a_lock_uv_cannot_resolve_is_refused_in_uvs_own_words() {
        let project = ProjectAndAStubUv::whose_uv_runs(
            "echo 'No solution found when resolving dependencies' >&2\nexit 1",
        );
        let refusal = lock_the_projects_python_requirements_with_the_runtime_unit_uv(
            &project.project_directory,
            &project.stub_uv,
            &[],
        )
        .unwrap_err();
        assert!(refusal.contains("`uv lock` failed"), "{refusal}");
        assert!(
            refusal.contains("No solution found when resolving dependencies"),
            "{refusal}"
        );
    }
}
