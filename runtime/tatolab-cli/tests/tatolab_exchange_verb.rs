// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab exchange` run as a user runs it, against a stub local API: its flags, what it prints
//! on stdout and on stderr, and how it exits. Each sampling scenario is the in-crate tests'. The
//! tests that reach a runtime serve the stub at an isolated machine's fixed socket, which a build
//! without the test feature can isolate on Linux alone; the usage refusals read no runtime
//! directory and run on every floor.

mod common;

use common::tatolab_binary_run::{
    run_tatolab_reading_no_runtime_directory, standard_error_text, standard_output_text,
};

#[test]
fn the_exchange_help_names_both_forms_and_every_flag() {
    let help_text = standard_output_text(&run_tatolab_reading_no_runtime_directory(&[
        "exchange", "--help",
    ]));

    for named in [
        "[SURFACE_ID]",
        "--out <DIR>",
        "--channel <CHANNEL>",
        "<runtime_name>/<node>/<port>",
        "--count <N>",
        "--every <N>",
        "--field <NAME>",
        "--stream <STREAM>",
        "--out is not cleared",
    ] {
        assert!(help_text.contains(named), "{named}:\n{help_text}");
    }
    assert!(
        !help_text.contains('`'),
        "help reads plainly, with no markdown:\n{help_text}"
    );
}

/// Control is reachable only through a runtime's local API socket, so the verb dials no address.
/// The flag carries a value: a value-taking flag given none fails the same way.
#[test]
fn exchange_takes_no_network_address_for_the_control_plane() {
    let refused = run_tatolab_reading_no_runtime_directory(&[
        "exchange",
        "s#1",
        "--out",
        "unwritten",
        "--url",
        "http://127.0.0.1:9100",
    ]);

    assert_eq!(refused.status.code(), Some(2));
    let refusal = standard_error_text(&refused);
    assert!(refusal.contains("unexpected argument '--url'"), "{refusal}");
}

/// Without `--out` the verb would write PNGs into whatever directory it ran from, which is never
/// what a harness meant.
#[test]
fn the_output_directory_is_required() {
    let refused = run_tatolab_reading_no_runtime_directory(&["exchange", "s#1"]);

    assert_eq!(refused.status.code(), Some(2));
    assert!(
        standard_error_text(&refused).contains("--out <DIR>"),
        "{}",
        standard_error_text(&refused)
    );
}

#[test]
fn exchange_needs_a_surface_id_or_a_channel() {
    let scratch_directory = tempfile::tempdir().unwrap();

    let refused = run_tatolab_reading_no_runtime_directory(&[
        "exchange",
        "--out",
        scratch_directory.path().to_str().unwrap(),
    ]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        "error: `tatolab exchange` needs a surface id or `--channel`. Ids come from bags — \
         `tatolab tap <channel>` shows what one carries.\n"
    );
    assert_eq!(standard_output_text(&refused), "");
}

/// A negative bound parses as a value rather than as a flag, so the verb refuses it itself.
#[test]
fn a_sample_bound_below_one_is_refused() {
    let scratch_directory = tempfile::tempdir().unwrap();

    for (sample_bound_flag, below_one) in [("--count", "0"), ("--every", "0"), ("--count", "-1")] {
        let refused = run_tatolab_reading_no_runtime_directory(&[
            "exchange",
            "--channel",
            "cam/frame",
            "--stream",
            "camera",
            "--out",
            scratch_directory.path().to_str().unwrap(),
            sample_bound_flag,
            below_one,
        ]);

        assert_eq!(refused.status.code(), Some(1), "{sample_bound_flag}");
        assert_eq!(
            standard_error_text(&refused),
            format!("error: `{sample_bound_flag}` must be at least 1.\n")
        );
    }
}

#[cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]
mod against_an_isolated_machine {
    use std::path::{Path, PathBuf};
    use std::process::Output;

    use serde_json::json;

