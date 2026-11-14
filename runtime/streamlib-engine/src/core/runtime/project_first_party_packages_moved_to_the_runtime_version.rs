// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Moving a project's first-party packages up to this runtime's version before it loads. A
//! package the project's `uv.lock` takes from an index — `tatolab-stream` or a `tatolab-*`
//! module — at a release below the runtime's is locked again at exactly the runtime's version,
//! and nothing else is let move, so a person's other dependencies change only where the new
//! packages require it. The new `uv.lock` is one transaction by the runtime on the project's
//! history, under a key naming the runtime's version, so an undo of it holds until a later
//! runtime.

use std::path::Path;
use std::time::Duration;

use crate::core::compiler::compiler_ops::processor_interpreter_spawn_host::{
    HowABoundedChildProcessRunEnded, run_a_child_process_to_its_exit_within,
};
use crate::core::error::Error;
use crate::core::runtime::project_venv_made_with_the_runtime_unit_uv::{
    ProjectVenvNotMade, project_venv_uv_command,
};
use crate::core::runtime::what_interrupted_a_load;

/// The project history the host hands the runtime to record the first-party package moves it
/// makes, so each is one transaction the runtime authored and an undo can reverse.
pub trait ProjectHistoryRecordingFirstPartyPackageMoves: Send + Sync {
    /// Whether the history of the project in `project_directory` holds a transaction under
    /// `move_key`, undone since or not; the error says why it could not be read.
    fn holds_a_move_recorded_under(
        &self,
        project_directory: &Path,
        move_key: &str,
    ) -> std::result::Result<bool, String>;

    /// Write `uv_lock` as the `uv.lock` of the project in `project_directory`, as one
    /// transaction by the runtime under `move_key`; the error says why it was not recorded.
    fn record_the_moved_uv_lock(
        &self,
        project_directory: &Path,
        move_key: &str,
        uv_lock: &[u8],
    ) -> std::result::Result<(), String>;

    /// Move each node of a `tatolab-ops` type in the stream documents of the project in
    /// `project_directory` to the Core Nodes type that replaced it, as one transaction by the
    /// runtime made once in the project's history; the error says why it was not recorded.
    fn move_each_tatolab_ops_node_to_core_nodes(
        &self,
        project_directory: &Path,
    ) -> std::result::Result<(), String>;
}

/// A first-party package a project's `uv.lock` takes from an index at a release below the
/// runtime's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FirstPartyPackageLockedBelowTheRuntimeVersion {
    /// Its normalized name, as `uv.lock` spells it.
    pub(crate) name: String,
    /// The version `uv.lock` holds.
    pub(crate) locked_version: String,
}

/// The `uv.lock` beside a project's `pyproject.toml`.
const PROJECT_UV_LOCK_FILE_NAME: &str = "uv.lock";

/// What a move's `uv lock` standard-error lines are logged under.
const FIRST_PARTY_PACKAGE_MOVE_STANDARD_ERROR_LOG_LABEL: &str = "project-first-party-package-move";

/// The idempotency key of the move to `runtime_version`, the same in every project.
pub(crate) fn the_first_party_package_move_key_to(runtime_version: &str) -> String {
    format!("tatolab-runtime-moves-first-party-packages-to-{runtime_version}")
}

/// Whether `name` is one of Tatolab's own: `tatolab-stream` or a `tatolab-*` module.
fn is_a_first_party_package_name(name: &str) -> bool {
    name.starts_with("tatolab-")
}

/// The leading release numbers of a PEP 440 version, `0.68.27` of `0.68.27rc1`; `None` when
/// it starts with no number.
fn release_numbers_of(version: &str) -> Option<Vec<u64>> {
    let mut release_numbers = Vec::new();
    for segment in version.split('.') {
        let digits: String = segment.chars().take_while(char::is_ascii_digit).collect();
        let Ok(number) = digits.parse() else {
            break;
        };
        release_numbers.push(number);
        if digits.len() != segment.len() {
            break;
        }
    }
    (!release_numbers.is_empty()).then_some(release_numbers)
}

/// Whether the release `version` comes before `runtime_version`, a missing trailing number
/// counted as zero.
fn is_a_release_below(version: &[u64], runtime_version: &[u64]) -> bool {
    let compared_length = version.len().max(runtime_version.len());
    let padded = |release: &[u64]| -> Vec<u64> {
        let mut padded = release.to_vec();
        padded.resize(compared_length, 0);
        padded
    };
    padded(version) < padded(runtime_version)
}

