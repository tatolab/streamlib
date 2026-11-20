// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::io::Write;
use zip::write::SimpleFileOptions;

const SYNTHETIC_DIST_INFO_WHEEL_MEMBER: &str = "streamlib-0.0.0.dist-info/WHEEL";

fn write_synthetic_wheel(wheel_path: &Path, members: &[(&str, u32)]) {
    let mut wheel_writer = zip::ZipWriter::new(std::fs::File::create(wheel_path).unwrap());
    for (member_name, unix_mode) in members {
        wheel_writer
            .start_file(
                *member_name,
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored)
                    .unix_permissions(*unix_mode),
            )
            .unwrap();
        wheel_writer.write_all(member_name.as_bytes()).unwrap();
    }
    wheel_writer.finish().unwrap();
}

fn runtime_unit_members() -> Vec<(&'static str, u32)> {
    vec![
        ("tatolab/runtime/__init__.py", 0o644),
        ("tatolab/runtime/_engine.abi3.so", 0o755),
        (SYNTHETIC_DIST_INFO_WHEEL_MEMBER, 0o644),
    ]
}

fn lend_refusal_for_members(members: &[(&str, u32)]) -> (String, PathBuf) {
    let scratch = tempfile::TempDir::new().unwrap();
    let wheel_path = scratch
        .path()
        .join("streamlib-0.0.0-cp310-abi3-linux_x86_64.whl");
    write_synthetic_wheel(&wheel_path, members);
    let lend_directory = lend_directory_in_the_runtime_unit(scratch.path());

    let refusal = replace_lend_directory_with_wheel_contents(&wheel_path, &lend_directory)
        .unwrap_err()
        .to_string();

    assert!(
        !lend_directory.exists(),
        "a refused lend is left behind at {}",
        lend_directory.display()
    );
    (refusal, lend_directory)
}

#[test]
fn the_lend_is_replaced_with_the_wheels_runtime_package_and_dist_info() {
    let scratch = tempfile::TempDir::new().unwrap();
    let wheel_path = scratch
        .path()
        .join("streamlib-0.0.0-cp310-abi3-linux_x86_64.whl");
    write_synthetic_wheel(&wheel_path, &runtime_unit_members());
    let lend_directory = lend_directory_in_the_runtime_unit(scratch.path());
    std::fs::create_dir_all(lend_directory.join("tatolab/runtime")).unwrap();
    std::fs::write(lend_directory.join("tatolab/runtime/stale_module.py"), "").unwrap();

    replace_lend_directory_with_wheel_contents(&wheel_path, &lend_directory).unwrap();

    assert!(
        !lend_directory
            .join("tatolab/runtime/stale_module.py")
            .exists()
    );
    assert_eq!(
        std::fs::read_to_string(lend_directory.join("tatolab/runtime/__init__.py")).unwrap(),
        "tatolab/runtime/__init__.py"
    );
    assert!(
        lend_directory
            .join(SYNTHETIC_DIST_INFO_WHEEL_MEMBER)
            .is_file()
    );
    assert!(!lend_directory.join("tatolab/__init__.py").exists());
}