    use super::common::isolated_machine_directories::IsolatedMachineDirectories;
    use super::common::stub_local_api_server::{StubLocalApiScript, StubSurfaceImageAnswer};
    use super::common::tapped_channel_bag_fixtures::{
        bag_publishing_surface_id, labelled_png_image_answer, png_bytes_for, png_files_in,
        tap_result_text,
    };
    use super::common::tatolab_binary_run::{standard_error_text, standard_output_text};

    fn exchange_on_the_machines_runtime(
        isolated_machine_directories: &IsolatedMachineDirectories,
        exchange_arguments: &[&str],
    ) -> Output {
        isolated_machine_directories.run_tatolab(&[&["exchange"], exchange_arguments].concat())
    }

    fn sample_the_channel(
        isolated_machine_directories: &IsolatedMachineDirectories,
        output_directory: &Path,
        sampling_flags: &[&str],
    ) -> Output {
        exchange_on_the_machines_runtime(
            isolated_machine_directories,
            &[
                &[
                    "--channel",
                    "cam/frame",
                    "--stream",
                    "camera",
                    "--out",
                    output_directory.to_str().unwrap(),
                ],
                sampling_flags,
            ]
            .concat(),
        )
    }

    fn printed_image_contents(finished_run: &Output) -> Vec<Vec<u8>> {
        standard_output_text(finished_run)
            .lines()
            .map(|printed_image_path| std::fs::read(printed_image_path).unwrap())
            .collect()
    }

