// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Syncing a project's `.venv` with the runtime unit's uv: installing the carried Python first, then the offline and index runs tried in turn.

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::core::compiler::compiler_ops::processor_interpreter_spawn_host::{
    HowABoundedChildProcessRunEnded, run_a_child_process_to_its_exit_within,
    run_a_child_process_to_its_exit_within_handing_on_its_standard_error_lines,
};
use crate::core::error::Error;
use crate::core::runtime::streams_loaded_in_this_runtime::project_stream_action_queue::StreamActionVenvSyncReport;
use crate::core::runtime::what_interrupted_a_load;
use streamlib_consumer_rhi::runtime_unit_layout::carried_python_downloads_metadata_file_in;

use super::project_venv_sync_stamp::{
    ProjectLockFileInTheVenvSyncStamp, the_venv_sync_stamp_with_the_lock_file,
};
use super::{
    PROJECT_VENV_SYNC_STAMP_FILE_NAME, PROJECT_VENV_SYNC_STANDARD_ERROR_LOG_LABEL,
    PackagesTheRuntimeUnitCarries, ProjectVenvNotMade, project_venv_uv_command,
    remove_what_stands_at, the_venv_sync_stamp_of,
};

/// What a sync that fails leaves of the project's `.venv`.
#[derive(Clone, Copy)]
pub(super) enum WhatAFailedSyncLeaves {
    /// The venv was made from nothing, so its half-made remains are removed.
    NoVenv,
    /// The venv's interpreter ran before the sync, so it is kept for the next one.
    TheVenvAsItWas,
}

/// The uv runs a sync or a lock tries in turn, each a step name and its command: offline first,
/// against the carried wheels and uv's cache, when the unit carries wheels, and then the same
/// run reaching the project's indexes.
pub(super) fn the_uv_runs_tried_in_turn(
    runtime_unit_uv: &Path,
    project_directory: &Path,
    uv_subcommand: &str,
    packages_the_runtime_unit_carries: &PackagesTheRuntimeUnitCarries,
) -> Vec<(String, Command)> {
    let mut runs = Vec::new();
    if packages_the_runtime_unit_carries
        .carried_wheels_directory
        .is_some()
    {
        let mut offline = project_venv_uv_command(runtime_unit_uv, project_directory);
        offline.args([uv_subcommand, "--offline"]);
        runs.push((format!("uv {uv_subcommand} --offline"), offline));
    }
    let mut reaching_the_indexes = project_venv_uv_command(runtime_unit_uv, project_directory);
    reaching_the_indexes.arg(uv_subcommand);
    runs.push((format!("uv {uv_subcommand}"), reaching_the_indexes));
    runs
}

