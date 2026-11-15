// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The packages the runtime unit carries declared in the project, the carried Python and wheels a sync uses, and `uv lock`.

use super::*;

fn first_party_modules_only(
    first_party_module_wheels: &[PathBuf],
) -> PackagesTheRuntimeUnitCarries {
    PackagesTheRuntimeUnitCarries {
        first_party_module_wheels: first_party_module_wheels.to_vec(),
        ..PackagesTheRuntimeUnitCarries::default()
    }
}

/// A first-party module's wheel beside the project, of `distribution_name`.
fn a_first_party_module_beside(project: &ProjectAndAStubUv, distribution_name: &str) -> PathBuf {
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
    let compositor = a_first_party_module_beside(&project, "tatolab-compositor");

    project
        .make_the_venv_with_the_first_party_modules(std::slice::from_ref(&compositor))
        .expect("the venv is made");

    assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
    assert_eq!(
        std::fs::read_link(
            project
                .project_directory
                .join(".tatolab/first-party-imports/tatolab_compositor-0.68.51-py3-none-any.whl")
        )
        .unwrap(),
        compositor
    );
    let pyproject = the_projects_pyproject(&project);
    assert_eq!(
        pyproject["project"]["dependencies"],
        toml::Value::try_from(["tatolab-compositor"]).unwrap()
    );
    assert_eq!(
        pyproject["tool"]["uv"]["sources"]["tatolab-compositor"]["path"].as_str(),
        Some(".tatolab/first-party-imports/tatolab_compositor-0.68.51-py3-none-any.whl"),
        "a path source through the project's own link keeps uv.lock free of the runtime's path"
    );
}

#[test]
fn a_link_to_another_runtimes_module_is_relinked_and_a_module_the_runtime_dropped_is_taken_out() {
    let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
    let compositor = a_first_party_module_beside(&project, "tatolab-compositor");
    let dropped = a_first_party_module_beside(&project, "tatolab-dropped");
    project
        .make_the_venv_with_the_first_party_modules(&[dropped.clone(), compositor.clone()])
        .expect("the venv is made");
    let compositor_link = project
        .project_directory
        .join(".tatolab/first-party-imports/tatolab_compositor-0.68.51-py3-none-any.whl");
    std::fs::remove_file(&compositor_link).unwrap();
    std::os::unix::fs::symlink(
        "/an/older/runtime/imports/tatolab_compositor-0.68.51-py3-none-any.whl",
        &compositor_link,
    )
    .unwrap();

    project
        .make_the_venv_with_the_first_party_modules(std::slice::from_ref(&compositor))
        .expect("the venv is synced again");

    assert_eq!(std::fs::read_link(&compositor_link).unwrap(), compositor);
    assert!(
        !project
            .project_directory
            .join(".tatolab/first-party-imports/tatolab_dropped-0.68.51-py3-none-any.whl")
            .exists()
    );
    let pyproject = the_projects_pyproject(&project);
    assert_eq!(
        pyproject["project"]["dependencies"],
        toml::Value::try_from(["tatolab-compositor"]).unwrap()
    );
    assert_eq!(
        pyproject["tool"]["uv"]["sources"]
            .as_table()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["tatolab-compositor"]
    );
    assert_eq!(project.uv_runs().len(), 2, "{:?}", project.uv_runs());
}

/// Give the project an import at `imports_path` whose `pyproject.toml` names each wheel file
/// of `named_wheels` as a source through its own first-party module links.
fn an_import_naming_first_party_modules(
    project: &ProjectAndAStubUv,
    imports_path: &str,
    named_wheels: &[(&str, &str)],
) -> (PathBuf, String) {
    let import_folder = project.project_directory.join(imports_path);
    std::fs::create_dir_all(&import_folder).unwrap();
    let mut import_pyproject = String::from("[project]\nname = \"lib\"\n\n[tool.uv.sources]\n");
    for (distribution_name, wheel_file_name) in named_wheels {
        import_pyproject.push_str(&format!(
            "{distribution_name} = {{ path = \".tatolab/first-party-imports/{wheel_file_name}\" }}\n"
        ));
    }
    std::fs::write(import_folder.join("pyproject.toml"), &import_pyproject).unwrap();
    (import_folder, import_pyproject)
}

