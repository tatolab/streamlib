// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Making a project's `.venv` with the uv the runtime unit carries, whenever
//! the venv's interpreter does not run. A venv whose interpreter runs is never
//! touched; one whose interpreter does not is removed and made again by
//! `uv sync`, which takes the Python the project's `.python-version` pins from
//! the machine or fetches it.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use crate::core::child_process_lifetime::{
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours,
    give_the_child_no_descriptor_beyond_stdio,
};
use crate::core::compiler::compiler_ops::processor_interpreter_shutdown_ladder::kill_the_process_group_and_reap_its_leader;
use crate::core::compiler::compiler_ops::processor_interpreter_spawn_host::{
    HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE, StreamInterpreterExitAwaited,
    remove_every_python_variable_named_in, spawn_standard_error_reader_keeping_its_tail,
    standard_error_tail_as_a_refusal_quotes_it, wait_for_a_stream_interpreter_to_exit,
};
use crate::core::compiler::compiler_ops::stream_function_compile_in_the_projects_interpreter::the_projects_venv_interpreter;
use crate::core::error::Error;
use crate::iceoryx2::spawn_outside_every_iceoryx2_listener_bind;

/// How long `.venv/bin/python -I -c ""` may take to show that the venv's
/// interpreter runs.
pub(crate) const PROJECT_VENV_INTERPRETER_PROBE_BOUND: Duration = Duration::from_secs(10);

/// How long one `uv sync` may run before its process group is killed: long
/// enough to fetch a Python and a project's dependencies.
pub(crate) const PROJECT_VENV_SYNC_BOUND: Duration = Duration::from_secs(240);

/// The longest making one project's venv takes: the probe, then the sync.
pub(crate) const PROJECT_VENV_MAKING_BOUND: Duration = Duration::from_secs(
    PROJECT_VENV_INTERPRETER_PROBE_BOUND.as_secs() + PROJECT_VENV_SYNC_BOUND.as_secs(),
);

/// Why a venv is not made by a runtime given no uv.
pub(crate) const THE_RUNTIME_UNIT_CARRIES_NO_UV: &str =
    "the runtime unit carries no uv; rebuild it with `cargo xtask build-runtime`";

/// The variables besides `PYTHON*` a sync is started without, so uv makes the
/// project's own `.venv` from its `.python-version`.
const VARIABLES_A_PROJECT_VENV_SYNC_IS_STARTED_WITHOUT: [&str; 2] =
    ["VIRTUAL_ENV", "UV_PROJECT_ENVIRONMENT"];

/// What a sync's standard-error lines are logged under.
const PROJECT_VENV_SYNC_STANDARD_ERROR_LOG_LABEL: &str = "project-venv-uv-sync";

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

/// Make the `.venv` of the project in `project_directory` with
/// `runtime_unit_uv` unless its `.venv/bin/python` runs. A machine shutdown
/// requested while uv runs kills it.
pub(crate) fn make_the_projects_venv_unless_its_interpreter_runs(
    project_directory: &Path,
    runtime_unit_uv: Option<&Path>,
) -> Result<(), ProjectVenvNotMade> {
    make_the_projects_venv_unless_its_interpreter_runs_within(
        project_directory,
        runtime_unit_uv,
        PROJECT_VENV_SYNC_BOUND,
        &crate::core::runtime::is_the_machines_shutdown_requested,
    )
}

