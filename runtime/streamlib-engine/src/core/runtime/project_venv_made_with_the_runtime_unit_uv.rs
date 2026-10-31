// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Making a project's `.venv` with the uv the runtime unit carries, whenever
//! the venv's interpreter does not run. A venv whose interpreter runs is never
//! touched; one whose interpreter does not is removed and made again by
//! `uv sync`, which takes the Python the project's `.python-version` pins from
//! the machine or fetches it.

use std::path::Path;
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
use crate::core::compiler::compiler_ops::stream_function_compile_in_the_projects_interpreter::{
    refuse_a_project_directory_not_named_by_its_absolute_path, the_projects_venv_interpreter,
};
use crate::core::error::Error;

/// How long `.venv/bin/python -I -c ""` may take to show that the venv's
/// interpreter runs.
pub(crate) const PROJECT_VENV_INTERPRETER_PROBE_BOUND: Duration = Duration::from_secs(10);

/// How long one `uv sync` may run: without end, since one large wheel can
/// outlast any bound; uv's own network timeouts end a stalled download.
const PROJECT_VENV_SYNC_RUNS_UNTIL_IT_EXITS: Duration = Duration::MAX;

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

/// Make the `.venv` of the project in `project_directory` with
/// `runtime_unit_uv` unless its `.venv/bin/python` runs. A machine shutdown,
/// or a cancel of the stream action making it on this thread, cuts it short.
pub(crate) fn make_the_projects_venv_unless_its_interpreter_runs(
    project_directory: &Path,
    runtime_unit_uv: Option<&Path>,
) -> Result<(), ProjectVenvNotMade> {
    make_the_projects_venv_unless_its_interpreter_runs_interrupted_by(
        project_directory,
        runtime_unit_uv,
        &crate::core::runtime::is_this_threads_stream_work_interrupted,
    )
}