#[test]
fn each_first_party_module_an_import_names_is_linked_in_the_import_and_its_files_are_untouched() {
    let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
    let stream = a_first_party_module_beside(&project, "tatolab-stream");
    let (import_folder, import_pyproject) = an_import_naming_first_party_modules(
        &project,
        "imports/lib",
        &[
            ("tatolab-stream", "tatolab_stream-0.68.50-py3-none-any.whl"),
            (
                "tatolab-not-carried",
                "tatolab_not_carried-0.68.50-py3-none-any.whl",
            ),
        ],
    );
    let (nested_import_folder, _) = an_import_naming_first_party_modules(
        &project,
        "imports/lib/imports/base",
        &[("tatolab_stream", "tatolab_stream-0.68.51-py3-none-any.whl")],
    );
    let stale_link = import_folder.join(".tatolab/first-party-imports/tatolab_gone.whl");
    std::fs::create_dir_all(stale_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("/an/older/runtime/tatolab_gone.whl", &stale_link).unwrap();

    project
        .make_the_venv_with_the_first_party_modules(std::slice::from_ref(&stream))
        .expect("the venv is made");

    assert_eq!(
        std::fs::read_link(
            import_folder
                .join(".tatolab/first-party-imports/tatolab_stream-0.68.50-py3-none-any.whl")
        )
        .unwrap(),
        stream,
        "uv finds the wheel at the path the import names, relative to the import"
    );
    assert_eq!(
        std::fs::read_link(
            nested_import_folder
                .join(".tatolab/first-party-imports/tatolab_stream-0.68.51-py3-none-any.whl")
        )
        .unwrap(),
        stream
    );
    assert!(
        std::fs::symlink_metadata(
            import_folder
                .join(".tatolab/first-party-imports/tatolab_not_carried-0.68.50-py3-none-any.whl")
        )
        .is_err()
    );
    assert!(std::fs::symlink_metadata(&stale_link).is_err());
    assert_eq!(
        std::fs::read_to_string(import_folder.join("pyproject.toml")).unwrap(),
        import_pyproject
    );
}

#[test]
fn a_lock_links_each_first_party_module_an_import_names_before_uv_locks() {
    let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
    let stream = a_first_party_module_beside(&project, "tatolab-stream");
    let (import_folder, _) = an_import_naming_first_party_modules(
        &project,
        "imports/lib",
        &[("tatolab-stream", "tatolab_stream-0.68.51-py3-none-any.whl")],
    );

    lock_the_projects_python_requirements_with_the_runtime_unit_uv(
        &project.project_directory,
        &project.stub_uv,
        &first_party_modules_only(std::slice::from_ref(&stream)),
    )
    .unwrap();

    assert_eq!(
        std::fs::read_link(
            import_folder
                .join(".tatolab/first-party-imports/tatolab_stream-0.68.51-py3-none-any.whl")
        )
        .unwrap(),
        stream
    );
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
fn the_core_nodes_project_is_linked_as_an_import_and_declared_as_a_uv_path_source() {
    let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
    let project_file = TatolabProjectFile::of_a_new_project("show");
    std::fs::write(
        project.project_directory.join("tatolab-project.json"),
        project_file.canonical_bytes().unwrap(),
    )
    .unwrap();
    let core_nodes_project_directory = project
        .project_directory
        .parent()
        .unwrap()
        .join("core-projects/tatolab-core-nodes");
    std::fs::create_dir_all(&core_nodes_project_directory).unwrap();
    std::fs::write(
        core_nodes_project_directory.join("tatolab-project.json"),
        TatolabProjectFile::of_a_new_project("Core Nodes")
            .canonical_bytes()
            .unwrap(),
    )
    .unwrap();
    std::fs::write(
        core_nodes_project_directory.join("pyproject.toml"),
        "[project]\nname = \"tatolab-core-nodes\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    for _each_load in 0..2 {
        declare_the_core_nodes_project_in_the_project(
            &project.project_directory,
            Some(&core_nodes_project_directory),
        )
        .unwrap();
    }

    assert_eq!(
        std::fs::read_link(project.project_directory.join("imports/core-nodes")).unwrap(),
        core_nodes_project_directory
    );
    let pyproject: toml::Value = toml::from_str(
        &std::fs::read_to_string(project.project_directory.join("pyproject.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        pyproject["project"]["dependencies"].as_array().unwrap(),
        &[toml::Value::from("tatolab-core-nodes")]
    );
    assert_eq!(
        pyproject["tool"]["uv"]["sources"]["tatolab-core-nodes"],
        toml::from_str::<toml::Value>("path = \"imports/core-nodes\"\npackage = false").unwrap()
    );
    assert_eq!(
        TatolabProjectFile::read_from_project_directory(&project.project_directory).unwrap(),
        project_file,
        "the project file never records the core project"
    );
}

#[test]
fn a_running_venv_is_synced_again_once_a_first_party_module_changes() {
    let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
    project.given_a_venv_interpreter_running("exit 0");
    project.given_the_venv_synced_with_what_the_project_holds_now();
    let compositor = a_first_party_module_beside(&project, "tatolab-compositor");

    project
        .make_the_venv_with_the_first_party_modules(&[compositor])
        .expect("the venv is synced again");

    assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
}

#[test]
fn a_running_venv_a_command_outside_the_runtime_took_a_first_party_module_out_of_is_synced_again() {
    let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
    project.given_a_venv_interpreter_running("exit 0");
    let compositor = a_first_party_module_beside(&project, "tatolab-compositor");
    let site_packages = project.venv().join("lib/python3.12/site-packages");
    std::fs::create_dir_all(site_packages.join("tatolab_compositor-0.68.41.dist-info")).unwrap();
    declare_each_first_party_module_in_the_project(
        &project.project_directory,
        std::slice::from_ref(&compositor),
    )
    .unwrap();
    std::fs::write(
        project.venv().join(PROJECT_VENV_SYNC_STAMP_FILE_NAME),
        the_venv_sync_stamp_of(
            &project.project_directory,
            &first_party_modules_only(std::slice::from_ref(&compositor)),
        )
        .unwrap(),
    )
    .unwrap();

    project
        .make_the_venv_with_the_first_party_modules(std::slice::from_ref(&compositor))
        .expect("the venv is current");
    assert!(project.uv_runs().is_empty(), "{:?}", project.uv_runs());

    std::fs::remove_dir(site_packages.join("tatolab_compositor-0.68.41.dist-info")).unwrap();
    project
        .make_the_venv_with_the_first_party_modules(&[compositor])
        .expect("the venv is synced again");
    assert_eq!(project.uv_runs(), project.one_sync_in_the_project());
}

#[test]
fn the_first_party_modules_are_each_wheel_in_name_order() {
    let modules = tempfile::tempdir().unwrap();
    for file_name in [
        "tatolab_compositor-0.68.51-py3-none-any.whl",
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
                .join("tatolab_compositor-0.68.51-py3-none-any.whl"),
            modules
                .path()
                .join("tatolab_h264-0.68.51-cp312-abi3-manylinux_2_28_x86_64.whl")
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
    let compositor = a_first_party_module_beside(&project, "tatolab-compositor");
    let stamp_before = the_venv_sync_stamp_of(
        &project.project_directory,
        &first_party_modules_only(std::slice::from_ref(&compositor)),
    );
    std::fs::write(&compositor, "a rebuilt wheel of another size").unwrap();
    assert_ne!(
        the_venv_sync_stamp_of(
            &project.project_directory,
            &first_party_modules_only(std::slice::from_ref(&compositor))
        ),
        stamp_before
    );
}

/// A runtime unit's carried wheels folder and Python mirror beside the project.
fn carried_packages_beside(project: &ProjectAndAStubUv) -> PackagesTheRuntimeUnitCarries {
    let unit_lib = project.project_directory.parent().unwrap().join("unit-lib");
    let carried_wheels_directory = unit_lib.join("carried-wheels");
    let carried_python_downloads_directory = unit_lib.join("python-downloads");
    std::fs::create_dir_all(&carried_wheels_directory).unwrap();
    std::fs::write(
        carried_wheels_directory.join("numpy-2.5.4-cp312-cp312-manylinux_2_28_x86_64.whl"),
        "a wheel",
    )
    .unwrap();
    std::fs::create_dir_all(carried_python_downloads_directory.join("20261013")).unwrap();
    std::fs::write(
        carried_python_downloads_metadata_file_in(&carried_python_downloads_directory),
        "{}",
    )
    .unwrap();
    PackagesTheRuntimeUnitCarries {
        first_party_module_wheels: Vec::new(),
        carried_wheels_directory: Some(carried_wheels_directory),
        carried_python_downloads_directory: Some(carried_python_downloads_directory),
        core_nodes_project_directory: None,
    }
}

/// A stub uv body that finds no Python, installs one, and runs `sync_body` for a sync.
fn a_uv_finding_no_python_and_syncing_with(sync_body: &str) -> String {
    format!(
        "case \"$1 $2\" in\n\
         'python find') exit 2 ;;\n\
         'python install') echo \"mirror=$UV_PYTHON_INSTALL_MIRROR downloads=$UV_PYTHON_DOWNLOADS_JSON_URL\" >> \"$(dirname \"$0\")/uv-environment\"; exit 0 ;;\n\
         esac\n{sync_body}"
    )
}

#[test]
fn a_unit_carrying_wheels_and_python_installs_its_python_then_syncs_offline_from_its_link() {
    let project = ProjectAndAStubUv::whose_uv_runs(&a_uv_finding_no_python_and_syncing_with(
        MAKES_A_VENV_WHOSE_INTERPRETER_RUNS,
    ));
    let carried = carried_packages_beside(&project);

    make_the_projects_venv_current_with_the_runtime_unit_uv(
        &project.project_directory,
        Some(&project.stub_uv),
        &carried,
        &|| false,
        None,
    )
    .expect("the venv is made");

    let project_resolved = project.the_project_directory_resolved();
    assert_eq!(
        project.uv_runs(),
        [
            format!("python find in {project_resolved}"),
            format!("python install --no-bin in {project_resolved}"),
            format!("sync --offline in {project_resolved}"),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(project.stub_uv.parent().unwrap().join("uv-environment"))
            .unwrap()
            .trim(),
        format!(
            "mirror=file://{} downloads={}",
            carried
                .carried_python_downloads_directory
                .as_ref()
                .unwrap()
                .display(),
            carried_python_downloads_metadata_file_in(
                carried.carried_python_downloads_directory.as_ref().unwrap()
            )
            .display()
        ),
        "uv reads the carried Python's sha256 from the unit's metadata, not its own list"
    );
    assert_eq!(
        std::fs::read_link(project.project_directory.join(".tatolab/carried-wheels")).unwrap(),
        *carried.carried_wheels_directory.as_ref().unwrap()
    );
    assert_eq!(
        the_projects_pyproject(&project)["tool"]["uv"]["find-links"],
        toml::Value::try_from([".tatolab/carried-wheels"]).unwrap(),
        "a find-links source through the project's own link keeps uv.lock free of the unit's path"
    );
}

#[test]
fn a_python_the_machine_has_is_used_and_none_is_installed() {
    let project = ProjectAndAStubUv::whose_uv_runs(&format!(
        "[ \"$1 $2\" = 'python find' ] && exit 0\n{MAKES_A_VENV_WHOSE_INTERPRETER_RUNS}"
    ));
    let carried = carried_packages_beside(&project);

    make_the_projects_venv_current_with_the_runtime_unit_uv(
        &project.project_directory,
        Some(&project.stub_uv),
        &carried,
        &|| false,
        None,
    )
    .expect("the venv is made");

    let project_resolved = project.the_project_directory_resolved();
    assert_eq!(
        project.uv_runs(),
        [
            format!("python find in {project_resolved}"),
            format!("sync --offline in {project_resolved}"),
        ]
    );
}

#[test]
fn a_failed_offline_sync_is_followed_by_one_that_reaches_the_projects_indexes() {
    let project = ProjectAndAStubUv::whose_uv_runs(&a_uv_finding_no_python_and_syncing_with(
        &format!(
            "[ \"$2\" = '--offline' ] && {{ echo 'robot-arm was not found in the cache' >&2; exit 1; }}\n\
             {MAKES_A_VENV_WHOSE_INTERPRETER_RUNS}"
        ),
    ));
    let carried = carried_packages_beside(&project);

    make_the_projects_venv_current_with_the_runtime_unit_uv(
        &project.project_directory,
        Some(&project.stub_uv),
        &carried,
        &|| false,
        None,
    )
    .expect("the venv is made from the indexes");

    let project_resolved = project.the_project_directory_resolved();
    assert_eq!(
        project.uv_runs()[2..],
        [
            format!("sync --offline in {project_resolved}"),
            format!("sync in {project_resolved}"),
        ]
    );
    assert!(project.interpreter().is_file());
}

#[test]
fn a_sync_that_fails_offline_and_from_the_indexes_is_refused_in_the_second_ones_words() {
    let project = ProjectAndAStubUv::whose_uv_runs(&a_uv_finding_no_python_and_syncing_with(
        "[ \"$2\" = '--offline' ] && { echo 'not in the cache' >&2; exit 1; }\n\
         echo 'robot-arm was not found in the package registry' >&2\nexit 1",
    ));
    let carried = carried_packages_beside(&project);

    let failed_because =
        the_reason_it_failed(make_the_projects_venv_current_with_the_runtime_unit_uv(
            &project.project_directory,
            Some(&project.stub_uv),
            &carried,
            &|| false,
            None,
        ));

    assert!(
        failed_because.contains("`uv sync` failed"),
        "{failed_because}"
    );
    assert!(
        failed_because.contains("robot-arm was not found in the package registry"),
        "{failed_because}"
    );
    assert!(
        !failed_because.contains("not in the cache"),
        "{failed_because}"
    );
    assert!(!project.venv().exists(), "the half-made venv is removed");
}

#[test]
fn a_running_venv_is_synced_again_once_the_carried_wheels_change() {
    let project = ProjectAndAStubUv::whose_uv_runs(MAKES_A_VENV_WHOSE_INTERPRETER_RUNS);
    let carried = carried_packages_beside(&project);
    let stamp_before = the_venv_sync_stamp_of(&project.project_directory, &carried).unwrap();

    std::fs::write(
        carried
            .carried_wheels_directory
            .as_ref()
            .unwrap()
            .join("pillow-13.0.0-cp312-cp312-manylinux_2_28_x86_64.whl"),
        "a wheel",
    )
    .unwrap();

    assert_ne!(
        the_venv_sync_stamp_of(&project.project_directory, &carried).unwrap(),
        stamp_before
    );
}

#[test]
fn a_unit_that_stops_carrying_wheels_takes_the_link_and_the_find_links_source_out() {
    let project = ProjectAndAStubUv::whose_uv_runs(&a_uv_finding_no_python_and_syncing_with(
        MAKES_A_VENV_WHOSE_INTERPRETER_RUNS,
    ));
    let carried = carried_packages_beside(&project);
    make_the_projects_venv_current_with_the_runtime_unit_uv(
        &project.project_directory,
        Some(&project.stub_uv),
        &carried,
        &|| false,
        None,
    )
    .expect("the venv is made");

    project
        .make_the_venv()
        .expect("the venv is synced again with nothing carried");

    assert!(
        project
            .project_directory
            .join(".tatolab/carried-wheels")
            .symlink_metadata()
            .is_err()
    );
    assert!(
        the_projects_pyproject(&project)
            .get("tool")
            .and_then(|tool| tool.get("uv"))
            .and_then(|uv| uv.get("find-links"))
            .is_none()
    );
}

#[test]
fn a_lock_with_carried_wheels_runs_offline_first_then_from_the_indexes() {
    let project = ProjectAndAStubUv::whose_uv_runs(
        "[ \"$2\" = '--offline' ] && { echo 'not in the cache' >&2; exit 1; }\nexit 0",
    );
    let carried = carried_packages_beside(&project);

    lock_the_projects_python_requirements_with_the_runtime_unit_uv(
        &project.project_directory,
        &project.stub_uv,
        &carried,
    )
    .unwrap();

    let project_resolved = project.the_project_directory_resolved();
    assert_eq!(
        project.uv_runs(),
        [
            format!("lock --offline in {project_resolved}"),
            format!("lock in {project_resolved}"),
        ]
    );
}

#[test]
fn a_lock_runs_uv_lock_in_the_project() {
    let project = ProjectAndAStubUv::whose_uv_runs("exit 0");
    lock_the_projects_python_requirements_with_the_runtime_unit_uv(
        &project.project_directory,
        &project.stub_uv,
        &PackagesTheRuntimeUnitCarries::default(),
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
    let compositor = a_first_party_module_beside(&project, "tatolab-compositor");

    lock_the_projects_python_requirements_with_the_runtime_unit_uv(
        &project.project_directory,
        &project.stub_uv,
        &first_party_modules_only(&[compositor]),
    )
    .unwrap();

    assert_eq!(
        the_projects_pyproject(&project)["tool"]["uv"]["sources"]["tatolab-compositor"]["path"]
            .as_str(),
        Some(".tatolab/first-party-imports/tatolab_compositor-0.68.51-py3-none-any.whl")
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
        &PackagesTheRuntimeUnitCarries::default(),
    )
    .unwrap_err();
    assert!(refusal.contains("`uv lock` failed"), "{refusal}");
    assert!(
        refusal.contains("No solution found when resolving dependencies"),
        "{refusal}"
    );
}