/// [`make_the_projects_venv_unless_its_interpreter_runs`], its sync bounded by
/// `sync_bound` and cut short whenever `is_interrupted_by_the_host` reports
/// true.
pub(crate) fn make_the_projects_venv_unless_its_interpreter_runs_within(
    project_directory: &Path,
    runtime_unit_uv: Option<&Path>,
    sync_bound: Duration,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<(), ProjectVenvNotMade> {
    let project = project_directory.display();
    if !project_directory.is_absolute() {
        return Err(ProjectVenvNotMade::Refused(Error::Configuration(format!(
            "the project directory `{project}` is not an absolute path; a stream is compiled in \
             a project named by its absolute path"
        ))));
    }
    if !project_directory.is_dir() {
        return Err(ProjectVenvNotMade::Refused(Error::Configuration(format!(
            "the project directory {project} does not exist"
        ))));
    }
    let interpreter = the_projects_venv_interpreter(project_directory);
    if the_venv_interpreter_runs(&interpreter, project_directory) {
        return Ok(());
    }
    let Some(runtime_unit_uv) = runtime_unit_uv.filter(|uv| uv.is_file()) else {
        return Err(ProjectVenvNotMade::Failed {
            failed_because: format!(
                "{project}/.venv/bin/python does not run, and {THE_RUNTIME_UNIT_CARRIES_NO_UV}"
            ),
        });
    };
    let venv = project_directory.join(".venv");
    tracing::info!(
        "{} does not run, so {} is made again with {}",
        interpreter.display(),
        venv.display(),
        runtime_unit_uv.display()
    );
    remove_the_venv(&venv)?;
    if is_interrupted_by_the_host() {
        return Err(ProjectVenvNotMade::Refused(Error::Runtime(format!(
            "the .venv of {project} was not made: the runtime is shutting down"
        ))));
    }

    let mut child = spawn_outside_every_iceoryx2_listener_bind(&mut project_venv_sync_command(
        runtime_unit_uv,
        project_directory,
    ))
    .map_err(|spawn_failure| ProjectVenvNotMade::Failed {
        failed_because: format!(
            "`{} sync` could not be started in {project}: {spawn_failure}",
            runtime_unit_uv.display()
        ),
    })?;
    let standard_error_tail = child.stderr.take().map(|standard_error| {
        spawn_standard_error_reader_keeping_its_tail(
            standard_error,
            PROJECT_VENV_SYNC_STANDARD_ERROR_LOG_LABEL,
        )
    });
    let sync_exit = wait_for_a_stream_interpreter_to_exit(
        &child,
        sync_bound,
        &|| false,
        is_interrupted_by_the_host,
    );
    let exit_status = kill_the_process_group_and_reap_its_leader(&mut child, "the venv's uv sync");
    let quoted_standard_error = standard_error_tail_as_a_refusal_quotes_it(
        &standard_error_tail
            .map(|tail| {
                tail.text_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE)
            })
            .unwrap_or_default(),
    );

    let why_the_sync_did_not_make_it = match sync_exit {
        StreamInterpreterExitAwaited::Exited
            if exit_status.is_some_and(|exit_status| exit_status.success()) =>
        {
            tracing::info!("`uv sync` made {}", venv.display());
            return Ok(());
        }
        StreamInterpreterExitAwaited::Exited => format!(
            "`uv sync` did not make {}: it exited with {}. {quoted_standard_error}",
            venv.display(),
            exit_status
                .map(|exit_status| exit_status.to_string())
                .unwrap_or_else(|| "an exit status that could not be collected".to_string())
        ),
        StreamInterpreterExitAwaited::BoundElapsed => format!(
            "`uv sync` did not make {} within {}s, and was killed. {quoted_standard_error}",
            venv.display(),
            sync_bound.as_secs_f64()
        ),
        StreamInterpreterExitAwaited::ItsStreamsShutdownRequested
        | StreamInterpreterExitAwaited::InterruptedByTheHost => {
            remove_the_venv(&venv)?;
            return Err(ProjectVenvNotMade::Refused(Error::Runtime(format!(
                "the .venv of {project} was not made: the runtime began shutting down while \
                 `uv sync` made it, and it was killed"
            ))));
        }
    };
    remove_the_venv(&venv)?;
    Err(ProjectVenvNotMade::Failed {
        failed_because: why_the_sync_did_not_make_it,
    })
}