/// [`make_the_projects_venv_unless_its_interpreter_runs`], cut short whenever
/// `is_interrupted_by_the_host` reports true.
pub(crate) fn make_the_projects_venv_unless_its_interpreter_runs_interrupted_by(
    project_directory: &Path,
    runtime_unit_uv: Option<&Path>,
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
    let a_shutdown_refusal = || {
        ProjectVenvNotMade::Refused(Error::Runtime(format!(
            "the .venv of {project} was not made: the runtime is shutting down, or the stream \
             action making it was cancelled"
        )))
    };
    let interpreter = the_projects_venv_interpreter(project_directory);
    let why_the_interpreter_does_not_run = match probe_the_venv_interpreter(
        &interpreter,
        project_directory,
        is_interrupted_by_the_host,
    ) {
        VenvInterpreterProbe::Runs => return Ok(()),
        VenvInterpreterProbe::CutShortByTheHost => return Err(a_shutdown_refusal()),
        VenvInterpreterProbe::DoesNotRun { why } => why,
    };
    let Some(runtime_unit_uv) = runtime_unit_uv.filter(|uv| uv.is_file()) else {
        return Err(ProjectVenvNotMade::Failed {
            failed_because: format!(
                "{why_the_interpreter_does_not_run}, and {THE_RUNTIME_UNIT_CARRIES_NO_UV}"
            ),
        });
    };
    if !project_directory.join("pyproject.toml").is_file() {
        return Err(ProjectVenvNotMade::Failed {
            failed_because: format!(
                "{why_the_interpreter_does_not_run}, and {project} holds no pyproject.toml to \
                 make its .venv from: uv makes a project's .venv from the project's own \
                 pyproject.toml, and in a folder without one it would sync the project above it"
            ),
        });
    }
    if is_interrupted_by_the_host() {
        return Err(a_shutdown_refusal());
    }
    let venv = project_directory.join(".venv");
    tracing::info!(
        "{why_the_interpreter_does_not_run}, so {} is made again with {}",
        venv.display(),
        runtime_unit_uv.display()
    );
    remove_the_venv(&venv)?;

    let sync = run_a_child_process_to_its_exit_within(
        &mut project_venv_sync_command(runtime_unit_uv, project_directory),
        PROJECT_VENV_SYNC_STANDARD_ERROR_LOG_LABEL,
        PROJECT_VENV_SYNC_RUNS_UNTIL_IT_EXITS,
        is_interrupted_by_the_host,
    )
    .map_err(|spawn_failure| ProjectVenvNotMade::Failed {
        failed_because: format!(
            "`{} sync` could not be started in {project}: {spawn_failure}",
            runtime_unit_uv.display()
        ),
    })?;
    if sync.exited_successfully() {
        tracing::info!("`uv sync` made {}", venv.display());
        return Ok(());
    }
    let quoted_standard_error = sync.standard_error_as_a_refusal_quotes_it();
    let failed_because = match sync.ended {
        HowABoundedChildProcessRunEnded::Exited | HowABoundedChildProcessRunEnded::BoundElapsed => {
            format!(
                "`uv sync` did not make {}: it exited with {}. {quoted_standard_error}",
                venv.display(),
                sync.exit_status_rendered()
            )
        }
        HowABoundedChildProcessRunEnded::InterruptedByTheHost => {
            remove_the_venv(&venv)?;
            return Err(a_shutdown_refusal());
        }
    };
    remove_the_venv(&venv)?;
    Err(ProjectVenvNotMade::Failed { failed_because })
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
    let mut command = a_command_detached_from_the_terminal_in(runtime_unit_uv, project_directory);
    command.arg("sync").stderr(Stdio::piped());
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
            make_the_projects_venv_unless_its_interpreter_runs_interrupted_by(
                &self.project_directory,
                Some(&self.stub_uv),
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
            failed_because.contains("`uv sync` did not make"),
            "{failed_because}"
        );
        assert!(!project.venv().exists(), "the half-made venv was left");
    }

    #[test]
    fn a_runtime_given_no_uv_fails_saying_the_unit_carries_none_and_leaves_the_venv() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        project.given_a_venv_interpreter_running("exit 3");

        let failed_because = the_reason_it_failed(
            make_the_projects_venv_unless_its_interpreter_runs_interrupted_by(
                &project.project_directory,
                None,
                &|| false,
            ),
        );

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
            the_refusal(
                make_the_projects_venv_unless_its_interpreter_runs_interrupted_by(
                    &project_directory,
                    Some(&project.stub_uv),
                    &|| false,
                ),
            );
        }
        assert!(project.uv_runs().is_empty(), "{:?}", project.uv_runs());
    }

    #[test]
    fn a_sync_the_host_interrupts_is_refused_and_removes_the_half_made_venv() {
        let project = ProjectAndAStubUv::whose_uv_runs("mkdir -p .venv/bin\nsleep 30");
        let interrupted_after = std::time::Instant::now() + Duration::from_millis(300);

        let refusal = the_refusal(
            make_the_projects_venv_unless_its_interpreter_runs_interrupted_by(
                &project.project_directory,
                Some(&project.stub_uv),
                &|| std::time::Instant::now() >= interrupted_after,
            ),
        );

        assert!(refusal.contains("shutting down"), "{refusal}");
        assert!(!project.venv().exists(), "the half-made venv was left");
    }

    #[test]
    fn a_probe_the_host_interrupts_is_refused_and_never_fails_the_project() {
        let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
        project.given_a_venv_interpreter_running("sleep 30");
        let interrupted_after = std::time::Instant::now() + Duration::from_millis(300);

        let refusal = the_refusal(
            make_the_projects_venv_unless_its_interpreter_runs_interrupted_by(
                &project.project_directory,
                None,
                &|| std::time::Instant::now() >= interrupted_after,
            ),
        );

        assert!(refusal.contains("shutting down"), "{refusal}");
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
}
