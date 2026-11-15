// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A project's `.venv`: when it is made, removed, kept or synced again, and how a failed or interrupted sync ends.

use super::*;

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
            &PackagesTheRuntimeUnitCarries::default(),
            &|| false,
            None,
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
            &PackagesTheRuntimeUnitCarries::default(),
            &|| false,
            None,
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
        &PackagesTheRuntimeUnitCarries::default(),
        &|| std::time::Instant::now() >= interrupted_after,
        None,
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
        &PackagesTheRuntimeUnitCarries::default(),
        &|| std::time::Instant::now() >= interrupted_after,
        None,
    ));

    assert!(refusal.contains("was not made: "), "{refusal}");
    assert!(
        project.interpreter().is_file(),
        "an interrupted probe removed the venv"
    );
}

#[test]
fn the_sync_is_started_without_the_variables_that_point_uv_elsewhere() {
    let [(step, command)]: [(String, Command); 1] = the_uv_runs_tried_in_turn(
        Path::new("/opt/tatolab/bin/uv"),
        Path::new("/home/someone/app"),
        "sync",
        &PackagesTheRuntimeUnitCarries::default(),
    )
    .try_into()
    .expect("one run when the unit carries no wheels");
    assert_eq!(step, "uv sync");
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
            .all(|(name, value)| value.is_none() || !name.to_string_lossy().starts_with("PYTHON")),
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
fn a_venv_whose_pyproject_changed_while_uv_synced_it_is_left_unstamped_and_synced_again() {
    let project = ProjectAndAStubUv::whose_uv_runs(
        "printf '[project]\\nname = \"p\"\\ndependencies = [\"pyserial\"]\\n' > pyproject.toml",
    );
    project.given_a_venv_interpreter_running("exit 0");
    let stamp = project.venv().join(PROJECT_VENV_SYNC_STAMP_FILE_NAME);
    std::fs::write(&stamp, "stale").unwrap();

    project.make_the_venv().expect("the venv is synced");
    assert!(
        !stamp.exists(),
        "the venv was stamped with what uv never read"
    );

    project.make_the_venv().expect("the venv is synced again");
    assert_eq!(project.uv_runs().len(), 2, "{:?}", project.uv_runs());
    assert!(stamp.is_file());
}

#[test]
fn a_venv_whose_uv_lock_uv_wrote_as_it_synced_is_stamped_and_runs_no_uv_on_its_next_load() {
    let project = ProjectAndAStubUv::whose_uv_runs("echo 'version = 1' > uv.lock");
    project.given_a_venv_interpreter_running("exit 0");

    project.make_the_venv().expect("the venv is synced");
    project.make_the_venv().expect("the venv is current");

    assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
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
        std::fs::read_to_string(project.venv().join(PROJECT_VENV_SYNC_STAMP_FILE_NAME)).unwrap(),
        "stale"
    );
}