/// Whether `interpreter` starts and exits successfully within
/// [`PROJECT_VENV_INTERPRETER_PROBE_BOUND`].
fn the_venv_interpreter_runs(interpreter: &Path, project_directory: &Path) -> bool {
    let mut command = Command::new(interpreter);
    command
        .args(["-I", "-c", ""])
        .current_dir(project_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    let Ok(mut child) = spawn_outside_every_iceoryx2_listener_bind(&mut command) else {
        return false;
    };
    exited_successfully_within(&mut child, PROJECT_VENV_INTERPRETER_PROBE_BOUND)
}

/// Whether `child` exits successfully within `bound`; its process group is
/// killed either way.
fn exited_successfully_within(child: &mut Child, bound: Duration) -> bool {
    let exit = wait_for_a_stream_interpreter_to_exit(child, bound, &|| false, &|| false);
    let exit_status = kill_the_process_group_and_reap_its_leader(child, "the venv's interpreter");
    matches!(exit, StreamInterpreterExitAwaited::Exited)
        && exit_status.is_some_and(|exit_status| exit_status.success())
}

/// `<runtime_unit_uv> sync` in `project_directory`, started without the
/// variables that would point uv at another environment or Python.
fn project_venv_sync_command(runtime_unit_uv: &Path, project_directory: &Path) -> Command {
    let mut command = Command::new(runtime_unit_uv);
    command
        .arg("sync")
        .current_dir(project_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    remove_every_python_variable_named_in(
        &mut command,
        std::env::vars_os().map(|(variable_name, _)| variable_name),
    );
    for variable_name in VARIABLES_A_PROJECT_VENV_SYNC_IS_STARTED_WITHOUT {
        command.env_remove(variable_name);
    }
    detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
    give_the_child_no_descriptor_beyond_stdio(&mut command);
    command
}

/// Remove `venv` whatever it is — a directory, a file or a dangling link —
/// when it is there.
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
            self.make_the_venv_within(PROJECT_VENV_SYNC_BOUND)
        }

        fn make_the_venv_within(&self, sync_bound: Duration) -> Result<(), ProjectVenvNotMade> {
            make_the_projects_venv_unless_its_interpreter_runs_within(
                &self.project_directory,
                Some(&self.stub_uv),
                sync_bound,
                &|| false,
            )
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
    fn a_venv_whose_interpreter_runs_is_untouched_and_uv_never_runs() {
        let project = ProjectAndAStubUv::whose_uv_runs("exit 1");
        project.given_a_venv_interpreter_running("exit 0");
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
            failed_because.contains("`uv sync` did not make"),
            "{failed_because}"
        );
        assert!(!project.venv().exists(), "the half-made venv was left");
    }

    #[test]
    fn a_sync_past_its_bound_is_killed_failing_naming_the_bound() {
        let project = ProjectAndAStubUv::whose_uv_runs("mkdir -p .venv/bin\nsleep 30");

        let failed_because =
            the_reason_it_failed(project.make_the_venv_within(Duration::from_millis(500)));

        assert!(failed_because.contains("within 0.5s"), "{failed_because}");
        assert!(!project.venv().exists(), "the half-made venv was left");
    }

    #[test]
    fn a_runtime_given_no_uv_fails_saying_the_unit_carries_none_and_leaves_the_venv() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        project.given_a_venv_interpreter_running("exit 3");

        let failed_because =
            the_reason_it_failed(make_the_projects_venv_unless_its_interpreter_runs_within(
                &project.project_directory,
                None,
                PROJECT_VENV_SYNC_BOUND,
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
            the_refusal(make_the_projects_venv_unless_its_interpreter_runs_within(
                &project_directory,
                Some(&project.stub_uv),
                PROJECT_VENV_SYNC_BOUND,
                &|| false,
            ));
        }
        assert!(project.uv_runs().is_empty(), "{:?}", project.uv_runs());
    }

    #[test]
    fn a_sync_the_host_interrupts_is_refused_and_removes_the_half_made_venv() {
        let project = ProjectAndAStubUv::whose_uv_runs("mkdir -p .venv/bin\nsleep 30");
        let interrupted_after = std::time::Instant::now() + Duration::from_millis(300);

        let refusal = the_refusal(make_the_projects_venv_unless_its_interpreter_runs_within(
            &project.project_directory,
            Some(&project.stub_uv),
            PROJECT_VENV_SYNC_BOUND,
            &|| std::time::Instant::now() >= interrupted_after,
        ));

        assert!(refusal.contains("shutting down"), "{refusal}");
        assert!(!project.venv().exists(), "the half-made venv was left");
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
}
