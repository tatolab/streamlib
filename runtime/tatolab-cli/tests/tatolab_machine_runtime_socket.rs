// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Every verb that reaches the machine's runtime does so through one socket at a fixed path, and
//! with nothing answering there refuses at once, naming it and how to start a runtime: no verb
//! starts one. The registry's verb and flags are gone, and asking for them is a usage error. The
//! holder line is the in-crate tests'.

mod common;

use common::tatolab_binary_run::{run_tatolab_reading_no_runtime_directory, standard_error_text};

#[test]
fn the_retired_verbs_and_flags_are_usage_errors() {
    for (retired_arguments, named_in_the_usage_error) in [
        (&["nodes"][..], "unrecognized subcommand 'nodes'"),
        (
            &["graph", "--node", "rig"][..],
            "unexpected argument '--node'",
        ),
        (
            &["tap", "rig/cam/video", "--stream", "cam", "--node", "rig"][..],
            "unexpected argument '--node'",
        ),
        (
            &["logs", "--node", "rig"][..],
            "unexpected argument '--node'",
        ),
        (
            &["exchange", "s#1", "--out", "frames", "--node", "rig"][..],
            "unexpected argument '--node'",
        ),
        (
            &["mcp", "--node", "rig"][..],
            "unexpected argument '--node'",
        ),
        (
            &["run", "--runtime-name", "rig"][..],
            "unexpected argument '--runtime-name'",
        ),
        (
            &["dev", "--runtime-name", "rig"][..],
            "unexpected argument '--runtime-name'",
        ),
        (
            &["logs", "Rabc", "--count", "4"][..],
            "unexpected argument '--count'",
        ),
    ] {
        let refused = run_tatolab_reading_no_runtime_directory(retired_arguments);

        assert_eq!(refused.status.code(), Some(2), "{retired_arguments:?}");
        let usage_error = standard_error_text(&refused);
        assert!(
            usage_error.contains(named_in_the_usage_error),
            "{retired_arguments:?}: {usage_error}"
        );
    }
}

#[test]
fn a_target_beside_a_file_is_a_usage_error() {
    let refused =
        run_tatolab_reading_no_runtime_directory(&["run", "stream.py:main", "-f", "x.py"]);

    assert_eq!(refused.status.code(), Some(2));
    let usage_error = standard_error_text(&refused);
    assert!(usage_error.contains("cannot be used with"), "{usage_error}");
}

#[test]
fn public_beside_remove_is_a_usage_error() {
    let refused = run_tatolab_reading_no_runtime_directory(&[
        "expose", "camera", "effect", "video", "--public", "--remove",
    ]);

    assert_eq!(refused.status.code(), Some(2));
    let usage_error = standard_error_text(&refused);
    assert!(usage_error.contains("cannot be used with"), "{usage_error}");
}

#[cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]
mod against_an_isolated_machine {
    use super::common::isolated_machine_directories::IsolatedMachineDirectories;
    use super::common::tatolab_binary_run::{standard_error_text, standard_output_text};

    #[test]
    fn with_no_runtime_every_verb_fails_at_once_naming_the_socket_and_how_to_start_one() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let scratch_directory = tempfile::tempdir().unwrap();
        let output_directory = scratch_directory.path().join("frames");
        let output_directory = output_directory.to_str().unwrap();
        let project_directory = scratch_directory.path().to_str().unwrap();
        let expected_refusal_line = format!(
            "error: no runtime is running on this machine: nothing answers at {}. Start one by \
             running `tatolabd` in a terminal.",
            isolated_machine_directories
                .local_api_socket_path()
                .display()
        );

        for verb_arguments in [
            &["run", "--dir", project_directory][..],
            &["run", "-d", "--dir", project_directory][..],
            &["dev", "--dir", project_directory][..],
            &["stop", "camera"][..],
            &["start", "camera"][..],
            &["rm", "camera"][..],
            &["streams"][..],
            &["expose", "camera", "effect", "video"][..],
            &["graph"][..],
            &["graph", "--stream", "camera"][..],
            &["tap", "rig/effect/video", "--stream", "camera"][..],
            &["logs", "--stream", "camera"][..],
            &["logs", "--stream", "camera", "-f"][..],
            &["exchange", "s#1", "--out", output_directory][..],
            &[
                "exchange",
                "--channel",
                "rig/effect/video",
                "--stream",
                "camera",
                "--out",
                output_directory,
            ][..],
            &["mcp"][..],
        ] {
            let refused = isolated_machine_directories.run_tatolab(verb_arguments);

            assert_eq!(refused.status.code(), Some(1), "{verb_arguments:?}");
            assert_eq!(standard_output_text(&refused), "", "{verb_arguments:?}");
            let standard_error = standard_error_text(&refused);
            let refusal_lines: Vec<&str> = standard_error.lines().collect();
            assert_eq!(
                refusal_lines.len(),
                1,
                "{verb_arguments:?}: {standard_error}"
            );
            if cfg!(feature = "machine-directories-under-a-test-root") {
                assert_eq!(
                    refusal_lines[0], expected_refusal_line,
                    "{verb_arguments:?}"
                );
            } else {
                // The real machine's lock may name a holder serving another user.
                assert!(
                    refusal_lines[0].starts_with(&expected_refusal_line),
                    "{verb_arguments:?}: {standard_error}"
                );
            }
        }
        assert!(
            !output_directory_was_written(output_directory),
            "a refused exchange writes nothing"
        );
    }

    fn output_directory_was_written(output_directory: &str) -> bool {
        std::path::Path::new(output_directory).exists()
    }
}