/// Install the CPython build the unit carries for the project's `.python-version` from the
/// unit's uv mirror, unless uv finds a matching Python on the machine, and never into the
/// person's own `~/.local/bin` (`--no-bin`). A Python the mirror does not hold is left to the
/// sync, which downloads it; only an interrupt is refused.
fn install_the_carried_python_unless_the_machine_has_one(
    project_directory: &Path,
    runtime_unit_uv: &Path,
    carried_python_downloads_directory: &Path,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<(), ProjectVenvNotMade> {
    let mut find = project_venv_uv_command(runtime_unit_uv, project_directory);
    find.args(["python", "find"]);
    let mut install = project_venv_uv_command(runtime_unit_uv, project_directory);
    install.args(["python", "install", "--no-bin"]).env(
        UV_PYTHON_INSTALL_MIRROR_VARIABLE,
        the_file_url_of(carried_python_downloads_directory),
    );
    let carried_python_downloads_metadata_file =
        carried_python_downloads_metadata_file_in(carried_python_downloads_directory);
    if carried_python_downloads_metadata_file.is_file() {
        install.env(
            UV_PYTHON_DOWNLOADS_JSON_URL_VARIABLE,
            carried_python_downloads_metadata_file,
        );
    }
    for (step, mut command) in [("uv python find", find), ("uv python install", install)] {
        let run = match run_a_child_process_to_its_exit_within(
            &mut command,
            PROJECT_VENV_SYNC_STANDARD_ERROR_LOG_LABEL,
            Duration::MAX,
            is_interrupted_by_the_host,
        ) {
            Ok(run) => run,
            Err(not_started) => {
                tracing::warn!(%not_started, "`{step}` could not be started, so the sync finds its own Python");
                return Ok(());
            }
        };
        if matches!(
            run.ended,
            HowABoundedChildProcessRunEnded::InterruptedByTheHost
        ) {
            return Err(ProjectVenvNotMade::Refused(Error::Runtime(format!(
                "the .venv of {} was not made: {}",
                project_directory.display(),
                what_interrupted_a_load()
            ))));
        }
        if run.exited_successfully() {
            return Ok(());
        }
        if step == "uv python install" {
            tracing::warn!(
                "`uv python install` from the runtime unit's {} failed, so the sync finds its own \
                 Python: {}",
                carried_python_downloads_directory.display(),
                run.standard_error_as_a_refusal_quotes_it()
            );
        }
    }
    Ok(())
}

/// `uv` names the folder it reads Python downloads from instead of their release with this.
const UV_PYTHON_INSTALL_MIRROR_VARIABLE: &str = "UV_PYTHON_INSTALL_MIRROR";

/// `uv` reads the Python downloads it may install, each with the sha256 it checks, from the file
/// this names instead of the list built into it.
const UV_PYTHON_DOWNLOADS_JSON_URL_VARIABLE: &str = "UV_PYTHON_DOWNLOADS_JSON_URL";

/// `directory` as a `file://` URL.
fn the_file_url_of(directory: &Path) -> String {
    format!("file://{}", directory.display())
}

/// `uv sync` in `project_directory`, then the stamp naming what it was made
/// from, taken after the sync since it may write `uv.lock` and before the files
/// uv reads are compared with what they held as it began, and left unwritten
/// when one changed, so the next load syncs again;
/// refused in uv's own words. When the unit carries a Python it is installed first, and when it carries wheels
/// the sync runs offline first and reaches an index only when that fails.
pub(super) fn sync_the_venv_with_the_runtime_unit_uv(
    project_directory: &Path,
    runtime_unit_uv: &Path,
    packages_the_runtime_unit_carries: &PackagesTheRuntimeUnitCarries,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
    what_a_failed_sync_leaves: WhatAFailedSyncLeaves,
    report_of_the_venv_sync: Option<StreamActionVenvSyncReport>,
) -> Result<(), ProjectVenvNotMade> {
    let project = project_directory.display();
    let venv = project_directory.join(".venv");
    let what_uv_reads_before_the_sync = the_venv_sync_stamp_with_the_lock_file(
        project_directory,
        packages_the_runtime_unit_carries,
        ProjectLockFileInTheVenvSyncStamp::LeftOut,
    );
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
    if let Some(carried_python_downloads_directory) =
        &packages_the_runtime_unit_carries.carried_python_downloads_directory
        && let Err(interrupted) = install_the_carried_python_unless_the_machine_has_one(
            project_directory,
            runtime_unit_uv,
            carried_python_downloads_directory,
            is_interrupted_by_the_host,
        )
    {
        leave_what_a_failed_sync_leaves()?;
        return Err(interrupted);
    }
    let report_of_the_venv_sync_shared_by_each_run =
        report_of_the_venv_sync.map(|report_of_the_venv_sync| {
            report_of_the_venv_sync.hold_the_venv_sync_so_far();
            Arc::new(Mutex::new(report_of_the_venv_sync))
        });
    let runs = the_uv_runs_tried_in_turn(
        runtime_unit_uv,
        project_directory,
        "sync",
        packages_the_runtime_unit_carries,
    );
    let last_run = runs.len() - 1;
    for (run_index, (step, mut command)) in runs.into_iter().enumerate() {
        let each_uv_standard_error_line_taken_in = report_of_the_venv_sync_shared_by_each_run
            .clone()
            .map(|report_of_the_venv_sync| -> Box<dyn FnMut(&str) + Send> {
                Box::new(move |uv_standard_error_line| {
                    if let Ok(mut report_of_the_venv_sync) = report_of_the_venv_sync.lock() {
                        report_of_the_venv_sync
                            .take_in_a_uv_standard_error_line(uv_standard_error_line);
                    }
                })
            });
        let run = run_a_child_process_to_its_exit_within_handing_on_its_standard_error_lines(
            &mut command,
            PROJECT_VENV_SYNC_STANDARD_ERROR_LOG_LABEL,
            Duration::MAX,
            is_interrupted_by_the_host,
            each_uv_standard_error_line_taken_in,
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
            break;
        }
        let quoted_standard_error = run.standard_error_as_a_refusal_quotes_it();
        if matches!(
            run.ended,
            HowABoundedChildProcessRunEnded::InterruptedByTheHost
        ) {
            leave_what_a_failed_sync_leaves()?;
            return Err(an_interrupt_refusal());
        }
        if run_index < last_run {
            tracing::info!(
                "`{step}` could not sync {} from what the runtime carries and uv's cache, so \
                 it syncs from the project's indexes: {quoted_standard_error}",
                venv.display()
            );
            continue;
        }
        leave_what_a_failed_sync_leaves()?;
        return Err(ProjectVenvNotMade::Failed {
            failed_because: format!(
                "`{step}` failed for {}: it exited with {}. {quoted_standard_error}",
                venv.display(),
                run.exit_status_rendered()
            ),
        });
    }
    tracing::info!("`uv sync` made {}", venv.display());
    let stamp = venv.join(PROJECT_VENV_SYNC_STAMP_FILE_NAME);
    let sync_stamp_after_the_sync =
        the_venv_sync_stamp_of(project_directory, packages_the_runtime_unit_carries);
    let what_uv_reads_now = the_venv_sync_stamp_with_the_lock_file(
        project_directory,
        packages_the_runtime_unit_carries,
        ProjectLockFileInTheVenvSyncStamp::LeftOut,
    );
    if what_uv_reads_now != what_uv_reads_before_the_sync {
        tracing::info!(
            venv = %venv.display(),
            "a file uv syncs the venv from changed while it synced, so the venv is left \
             unstamped and its next load syncs it again"
        );
        if let Err(not_removed) = std::fs::remove_file(&stamp)
            && not_removed.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                stamp = %stamp.display(),
                %not_removed,
                "the venv's earlier sync stamp was not removed, so its next load may not sync it"
            );
        }
        return Ok(());
    }
    if let Some(sync_stamp_after_the_sync) = sync_stamp_after_the_sync
        && let Err(not_written) = std::fs::write(&stamp, sync_stamp_after_the_sync)
    {
        tracing::warn!(
            venv = %venv.display(),
            %not_written,
            "the venv's sync stamp was not written, so its next load syncs it again"
        );
    }
    Ok(())
}
