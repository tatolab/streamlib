// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SCAFFOLD_TEMPLATE_LICENSE_HEADER: &str =
    "# Copyright (c) 2025 Jonathan Fontanez\n# SPDX-License-Identifier: BUSL-1.1\n\n";

fn scaffold_template_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/tatolab-stream/scaffold_template")
}

fn scaffold_template_text(template_file: &str) -> String {
    fs::read_to_string(scaffold_template_directory().join(template_file)).unwrap()
}

fn run_tatolab(working_directory: &Path, tatolab_arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tatolab"))
        .args(tatolab_arguments)
        .current_dir(working_directory)
        .output()
        .unwrap()
}

fn every_file_under(directory: &Path) -> BTreeSet<String> {
    let mut relative_file_paths = BTreeSet::new();
    let mut directories_to_visit = vec![directory.to_path_buf()];
    while let Some(visited_directory) = directories_to_visit.pop() {
        for directory_entry in fs::read_dir(&visited_directory).unwrap() {
            let entry_path = directory_entry.unwrap().path();
            if entry_path.is_dir() {
                directories_to_visit.push(entry_path);
            } else {
                relative_file_paths.insert(
                    entry_path
                        .strip_prefix(directory)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    relative_file_paths
}

fn expected_scaffolded_file_paths() -> BTreeSet<String> {
    [
        "stream.py",
        "nodes/__init__.py",
        "nodes/inverting_effect.py",
        "nodes/brightness_meter.py",
        "pyproject.toml",
        ".python-version",
        ".gitignore",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[test]
fn help_lists_exactly_the_served_verbs() {
    let scratch_directory = tempfile::tempdir().unwrap();
    let help_output = run_tatolab(scratch_directory.path(), &["--help"]);
    assert!(help_output.status.success());
    let help_text = String::from_utf8(help_output.stdout).unwrap();
    let listed_verbs: Vec<String> = help_text
        .lines()
        .skip_while(|help_line| !help_line.starts_with("Commands:"))
        .skip(1)
        .take_while(|help_line| !help_line.trim().is_empty())
        .map(|help_line| help_line.split_whitespace().next().unwrap().to_owned())
        .collect();
    assert_eq!(
        listed_verbs,
        [
            "new",
            "run",
            "dev",
            "nodes",
            "graph",
            "tap",
            "enable-virtual-camera"
        ],
        "help was:\n{help_text}"
    );
}

#[test]
fn new_writes_the_camera_stream_project_with_dotfiles_and_no_licence_header() {
    let scratch_directory = tempfile::tempdir().unwrap();
    let new_output = run_tatolab(scratch_directory.path(), &["new", "My Probe App"]);
    assert!(
        new_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&new_output.stderr)
    );
    let project_directory = scratch_directory.path().join("My Probe App");
    assert_eq!(
        every_file_under(&project_directory),
        expected_scaffolded_file_paths()
    );

    for scaffolded_file in expected_scaffolded_file_paths() {
        let scaffolded_text = fs::read_to_string(project_directory.join(&scaffolded_file)).unwrap();
        assert!(
            !scaffolded_text.contains("SPDX-License-Identifier"),
            "{scaffolded_file} kept the licence header"
        );
    }

    let stream_py = fs::read_to_string(project_directory.join("stream.py")).unwrap();
    assert!(
        stream_py.contains(
            "from tatolab.stream import CameraSource, DisplayWindow, StreamBuilder, stream"
        )
    );
    assert!(stream_py.contains("stream_builder.add(CameraSource)"));
    assert!(!stream_py.contains("TestPatternSource"));
    assert_eq!(
        stream_py,
        scaffold_template_text("stream.py")
            .strip_prefix(SCAFFOLD_TEMPLATE_LICENSE_HEADER)
            .unwrap(),
        "the camera variant is the template itself"
    );

    for node_module in [
        "nodes/__init__.py",
        "nodes/inverting_effect.py",
        "nodes/brightness_meter.py",
    ] {
        assert_eq!(
            fs::read_to_string(project_directory.join(node_module)).unwrap(),
            scaffold_template_text(node_module)
                .strip_prefix(SCAFFOLD_TEMPLATE_LICENSE_HEADER)
                .unwrap()
        );
    }

    let pyproject_toml = fs::read_to_string(project_directory.join("pyproject.toml")).unwrap();
    assert!(pyproject_toml.contains("name = \"my-probe-app\""));
    assert_eq!(
        pyproject_toml,
        scaffold_template_text("pyproject.toml")
            .replace("name = \"streamlib-app\"", "name = \"my-probe-app\"")
    );
    assert_eq!(
        fs::read_to_string(project_directory.join(".python-version")).unwrap(),
        scaffold_template_text("python-version")
    );
    assert_eq!(
        fs::read_to_string(project_directory.join(".gitignore")).unwrap(),
        scaffold_template_text("gitignore")
    );

    let next_steps = String::from_utf8(new_output.stdout).unwrap();
    assert!(next_steps.contains("    cd My Probe App\n"), "{next_steps}");
    assert!(next_steps.contains("    uv sync\n"), "{next_steps}");
    assert!(next_steps.contains("    tatolab dev\n"), "{next_steps}");
}

#[test]
fn new_with_test_pattern_wires_the_test_pattern_and_names_no_camera() {
    let scratch_directory = tempfile::tempdir().unwrap();
    let new_output = run_tatolab(
        scratch_directory.path(),
        &["new", "pattern-app", "--test-pattern"],
    );
    assert!(new_output.status.success());
    let project_directory = scratch_directory.path().join("pattern-app");
    assert_eq!(
        every_file_under(&project_directory),
        expected_scaffolded_file_paths()
    );

    let stream_py = fs::read_to_string(project_directory.join("stream.py")).unwrap();
    assert!(!stream_py.contains("SPDX-License-Identifier"));
    assert!(
        stream_py.contains(
            "from tatolab.stream import DisplayWindow, StreamBuilder, TestPatternSource, stream"
        ),
        "{stream_py}"
    );
    assert!(stream_py.contains("stream_builder.add(TestPatternSource)"));
    assert!(stream_py.contains("A StreamLib stream: test pattern →"));
    assert!(stream_py.contains("\"\"\"Test pattern, inverted,"));
    assert!(!stream_py.contains("CameraSource"));
    assert!(!stream_py.to_lowercase().contains("camera"), "{stream_py}");

    let pyproject_toml = fs::read_to_string(project_directory.join("pyproject.toml")).unwrap();
    assert!(pyproject_toml.contains("name = \"pattern-app\""));
}

#[test]
fn new_refuses_a_directory_holding_a_scaffolded_file_and_writes_nothing() {
    let scratch_directory = tempfile::tempdir().unwrap();
    let project_directory = scratch_directory.path().join("taken");
    fs::create_dir_all(&project_directory).unwrap();
    fs::write(project_directory.join("stream.py"), "# the user's own\n").unwrap();

    let new_output = run_tatolab(scratch_directory.path(), &["new", "taken"]);
    assert_eq!(new_output.status.code(), Some(1));
    let refusal = String::from_utf8(new_output.stderr).unwrap();
    assert!(refusal.starts_with("error: "), "{refusal}");
    assert!(refusal.contains("already has stream.py"), "{refusal}");
    assert_eq!(
        every_file_under(&project_directory),
        BTreeSet::from(["stream.py".to_owned()])
    );
    assert_eq!(
        fs::read_to_string(project_directory.join("stream.py")).unwrap(),
        "# the user's own\n"
    );
}