    #[test]
    fn the_id_form_writes_the_exact_bytes_and_prints_the_path() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let _stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            StubLocalApiScript::answering_surface_images([(
                "cam/frame#7",
                labelled_png_image_answer("seven"),
            )]),
        );
        let scratch_directory = tempfile::tempdir().unwrap();
        let output_directory = scratch_directory.path().join("frames");

        let exchanged = exchange_on_the_machines_runtime(
            &isolated_machine_directories,
            &["cam/frame#7", "--out", output_directory.to_str().unwrap()],
        );

        assert_eq!(
            exchanged.status.code(),
            Some(0),
            "{}",
            standard_error_text(&exchanged)
        );
        let written_image_path = output_directory.join("cam_frame_7.png");
        assert_eq!(
            standard_output_text(&exchanged),
            format!("{}\n", written_image_path.display())
        );
        assert_eq!(
            std::fs::read(&written_image_path).unwrap(),
            png_bytes_for("seven")
        );
        assert_eq!(standard_error_text(&exchanged), "");
    }

    #[test]
    fn a_surface_id_that_does_not_resolve_fails_the_verb() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let _stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            StubLocalApiScript::answering_surface_images(
                Vec::<(String, StubSurfaceImageAnswer)>::new(),
            ),
        );
        let output_directory = tempfile::tempdir().unwrap();

        let refused = exchange_on_the_machines_runtime(
            &isolated_machine_directories,
            &["gone#1", "--out", output_directory.path().to_str().unwrap()],
        );

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: exchange of surface `gone#1` answered 404: no such surface\n"
        );
        assert_eq!(standard_output_text(&refused), "");
    }

    /// `--out` spelled with `.` components prints each path without them.
    #[test]
    fn a_written_path_prints_without_the_outputs_current_directory_components() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let _stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            StubLocalApiScript::answering_surface_images([(
                "s#1",
                labelled_png_image_answer("one"),
            )]),
        );
        let scratch_directory = tempfile::tempdir().unwrap();
        let output_directory = scratch_directory.path().join("frames");

        let exchanged = exchange_on_the_machines_runtime(
            &isolated_machine_directories,
            &[
                "s#1",
                "--out",
                &format!("{}/./frames/", scratch_directory.path().display()),
            ],
        );

        assert_eq!(
            exchanged.status.code(),
            Some(0),
            "{}",
            standard_error_text(&exchanged)
        );
        assert_eq!(
            PathBuf::from(standard_output_text(&exchanged).trim_end()),
            output_directory.join("s_1.png")
        );
        assert_eq!(
            standard_output_text(&exchanged),
            format!("{}\n", output_directory.join("s_1.png").display())
        );
    }

    #[test]
    fn the_channel_form_taps_then_exchanges_each_sampled_id() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server =
            isolated_machine_directories.serve_stub_local_api(StubLocalApiScript::tapping(
                &[tap_result_text(&[
                    bag_publishing_surface_id("s#1"),
                    bag_publishing_surface_id("s#2"),
                ])],
                [
                    ("s#1", labelled_png_image_answer("one")),
                    ("s#2", labelled_png_image_answer("two")),
                ],
            ));
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &isolated_machine_directories,
            output_directory.path(),
            &["--count", "2"],
        );

        assert_eq!(
            sampled.status.code(),
            Some(0),
            "{}",
            standard_error_text(&sampled)
        );
        assert_eq!(
            stub_local_api_server.recorded_tool_calls()[0].tool_arguments,
            json!({"stream": "camera", "channel": "cam/frame", "count": 2})
        );
        assert_eq!(
            standard_output_text(&sampled),
            format!(
                "{}\n{}\n",
                output_directory.path().join("0000-s_1.png").display(),
                output_directory.path().join("0001-s_2.png").display()
            )
        );
        assert_eq!(
            printed_image_contents(&sampled),
            [png_bytes_for("one"), png_bytes_for("two")]
        );
        assert_eq!(
            standard_error_text(&sampled),
            "exchanged 2 of 2 requested frames from `cam/frame` (2 bags examined over 1 tap \
             round)\n"
        );
    }

    /// A harness reading the directory must not take "fewer frames than I asked for" as "this is
    /// all the channel had"; the one frame that landed is still named.
    #[test]
    fn a_short_sample_exits_nonzero() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let _stub_local_api_server =
            isolated_machine_directories.serve_stub_local_api(StubLocalApiScript::tapping(
                &[tap_result_text(&[bag_publishing_surface_id("s#1")])],
                [("s#1", labelled_png_image_answer("one"))],
            ));
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &isolated_machine_directories,
            output_directory.path(),
            &["--count", "3"],
        );

        assert_eq!(sampled.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&sampled),
            "exchanged 1 of 3 requested frames from `cam/frame` (1 bags examined over 8 tap \
             rounds)\n"
        );
        assert_eq!(printed_image_contents(&sampled), [png_bytes_for("one")]);
    }

    /// A PNG on disk whose path was never printed is evidence a harness cannot use and a human
    /// will not find, so the stop is reported beside the frames rather than instead of them.
    #[test]
    fn frames_that_landed_before_a_fatal_stop_are_still_printed() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let _stub_local_api_server =
            isolated_machine_directories.serve_stub_local_api(StubLocalApiScript::tapping(
                &[tap_result_text(&[
                    bag_publishing_surface_id("s#1"),
                    bag_publishing_surface_id("s#2"),
                ])],
                [
                    ("s#1", labelled_png_image_answer("one")),
                    (
                        "s#2",
                        StubSurfaceImageAnswer::refusal(404, "no such surface"),
                    ),
                ],
            ));
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &isolated_machine_directories,
            output_directory.path(),
            &["--count", "2"],
        );

        assert_eq!(sampled.status.code(), Some(1));
        let printed_image_contents = printed_image_contents(&sampled);
        assert_eq!(printed_image_contents, [png_bytes_for("one")]);
        assert_eq!(
            standard_error_text(&sampled),
            "exchanged 1 of 2 requested frames from `cam/frame` (2 bags examined over 1 tap \
             round)\n\
             error: exchange of surface `s#2` answered 404: no such surface\n"
        );
        assert_eq!(
            png_files_in(output_directory.path()),
            printed_image_contents.len()
        );
    }
}