#[cfg(unix)]
#[test]
fn an_unpacked_native_extension_keeps_its_unix_mode() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::TempDir::new().unwrap();
    let wheel_path = scratch
        .path()
        .join("streamlib-0.0.0-cp310-abi3-linux_x86_64.whl");
    write_synthetic_wheel(&wheel_path, &runtime_unit_members());
    let lend_directory = scratch.path().join("lend");

    replace_lend_directory_with_wheel_contents(&wheel_path, &lend_directory).unwrap();

    let unpacked_mode = std::fs::metadata(lend_directory.join("tatolab/runtime/_engine.abi3.so"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(unpacked_mode & 0o777, 0o755);
}

#[test]
fn a_wheel_carrying_tatolab_init_is_refused_by_the_namespace_gate_naming_the_lend_path() {
    let mut members = runtime_unit_members();
    members.push(("tatolab/__init__.py", 0o644));

    let (refusal, lend_directory) = lend_refusal_for_members(&members);

    assert!(
        refusal.contains(
            &lend_directory
                .join("tatolab/__init__.py")
                .display()
                .to_string()
        ),
        "{refusal}"
    );
    assert!(refusal.contains("PEP 420"), "{refusal}");
}

#[test]
fn a_wheel_carrying_the_stream_package_is_refused() {
    let mut members = runtime_unit_members();
    members.push(("tatolab/stream/__init__.py", 0o644));

    let (refusal, _) = lend_refusal_for_members(&members);

    assert!(
        refusal.contains("`tatolab/stream/__init__.py`"),
        "{refusal}"
    );
}

#[test]
fn a_wheel_member_that_would_unpack_outside_the_lend_is_refused() {
    let mut members = runtime_unit_members();
    members.push(("../escaped.py", 0o644));

    let (refusal, _) = lend_refusal_for_members(&members);

    assert!(refusal.contains("`../escaped.py`"), "{refusal}");
    assert!(refusal.contains("outside the lend"), "{refusal}");
}

#[test]
fn a_wheel_without_the_runtime_package_init_is_refused() {
    let (refusal, _) = lend_refusal_for_members(&[
        ("tatolab/runtime/_engine.abi3.so", 0o755),
        (SYNTHETIC_DIST_INFO_WHEEL_MEMBER, 0o644),
    ]);

    assert!(refusal.contains("tatolab/runtime/__init__.py"), "{refusal}");
}

#[test]
fn a_wheel_with_no_dist_info_is_refused() {
    let (refusal, _) = lend_refusal_for_members(&[("tatolab/runtime/__init__.py", 0o644)]);

    assert!(refusal.contains("0 .dist-info directories"), "{refusal}");
}

#[test]
fn exactly_one_wheel_is_taken_from_the_wheel_directory() {
    let wheel_directory = tempfile::TempDir::new().unwrap();
    let no_wheel = the_one_wheel_in(wheel_directory.path())
        .unwrap_err()
        .to_string();
    assert!(no_wheel.contains("found 0"), "{no_wheel}");

    let first_wheel = wheel_directory
        .path()
        .join("streamlib-0.0.0-cp310-abi3-linux_x86_64.whl");
    std::fs::write(&first_wheel, "").unwrap();
    std::fs::write(wheel_directory.path().join("build.log"), "").unwrap();
    assert_eq!(
        the_one_wheel_in(wheel_directory.path()).unwrap(),
        first_wheel
    );

    std::fs::write(
        wheel_directory
            .path()
            .join("streamlib-0.0.1-cp310-abi3-linux_x86_64.whl"),
        "",
    )
    .unwrap();
    let two_wheels = the_one_wheel_in(wheel_directory.path())
        .unwrap_err()
        .to_string();
    assert!(two_wheels.contains("found 2"), "{two_wheels}");
}

fn a_compiler_artifact_message(target_name: &str, executable: Option<&str>) -> String {
    serde_json::json!({
        "reason": "compiler-artifact",
        "target": {"name": target_name, "kind": ["bin"]},
        "executable": executable,
    })
    .to_string()
}

#[test]
fn the_runtime_unit_binaries_are_read_from_cargos_build_messages() {
    let cargo_build_messages = [
        a_compiler_artifact_message("streamlib_engine", None),
        r#"{"reason":"build-script-executed","package_id":"x"}"#.to_owned(),
        a_compiler_artifact_message("tatolabd", Some("/target/debug/tatolabd")),
        a_compiler_artifact_message("tatolab", Some("/target/debug/tatolab")),
        a_compiler_artifact_message(
            "tatolab-video-poster-frame",
            Some("/target/debug/tatolab-video-poster-frame"),
        ),
        a_compiler_artifact_message(
            "tatolab-video-encode-host",
            Some("/target/debug/tatolab-video-encode-host"),
        ),
        a_compiler_artifact_message("generate_openapi", Some("/target/debug/generate_openapi")),
        r#"{"reason":"build-finished","success":true}"#.to_owned(),
    ]
    .join("\n");

    let built_binaries =
        runtime_unit_binaries_from_cargo_build_messages(&cargo_build_messages).unwrap();

    assert_eq!(
        built_binaries,
        vec![
            BuiltRuntimeUnitBinary {
                binary_target_name: "tatolabd".to_owned(),
                built_executable: PathBuf::from("/target/debug/tatolabd"),
            },
            BuiltRuntimeUnitBinary {
                binary_target_name: "tatolab".to_owned(),
                built_executable: PathBuf::from("/target/debug/tatolab"),
            },
            BuiltRuntimeUnitBinary {
                binary_target_name: "tatolab-video-poster-frame".to_owned(),
                built_executable: PathBuf::from("/target/debug/tatolab-video-poster-frame"),
            },
            BuiltRuntimeUnitBinary {
                binary_target_name: "tatolab-video-encode-host".to_owned(),
                built_executable: PathBuf::from("/target/debug/tatolab-video-encode-host"),
            },
        ]
    );
}

#[test]
fn a_build_naming_no_executable_for_a_runtime_unit_binary_is_refused_naming_it() {
    let refusal = runtime_unit_binaries_from_cargo_build_messages(&a_compiler_artifact_message(
        "tatolabd",
        Some("/target/debug/tatolabd"),
    ))
    .unwrap_err()
    .to_string();

    assert!(
        refusal.contains("`tatolab` binary of `tatolab-cli`"),
        "{refusal}"
    );
}

#[cfg(unix)]
#[test]
fn the_runtime_unit_binaries_replace_what_bin_holds_and_stay_executable() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::TempDir::new().unwrap();
    let built_tatolabd = scratch.path().join("built-tatolabd");
    std::fs::write(&built_tatolabd, "new tatolabd").unwrap();
    std::fs::set_permissions(&built_tatolabd, std::fs::Permissions::from_mode(0o755)).unwrap();
    let bin_directory = runtime_unit_binary_directory_in_the_workspace(scratch.path());
    std::fs::create_dir_all(&bin_directory).unwrap();
    std::fs::write(bin_directory.join("tatolabd"), "old tatolabd").unwrap();

    replace_runtime_unit_binaries_in(
        &bin_directory,
        &[BuiltRuntimeUnitBinary {
            binary_target_name: "tatolabd".to_owned(),
            built_executable: built_tatolabd,
        }],
    )
    .unwrap();

    let placed_tatolabd = bin_directory.join("tatolabd");
    assert_eq!(
        std::fs::read_to_string(&placed_tatolabd).unwrap(),
        "new tatolabd"
    );
    assert_eq!(
        std::fs::metadata(&placed_tatolabd)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert!(
        !std::fs::symlink_metadata(&placed_tatolabd)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn the_typescript_compiler_replaces_its_directory_in_bin_with_its_library_files_beside_it() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::TempDir::new().unwrap();
    let compiler_directory = scratch.path().join("staged");
    std::fs::create_dir_all(&compiler_directory).unwrap();
    std::fs::write(compiler_directory.join("tsc"), "new tsc").unwrap();
    std::fs::set_permissions(
        compiler_directory.join("tsc"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(compiler_directory.join("lib.d.ts"), "/// lib").unwrap();
    let bin_directory = runtime_unit_binary_directory_in_the_workspace(scratch.path());
    let placed_compiler =
        bin_directory.join(RUNTIME_UNIT_TYPESCRIPT_COMPILER_RELATIVE_TO_THE_BINARY_DIRECTORY);
    let placed_directory = placed_compiler.parent().unwrap();
    std::fs::create_dir_all(placed_directory).unwrap();
    std::fs::write(placed_directory.join("lib.removed.d.ts"), "/// old").unwrap();

    replace_the_typescript_compiler_in(&bin_directory, &compiler_directory).unwrap();

    assert_eq!(
        std::fs::read_to_string(&placed_compiler).unwrap(),
        "new tsc"
    );
    assert_eq!(
        std::fs::metadata(&placed_compiler)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert!(placed_directory.join("lib.d.ts").is_file());
    assert!(!placed_directory.join("lib.removed.d.ts").exists());
}

#[test]
fn a_staged_directory_without_the_typescript_compiler_is_refused() {
    let scratch = tempfile::TempDir::new().unwrap();
    let compiler_directory = scratch.path().join("staged");
    std::fs::create_dir_all(&compiler_directory).unwrap();
    std::fs::write(compiler_directory.join("lib.d.ts"), "/// lib").unwrap();

    let refusal = replace_the_typescript_compiler_in(
        &runtime_unit_binary_directory_in_the_workspace(scratch.path()),
        &compiler_directory,
    )
    .unwrap_err()
    .to_string();

    assert!(refusal.contains("holds no tsc"), "{refusal}");
}

#[test]
fn a_unit_for_the_machine_builds_every_binary_package_without_the_test_root_feature() {
    assert_eq!(
        runtime_unit_binaries_cargo_build_selection(
            RuntimeUnitBuildProfile::Release,
            RuntimeUnitMachineDirectories::TheMachines,
        ),
        [
            "-p",
            "tatolabd",
            "-p",
            "tatolab-cli",
            "-p",
            "tatolab-video-poster-frame",
            "-p",
            "tatolab-video-encode-host",
            "--release"
        ]
    );
}

#[test]
fn a_unit_under_a_test_root_builds_every_binary_package_with_the_feature() {
    assert_eq!(
        runtime_unit_binaries_cargo_build_selection(
            RuntimeUnitBuildProfile::Debug,
            RuntimeUnitMachineDirectories::UnderATestRoot,
        ),
        [
            "-p",
            "tatolabd",
            "-p",
            "tatolab-cli",
            "-p",
            "tatolab-video-poster-frame",
            "-p",
            "tatolab-video-encode-host",
            "--features",
            "tatolabd/machine-directories-under-a-test-root,\
                 tatolab-cli/machine-directories-under-a-test-root,\
                 tatolab-video-poster-frame/machine-directories-under-a-test-root,\
                 tatolab-video-encode-host/machine-directories-under-a-test-root",
        ]
    );
}

#[test]
fn every_runtime_unit_binary_package_declares_the_test_root_feature() {
    for (package, _) in RUNTIME_UNIT_BINARY_PACKAGES_AND_TARGETS {
        let package_manifest_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../runtime")
            .join(package)
            .join("Cargo.toml");
        let package_manifest: toml::Value =
            toml::from_str(&std::fs::read_to_string(&package_manifest_path).unwrap()).unwrap();
        assert!(
            package_manifest["features"]
                .get(MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_FEATURE)
                .is_some(),
            "{} declares no `{MACHINE_DIRECTORIES_UNDER_A_TEST_ROOT_FEATURE}` feature",
            package_manifest_path.display()
        );
    }
}

#[test]
fn the_marker_is_written_at_the_unit_root_and_removed_again() {
    let runtime_unit_root = tempfile::TempDir::new().unwrap();
    let marker = machine_directories_under_a_test_root_marker_in(runtime_unit_root.path());
    assert_eq!(
        marker,
        runtime_unit_root
            .path()
            .join("machine-directories-under-a-test-root")
    );

    remove_the_machine_directories_marker(&marker).unwrap();
    write_the_machine_directories_marker(&marker).unwrap();
    let marker_text = std::fs::read_to_string(&marker).unwrap();
    assert!(
        marker_text.contains("TATOLAB_TEST_MACHINE_ROOT"),
        "{marker_text}"
    );

    remove_the_machine_directories_marker(&marker).unwrap();
    assert!(!marker.exists());
}

#[test]
fn the_macos_deployment_target_is_read_from_the_maturin_target_table() {
    let maturin_pyproject = "[tool.maturin]\npython-source = \"python\"\n\n\
             [tool.maturin.target.aarch64-apple-darwin]\nmacos-deployment-target = \"26.0\"\n";

    assert_eq!(
        macos_deployment_target_from_maturin_pyproject(maturin_pyproject).unwrap(),
        "26.0"
    );
    assert!(macos_deployment_target_from_maturin_pyproject("[tool.maturin]\n").is_err());
}

#[test]
fn the_checked_in_maturin_project_pins_a_macos_deployment_target() {
    let maturin_pyproject = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(RUNTIME_UNIT_MATURIN_PROJECT_RELATIVE_TO_WORKSPACE)
            .join("pyproject.toml"),
    )
    .unwrap();

    assert_eq!(
        macos_deployment_target_from_maturin_pyproject(&maturin_pyproject).unwrap(),
        "26.0"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_lend_missing_any_bundled_vulkan_driver_file_is_refused_naming_it() {
    for missing_driver_file_name in runtime_unit_layout::BUNDLED_VULKAN_DRIVER_FILE_NAMES {
        let lend_directory = tempfile::TempDir::new().unwrap();
        let driver_directory =
            runtime_unit_layout::bundled_vulkan_driver_directory_in_the_lend(lend_directory.path());
        std::fs::create_dir_all(&driver_directory).unwrap();
        for carried_driver_file_name in runtime_unit_layout::BUNDLED_VULKAN_DRIVER_FILE_NAMES
            .iter()
            .filter(|driver_file_name| **driver_file_name != missing_driver_file_name)
        {
            std::fs::write(driver_directory.join(carried_driver_file_name), "").unwrap();
        }

        let refusal = ensure_lend_carries_the_bundled_vulkan_driver(lend_directory.path())
            .unwrap_err()
            .to_string();
        assert!(refusal.contains(missing_driver_file_name), "{refusal}");

        std::fs::write(driver_directory.join(missing_driver_file_name), "").unwrap();
        ensure_lend_carries_the_bundled_vulkan_driver(lend_directory.path()).unwrap();
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_lend_missing_either_slang_compiler_library_is_refused_naming_it() {
    for missing_compiler_file_name in runtime_unit_layout::BUNDLED_SLANG_COMPILER_FILE_NAMES {
        let lend_directory = tempfile::TempDir::new().unwrap();
        let compiler_directory = runtime_unit_layout::bundled_slang_compiler_directory_in_the_lend(
            lend_directory.path(),
        );
        std::fs::create_dir_all(&compiler_directory).unwrap();
        for carried_compiler_file_name in runtime_unit_layout::BUNDLED_SLANG_COMPILER_FILE_NAMES
            .iter()
            .filter(|compiler_file_name| **compiler_file_name != missing_compiler_file_name)
        {
            std::fs::write(compiler_directory.join(carried_compiler_file_name), "").unwrap();
        }

        let refusal = ensure_lend_carries_the_slang_compiler(lend_directory.path())
            .unwrap_err()
            .to_string();
        assert!(refusal.contains(missing_compiler_file_name), "{refusal}");

        std::fs::write(compiler_directory.join(missing_compiler_file_name), "").unwrap();
        ensure_lend_carries_the_slang_compiler(lend_directory.path()).unwrap();
    }
}

#[test]
fn the_staging_script_pins_the_slang_release_the_layout_names() {
    let staging_script = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(SLANG_COMPILER_STAGING_SCRIPT_RELATIVE_TO_WORKSPACE),
    )
    .unwrap();
    assert!(
        staging_script.contains(&format!(
            "SLANG_VERSION=\"{}\"",
            runtime_unit_layout::BUNDLED_SLANG_COMPILER_VERSION
        )),
        "the staging script pins a Slang release other than the one the runtime unit layout \
             names its optimiser library after"
    );
}