/// Each first-party package `uv_lock` takes from an index at a release below
/// `runtime_version`, in the order it lists them. A package from a path, a directory, Git or
/// the project itself is never one: it moves with its source, not with an index.
pub(crate) fn first_party_packages_locked_below(
    uv_lock: &str,
    runtime_version: &str,
) -> Vec<FirstPartyPackageLockedBelowTheRuntimeVersion> {
    let Some(runtime_release) = release_numbers_of(runtime_version) else {
        return Vec::new();
    };
    let Ok(lock) = uv_lock.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Some(packages) = lock.get("package").and_then(toml::Value::as_array) else {
        return Vec::new();
    };
    packages
        .iter()
        .filter_map(|package| {
            let name = package.get("name")?.as_str()?;
            let locked_version = package.get("version")?.as_str()?;
            let from_an_index = package.get("source")?.get("registry").is_some();
            let below = is_a_release_below(&release_numbers_of(locked_version)?, &runtime_release);
            (from_an_index && below && is_a_first_party_package_name(name)).then(|| {
                FirstPartyPackageLockedBelowTheRuntimeVersion {
                    name: name.to_owned(),
                    locked_version: locked_version.to_owned(),
                }
            })
        })
        .collect()
}

/// Move each first-party package the `uv.lock` of the project in `project_directory` takes
/// below `runtime_version` up to it with `runtime_unit_uv`, recorded in `history` when the host
/// handed one. A move `history` already holds under its key, undone since or not, is not made
/// again. A move that fails leaves `uv.lock` as it was and is logged in uv's own words: the load
/// goes on with the packages the project holds. Refused only when `is_interrupted_by_the_host`
/// cut it short.
pub(crate) fn move_the_projects_first_party_packages_to_the_runtime_version(
    project_directory: &Path,
    runtime_unit_uv: &Path,
    runtime_version: &str,
    history: Option<&dyn ProjectHistoryRecordingFirstPartyPackageMoves>,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> Result<(), ProjectVenvNotMade> {
    let uv_lock_path = project_directory.join(PROJECT_UV_LOCK_FILE_NAME);
    let Ok(uv_lock_as_it_was) = std::fs::read(&uv_lock_path) else {
        return Ok(());
    };
    let packages_below = first_party_packages_locked_below(
        &String::from_utf8_lossy(&uv_lock_as_it_was),
        runtime_version,
    );
    if packages_below.is_empty() {
        return Ok(());
    }
    let project = project_directory.display();
    let move_key = the_first_party_package_move_key_to(runtime_version);
    if let Some(history) = history {
        match history.holds_a_move_recorded_under(project_directory, &move_key) {
            Ok(true) => {
                tracing::info!(
                    "{project} keeps its first-party packages below {runtime_version}: its history \
                     already holds their move to {runtime_version}, undone since"
                );
                return Ok(());
            }
            Ok(false) => {}
            Err(unread) => tracing::warn!(
                "the history of {project} was not read for an earlier move of its first-party \
                 packages, so they are moved: {unread}"
            ),
        }
    }
    let packages_named = packages_below
        .iter()
        .map(|package| format!("{} {}", package.name, package.locked_version))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!(
        "{project} locks {packages_named}, below this runtime's {runtime_version}, so they are \
         moved to {runtime_version}"
    );
    let put_the_uv_lock_back = || {
        if let Err(not_written) = std::fs::write(&uv_lock_path, &uv_lock_as_it_was) {
            tracing::warn!(
                "{} was not put back as it was: {not_written}",
                uv_lock_path.display()
            );
        }
    };
    let mut command = project_venv_uv_command(runtime_unit_uv, project_directory);
    command.arg("lock");
    for package in &packages_below {
        command
            .arg("--upgrade-package")
            .arg(format!("{}=={runtime_version}", package.name));
    }
    let run = match run_a_child_process_to_its_exit_within(
        &mut command,
        FIRST_PARTY_PACKAGE_MOVE_STANDARD_ERROR_LOG_LABEL,
        Duration::MAX,
        is_interrupted_by_the_host,
    ) {
        Ok(run) => run,
        Err(not_started) => {
            tracing::warn!(
                "the first-party packages of {project} were not moved to {runtime_version}: `{} \
                 lock` could not be started: {not_started}",
                runtime_unit_uv.display()
            );
            return Ok(());
        }
    };
    if matches!(
        run.ended,
        HowABoundedChildProcessRunEnded::InterruptedByTheHost
    ) {
        put_the_uv_lock_back();
        return Err(ProjectVenvNotMade::Refused(Error::Runtime(format!(
            "the first-party packages of {project} were not moved: {}",
            what_interrupted_a_load()
        ))));
    }
    if !run.exited_successfully() {
        put_the_uv_lock_back();
        tracing::warn!(
            "the first-party packages of {project} were not moved to {runtime_version}: `uv lock` \
             exited with {}. {}",
            run.exit_status_rendered(),
            run.standard_error_as_a_refusal_quotes_it()
        );
        return Ok(());
    }
    let uv_lock_moved = match std::fs::read(&uv_lock_path) {
        Ok(uv_lock_moved) => uv_lock_moved,
        Err(unread) => {
            put_the_uv_lock_back();
            tracing::warn!(
                "the first-party packages of {project} were not moved: the new {} was not read: \
                 {unread}",
                uv_lock_path.display()
            );
            return Ok(());
        }
    };
    if uv_lock_moved == uv_lock_as_it_was {
        return Ok(());
    }
    let Some(history) = history else {
        tracing::info!(
            "the first-party packages of {project} moved to {runtime_version}; this runtime \
             records no project history"
        );
        return Ok(());
    };
    put_the_uv_lock_back();
    match history.record_the_moved_uv_lock(project_directory, &move_key, &uv_lock_moved) {
        Ok(()) => tracing::info!(
            "the first-party packages of {project} moved to {runtime_version}, recorded in its \
             history"
        ),
        Err(not_recorded) => {
            tracing::warn!(
                "the move of the first-party packages of {project} to {runtime_version} was not \
                 recorded in its history, so it is written without an undo: {not_recorded}"
            );
            if let Err(not_written) = std::fs::write(&uv_lock_path, &uv_lock_moved) {
                tracing::warn!(
                    "the moved {} was not written: {not_written}",
                    uv_lock_path.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::test_support::{
        a_temporary_directory_at_owner_only_mode, write_an_executable_script_from_a_child_process,
    };
    use parking_lot::Mutex;
    use std::path::PathBuf;

    const RUNTIME_VERSION: &str = "0.68.29";

    fn a_locked_package(name: &str, version: &str, source: &str) -> String {
        format!("[[package]]\nname = \"{name}\"\nversion = \"{version}\"\nsource = {source}\n\n")
    }

    const FROM_AN_INDEX: &str = "{ registry = \"https://get.tatolab.com/simple/\" }";

    fn a_uv_lock_of(packages: &[String]) -> String {
        format!(
            "version = 1\nrequires-python = \">=3.12\"\n\n{}",
            packages.concat()
        )
    }

    fn the_names_of(packages: Vec<FirstPartyPackageLockedBelowTheRuntimeVersion>) -> Vec<String> {
        packages.into_iter().map(|package| package.name).collect()
    }

    #[test]
    fn each_first_party_package_an_index_serves_below_the_runtime_version_is_found() {
        let uv_lock = a_uv_lock_of(&[
            a_locked_package(
                "numpy",
                "1.0.0",
                "{ registry = \"https://pypi.org/simple\" }",
            ),
            a_locked_package(
                "tatolab-stream",
                "0.68.27",
                "{ registry = \"https://pypi.org/simple\" }",
            ),
            a_locked_package("tatolab-testpattern", "0.68.27", FROM_AN_INDEX),
            a_locked_package("tatolab-h264", "0.68.29", FROM_AN_INDEX),
        ]);

        assert_eq!(
            first_party_packages_locked_below(&uv_lock, RUNTIME_VERSION),
            vec![
                FirstPartyPackageLockedBelowTheRuntimeVersion {
                    name: "tatolab-stream".to_owned(),
                    locked_version: "0.68.27".to_owned(),
                },
                FirstPartyPackageLockedBelowTheRuntimeVersion {
                    name: "tatolab-testpattern".to_owned(),
                    locked_version: "0.68.27".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn a_first_party_package_from_a_path_or_the_project_itself_is_never_moved() {
        let uv_lock = a_uv_lock_of(&[
            a_locked_package("tatolab-stream-project", "0.1.0", "{ editable = \".\" }"),
            a_locked_package(
                "tatolab-compositor",
                "0.68.27",
                "{ directory = \".tatolab/first-party-imports/tatolab-compositor\" }",
            ),
            a_locked_package("tatolab-desk", "0.1.0", "{ virtual = \"imports/desk\" }"),
        ]);

        assert!(first_party_packages_locked_below(&uv_lock, RUNTIME_VERSION).is_empty());
    }

    #[test]
    fn a_package_at_or_above_the_runtime_version_is_current() {
        let uv_lock = a_uv_lock_of(&[
            a_locked_package("tatolab-stream", "0.68.29", FROM_AN_INDEX),
            a_locked_package("tatolab-opus", "0.69.0", FROM_AN_INDEX),
            a_locked_package("tatolab-h265", "0.68.29.post1", FROM_AN_INDEX),
        ]);

        assert!(first_party_packages_locked_below(&uv_lock, RUNTIME_VERSION).is_empty());
    }

    #[test]
    fn releases_compare_by_their_numbers_not_their_spelling() {
        let uv_lock = a_uv_lock_of(&[
            a_locked_package("tatolab-stream", "0.9.0", FROM_AN_INDEX),
            a_locked_package("tatolab-opus", "0.68", FROM_AN_INDEX),
            a_locked_package("tatolab-h264", "0.68.29rc1", FROM_AN_INDEX),
        ]);

        assert_eq!(
            the_names_of(first_party_packages_locked_below(&uv_lock, RUNTIME_VERSION)),
            vec!["tatolab-stream", "tatolab-opus"]
        );
    }

    #[test]
    fn a_uv_lock_that_does_not_parse_holds_nothing_to_move() {
        assert!(first_party_packages_locked_below("[[package", RUNTIME_VERSION).is_empty());
    }

    /// A project holding `uv_lock`, and a stub uv that writes its arguments to `uv-runs` and
    /// then runs `body` in the project.
    struct ProjectLockingWithAStubUv {
        project_directory: PathBuf,
        stub_uv: PathBuf,
        uv_runs: PathBuf,
        _directory_removed_on_drop: tempfile::TempDir,
    }

    impl ProjectLockingWithAStubUv {
        fn holding(uv_lock: &str, body: &str) -> Self {
            let temporary_directory =
                a_temporary_directory_at_owner_only_mode().expect("a temporary directory");
            let project_directory = temporary_directory.path().join("project");
            std::fs::create_dir(&project_directory).expect("the project directory");
            std::fs::write(project_directory.join("uv.lock"), uv_lock).expect("the uv.lock");
            let uv_runs = temporary_directory.path().join("uv-runs");
            let stub_uv = temporary_directory.path().join("uv");
            write_an_executable_script_from_a_child_process(
                &stub_uv,
                &format!(
                    "#!/bin/sh\necho \"$*\" >> '{}'\n{body}\n",
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

        fn move_with(
            &self,
            history: Option<&dyn ProjectHistoryRecordingFirstPartyPackageMoves>,
        ) -> Result<(), ProjectVenvNotMade> {
            move_the_projects_first_party_packages_to_the_runtime_version(
                &self.project_directory,
                &self.stub_uv,
                RUNTIME_VERSION,
                history,
                &|| false,
            )
        }

        fn uv_lock(&self) -> String {
            std::fs::read_to_string(self.project_directory.join("uv.lock")).expect("the uv.lock")
        }

        fn uv_runs(&self) -> String {
            std::fs::read_to_string(&self.uv_runs).unwrap_or_default()
        }
    }

    /// A history that holds the keys of `moves_recorded_before` and records each move.
    #[derive(Default)]
    struct HistoryRecordingInMemory {
        moves_recorded_before: Vec<String>,
        refuses_to_record: bool,
        recorded: Mutex<Vec<(String, String)>>,
    }

    impl ProjectHistoryRecordingFirstPartyPackageMoves for HistoryRecordingInMemory {
        fn holds_a_move_recorded_under(
            &self,
            _project_directory: &Path,
            move_key: &str,
        ) -> std::result::Result<bool, String> {
            Ok(self.moves_recorded_before.iter().any(|key| key == move_key))
        }

        fn record_the_moved_uv_lock(
            &self,
            _project_directory: &Path,
            move_key: &str,
            uv_lock: &[u8],
        ) -> std::result::Result<(), String> {
            if self.refuses_to_record {
                return Err("the history is not writable".to_owned());
            }
            self.recorded.lock().push((
                move_key.to_owned(),
                String::from_utf8_lossy(uv_lock).into_owned(),
            ));
            Ok(())
        }

        fn move_each_tatolab_ops_node_to_core_nodes(
            &self,
            _project_directory: &Path,
        ) -> std::result::Result<(), String> {
            unreachable!("a first-party package move never moves a project's nodes")
        }
    }

    fn a_project_locking_tatolab_stream_at_0_68_27(body: &str) -> ProjectLockingWithAStubUv {
        ProjectLockingWithAStubUv::holding(
            &a_uv_lock_of(&[
                a_locked_package("numpy", "2.1.0", FROM_AN_INDEX),
                a_locked_package("tatolab-stream", "0.68.27", FROM_AN_INDEX),
                a_locked_package("tatolab-testpattern", "0.68.27", FROM_AN_INDEX),
            ]),
            body,
        )
    }

    const WRITES_THE_MOVED_LOCK: &str = "echo 'the moved lock' > uv.lock";

    #[test]
    fn only_the_first_party_packages_below_the_runtime_are_let_move_each_to_its_version() {
        let project = a_project_locking_tatolab_stream_at_0_68_27(WRITES_THE_MOVED_LOCK);

        project.move_with(None).expect("the move");

        assert_eq!(
            project.uv_runs(),
            "lock --upgrade-package tatolab-stream==0.68.29 \
             --upgrade-package tatolab-testpattern==0.68.29\n"
        );
        assert_eq!(project.uv_lock(), "the moved lock\n");
    }

    #[test]
    fn the_move_is_recorded_as_one_transaction_under_the_runtime_versions_key() {
        let project = a_project_locking_tatolab_stream_at_0_68_27(WRITES_THE_MOVED_LOCK);
        let lock_as_it_was = project.uv_lock();
        let history = HistoryRecordingInMemory::default();

        project.move_with(Some(&history)).expect("the move");

        assert_eq!(
            *history.recorded.lock(),
            vec![(
                "tatolab-runtime-moves-first-party-packages-to-0.68.29".to_owned(),
                "the moved lock\n".to_owned()
            )]
        );
        assert_eq!(
            project.uv_lock(),
            lock_as_it_was,
            "the history's transaction writes the moved lock, not the move itself"
        );
    }

    #[test]
    fn a_move_the_history_already_holds_is_not_made_again_so_its_undo_holds() {
        let project = a_project_locking_tatolab_stream_at_0_68_27(WRITES_THE_MOVED_LOCK);
        let lock_as_it_was = project.uv_lock();
        let history = HistoryRecordingInMemory {
            moves_recorded_before: vec![the_first_party_package_move_key_to(RUNTIME_VERSION)],
            ..HistoryRecordingInMemory::default()
        };

        project.move_with(Some(&history)).expect("no move");

        assert_eq!(project.uv_runs(), "");
        assert!(history.recorded.lock().is_empty());
        assert_eq!(project.uv_lock(), lock_as_it_was);
    }

    #[test]
    fn a_move_the_history_will_not_record_is_still_written() {
        let project = a_project_locking_tatolab_stream_at_0_68_27(WRITES_THE_MOVED_LOCK);
        let history = HistoryRecordingInMemory {
            refuses_to_record: true,
            ..HistoryRecordingInMemory::default()
        };

        project.move_with(Some(&history)).expect("the move");

        assert_eq!(project.uv_lock(), "the moved lock\n");
    }

    #[test]
    fn a_failed_lock_leaves_the_uv_lock_as_it_was_and_the_load_goes_on() {
        let project = a_project_locking_tatolab_stream_at_0_68_27(
            "echo 'half written' > uv.lock\necho 'error: no version of tatolab-stream==0.68.29' >&2\nexit 1",
        );
        let lock_as_it_was = project.uv_lock();
        let history = HistoryRecordingInMemory::default();

        project.move_with(Some(&history)).expect("the load goes on");

        assert_eq!(project.uv_lock(), lock_as_it_was);
        assert!(history.recorded.lock().is_empty());
    }

    #[test]
    fn a_project_whose_first_party_packages_are_current_runs_no_uv() {
        let project = ProjectLockingWithAStubUv::holding(
            &a_uv_lock_of(&[a_locked_package("tatolab-stream", "0.68.29", FROM_AN_INDEX)]),
            WRITES_THE_MOVED_LOCK,
        );

        project.move_with(None).expect("nothing to move");

        assert_eq!(project.uv_runs(), "");
    }

    #[test]
    fn an_interrupted_move_is_refused_with_the_uv_lock_as_it_was() {
        let project =
            a_project_locking_tatolab_stream_at_0_68_27("echo 'half written' > uv.lock\nsleep 30");
        let lock_as_it_was = project.uv_lock();

        let refused = move_the_projects_first_party_packages_to_the_runtime_version(
            &project.project_directory,
            &project.stub_uv,
            RUNTIME_VERSION,
            None,
            &|| project.uv_runs.exists(),
        );

        assert!(matches!(refused, Err(ProjectVenvNotMade::Refused(_))));
        assert_eq!(project.uv_lock(), lock_as_it_was);
    }
}
