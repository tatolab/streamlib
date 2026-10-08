// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab exchange` run as a user runs it, against a stub local API. The registry is isolated
//! through `XDG_RUNTIME_DIR`, which only Linux honours, so the tests that read one are Linux-only;
//! the usage refusals read none and run on every floor, and the in-crate tests drive the same
//! logic on every floor.

mod common;

#[path = "common/tapped_channel_bag_fixtures.rs"]
mod tapped_channel_bag_fixtures;

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
        "--node <RUNTIME_NAME_OR_ID>",
        "--out is not cleared",
    ] {
        assert!(help_text.contains(named), "{named}:\n{help_text}");
    }
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
}

#[test]
fn exchange_refuses_a_surface_id_and_a_channel_together() {
    let scratch_directory = tempfile::tempdir().unwrap();

    let refused = run_tatolab_reading_no_runtime_directory(&[
        "exchange",
        "s#1",
        "--channel",
        "cam/frame",
        "--out",
        scratch_directory.path().to_str().unwrap(),
    ]);

    assert_eq!(refused.status.code(), Some(1));
    assert!(
        standard_error_text(&refused).contains("not both"),
        "{}",
        standard_error_text(&refused)
    );
}

/// These sample a channel, and a surface id already names one frame, so applying them would be
/// silently ignored. Asking explicitly for the value the channel form defaults to is still asking
/// for the channel form.
#[test]
fn a_channel_form_flag_beside_a_surface_id_is_refused() {
    let scratch_directory = tempfile::tempdir().unwrap();

    for (channel_form_flag, flag_value) in [
        ("--count", "3"),
        ("--every", "2"),
        ("--field", "frame_id"),
        ("--count", "1"),
    ] {
        let refused = run_tatolab_reading_no_runtime_directory(&[
            "exchange",
            "s#1",
            "--out",
            scratch_directory.path().to_str().unwrap(),
            channel_form_flag,
            flag_value,
        ]);

        assert_eq!(refused.status.code(), Some(1), "{channel_form_flag}");
        assert_eq!(
            standard_error_text(&refused),
            format!(
                "error: {channel_form_flag} sample a channel, and a surface id names one frame \
                 already. Use `--channel` instead of SURFACE_ID.\n"
            )
        );
    }
}

#[test]
fn a_sample_bound_below_one_is_refused() {
    let scratch_directory = tempfile::tempdir().unwrap();

    for (sample_bound_flag, below_one) in [("--count", "0"), ("--every", "0"), ("--count", "-1")] {
        let refused = run_tatolab_reading_no_runtime_directory(&[
            "exchange",
            "--channel",
            "cam/frame",
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

#[cfg(target_os = "linux")]
mod against_an_isolated_registry {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::process::Output;

    use serde_json::json;

    use super::common::isolated_node_registry::{
        IsolatedNodeRegistry, a_registry_entry, a_registry_entry_named,
    };
    use super::common::stub_local_api_server::{
        StubLocalApiScript, StubLocalApiServer, StubSurfaceImageAnswer, StubToolAnswer,
    };
    use super::common::tatolab_binary_run::{
        run_tatolab_with_xdg_runtime_dir, standard_error_text, standard_output_text,
    };
    use super::tapped_channel_bag_fixtures::{
        SLICE_HOLDS_ONLY_THE_BAG, bag_publishing_no_surface_id, bag_publishing_surface_id,
        bag_publishing_surface_id_in_field, empty_tap_result_text, framed_bag, msgpack_named_map,
        png_bytes_for, tap_result_text, tap_result_text_capping_bags,
    };

    fn image_answer(label: &str) -> StubSurfaceImageAnswer {
        StubSurfaceImageAnswer::png_image(&png_bytes_for(label), Some(1920), Some(1080))
    }

    fn surface_image_answers_by_id<PublishedSurfaceId: Into<String>>(
        surface_image_answers: impl IntoIterator<Item = (PublishedSurfaceId, StubSurfaceImageAnswer)>,
    ) -> HashMap<String, StubSurfaceImageAnswer> {
        surface_image_answers
            .into_iter()
            .map(|(published_surface_id, answer)| (published_surface_id.into(), answer))
            .collect()
    }

    fn stub_answering_surface_images<PublishedSurfaceId: Into<String>>(
        surface_image_answers: impl IntoIterator<Item = (PublishedSurfaceId, StubSurfaceImageAnswer)>,
    ) -> StubLocalApiServer {
        StubLocalApiServer::serve(StubLocalApiScript {
            surface_image_answers: surface_image_answers_by_id(surface_image_answers),
            ..StubLocalApiScript::default()
        })
    }

    /// A stub whose `tap` answers `queued_tap_results` in order, then an empty round forever.
    fn stub_tapping<PublishedSurfaceId: Into<String>>(
        queued_tap_results: &[String],
        surface_image_answers: impl IntoIterator<Item = (PublishedSurfaceId, StubSurfaceImageAnswer)>,
    ) -> StubLocalApiServer {
        StubLocalApiServer::serve(StubLocalApiScript {
            fixed_tool_answer: Some(StubToolAnswer::tool_result(&empty_tap_result_text())),
            queued_tool_answers: queued_tap_results
                .iter()
                .map(|tap_result| StubToolAnswer::tool_result(tap_result))
                .collect(),
            surface_image_answers: surface_image_answers_by_id(surface_image_answers),
            ..StubLocalApiScript::default()
        })
    }

    /// A registry whose one live runtime is `stub_local_api_server`.
    fn registry_holding_only(stub_local_api_server: &StubLocalApiServer) -> IsolatedNodeRegistry {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        isolated_node_registry.write_registry_entry(&a_registry_entry(
            "Ronly",
            &stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry
    }

    fn exchange_on_the_sole_runtime(
        stub_local_api_server: &StubLocalApiServer,
        exchange_arguments: &[&str],
    ) -> Output {
        let isolated_node_registry = registry_holding_only(stub_local_api_server);
        run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &[&["exchange"], exchange_arguments].concat(),
        )
    }

    fn sample_the_channel(
        stub_local_api_server: &StubLocalApiServer,
        output_directory: &Path,
        sampling_flags: &[&str],
    ) -> Output {
        exchange_on_the_sole_runtime(
            stub_local_api_server,
            &[
                &[
                    "--channel",
                    "cam/frame",
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

    fn png_files_in(directory: &Path) -> usize {
        std::fs::read_dir(directory)
            .unwrap()
            .filter(|directory_entry| {
                directory_entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "png")
            })
            .count()
    }

    // `tatolab exchange <SURFACE_ID>`.

    #[test]
    fn the_id_form_writes_the_exact_bytes_and_prints_the_path() {
        let stub_local_api_server =
            stub_answering_surface_images([("cam/frame#7", image_answer("seven"))]);
        let scratch_directory = tempfile::tempdir().unwrap();
        let output_directory = scratch_directory.path().join("frames");

        let exchanged = exchange_on_the_sole_runtime(
            &stub_local_api_server,
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
    fn the_id_form_reaches_a_registered_runtime_through_its_local_api_socket() {
        let stub_local_api_server =
            stub_answering_surface_images([("cam/frame#7", image_answer("seven"))]);
        let other_stub_local_api_server = StubLocalApiServer::serve_default();
        let isolated_node_registry = IsolatedNodeRegistry::new();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rcam",
            "rig-cam",
            &stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rother",
            "rig-other",
            &other_stub_local_api_server.local_api_socket_path,
        ));
        let output_directory = tempfile::tempdir().unwrap();

        let exchanged = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &[
                "exchange",
                "cam/frame#7",
                "--out",
                output_directory.path().to_str().unwrap(),
                "--node",
                "rig-cam",
            ],
        );

        assert_eq!(
            exchanged.status.code(),
            Some(0),
            "{}",
            standard_error_text(&exchanged)
        );
        assert_eq!(printed_image_contents(&exchanged), [png_bytes_for("seven")]);
        assert_eq!(
            stub_local_api_server.recorded_image_request_paths(),
            ["/api/surfaces/cam%2Fframe%237/image"]
        );
        assert!(
            other_stub_local_api_server
                .recorded_image_request_paths()
                .is_empty()
        );
    }

    #[test]
    fn the_id_form_creates_the_output_directory() {
        let stub_local_api_server = stub_answering_surface_images([("s#1", image_answer("one"))]);
        let scratch_directory = tempfile::tempdir().unwrap();
        let output_directory = scratch_directory.path().join("nested").join("frames");

        let exchanged = exchange_on_the_sole_runtime(
            &stub_local_api_server,
            &["s#1", "--out", output_directory.to_str().unwrap()],
        );

        assert_eq!(
            exchanged.status.code(),
            Some(0),
            "{}",
            standard_error_text(&exchanged)
        );
        assert!(output_directory.is_dir());
        assert_eq!(png_files_in(&output_directory), 1);
    }

    #[test]
    fn a_surface_id_that_does_not_resolve_fails_the_verb() {
        let stub_local_api_server =
            stub_answering_surface_images(Vec::<(String, StubSurfaceImageAnswer)>::new());
        let output_directory = tempfile::tempdir().unwrap();

        let refused = exchange_on_the_sole_runtime(
            &stub_local_api_server,
            &["gone#1", "--out", output_directory.path().to_str().unwrap()],
        );

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: exchange of surface `gone#1` answered 404: no such surface\n"
        );
        assert_eq!(standard_output_text(&refused), "");
    }

    /// `--out` naming an existing regular file is a typo, and a typo gets a message.
    #[test]
    fn an_output_directory_that_cannot_be_written_is_reported_not_raised() {
        let stub_local_api_server = stub_answering_surface_images([("s#1", image_answer("one"))]);
        let scratch_directory = tempfile::tempdir().unwrap();
        let already_a_file = scratch_directory.path().join("already-a-file");
        std::fs::write(&already_a_file, "not a directory").unwrap();

        let refused = exchange_on_the_sole_runtime(
            &stub_local_api_server,
            &["s#1", "--out", already_a_file.to_str().unwrap()],
        );

        assert_eq!(refused.status.code(), Some(1));
        let refusal = standard_error_text(&refused);
        assert!(
            refusal.starts_with(&format!(
                "error: could not write into `{}`: ",
                already_a_file.display()
            )),
            "{refusal}"
        );
        assert_eq!(standard_output_text(&refused), "");
    }

    /// Usage is checked first, then the runtime is picked.
    #[test]
    fn an_exchange_with_no_runtime_running_names_the_command_that_starts_one() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let output_directory = tempfile::tempdir().unwrap();

        let refused = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &[
                "exchange",
                "s#1",
                "--out",
                output_directory.path().to_str().unwrap(),
            ],
        );

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: no running runtime found on this machine.\nStart one with `tatolab run`.\n"
        );
    }

    // `tatolab exchange --channel`.

    #[test]
    fn the_channel_form_taps_then_exchanges_each_sampled_id() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&[
                bag_publishing_surface_id("s#1"),
                bag_publishing_surface_id("s#2"),
            ])],
            [("s#1", image_answer("one")), ("s#2", image_answer("two"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
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
            json!({"channel": "cam/frame", "count": 2})
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

    #[test]
    fn the_channel_form_reaches_a_registered_runtime_through_its_local_api_socket() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&[
                bag_publishing_surface_id("s#1"),
                bag_publishing_surface_id("s#2"),
            ])],
            [("s#1", image_answer("one")), ("s#2", image_answer("two"))],
        );
        let other_stub_local_api_server = StubLocalApiServer::serve_default();
        let isolated_node_registry = IsolatedNodeRegistry::new();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rcam",
            "rig-cam",
            &stub_local_api_server.local_api_socket_path,
        ));
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rother",
            "rig-other",
            &other_stub_local_api_server.local_api_socket_path,
        ));
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &[
                "exchange",
                "--channel",
                "cam/frame",
                "--count",
                "2",
                "--out",
                output_directory.path().to_str().unwrap(),
                "--node",
                "rig-cam",
            ],
        );

        assert_eq!(
            sampled.status.code(),
            Some(0),
            "{}",
            standard_error_text(&sampled)
        );
        assert_eq!(
            printed_image_contents(&sampled),
            [png_bytes_for("one"), png_bytes_for("two")]
        );
        assert_eq!(
            stub_local_api_server.recorded_image_request_paths(),
            ["/api/surfaces/s%231/image", "/api/surfaces/s%232/image"]
        );
        assert!(other_stub_local_api_server.recorded_tool_calls().is_empty());
    }

    /// The composition is the client's whole job: `tap` keeps its contract, gaining no field
    /// argument and no decode.
    #[test]
    fn the_runtime_is_never_asked_to_read_a_bag() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&[bag_publishing_surface_id("s#1")])],
            [("s#1", image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
            output_directory.path(),
            &["--field", "surface_id"],
        );

        assert_eq!(
            sampled.status.code(),
            Some(0),
            "{}",
            standard_error_text(&sampled)
        );
        assert_eq!(
            stub_local_api_server.recorded_tool_calls()[0].tool_arguments,
            json!({"channel": "cam/frame", "count": 1})
        );
    }

    #[test]
    fn the_field_override_reads_the_key_the_caller_named() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&[bag_publishing_surface_id_in_field(
                "s#9",
                "rendered_surface",
            )])],
            [("s#9", image_answer("nine"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
            output_directory.path(),
            &["--field", "rendered_surface"],
        );

        assert_eq!(
            sampled.status.code(),
            Some(0),
            "{}",
            standard_error_text(&sampled)
        );
        assert_eq!(printed_image_contents(&sampled), [png_bytes_for("nine")]);
    }

    /// The loud half of the contract: the run recovers and says which id it had to give up on,
    /// so a sample never quietly becomes a different frame.
    #[test]
    fn a_recycled_frame_is_retried_against_a_newer_bag_and_reported() {
        let stub_local_api_server = stub_tapping(
            &[
                tap_result_text(&[bag_publishing_surface_id("stale#1")]),
                tap_result_text(&[bag_publishing_surface_id("fresh#2")]),
            ],
            [
                (
                    "stale#1",
                    StubSurfaceImageAnswer::refusal(
                        410,
                        "surface frame recycled: slot reused since that generation",
                    ),
                ),
                ("fresh#2", image_answer("fresh")),
            ],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(&stub_local_api_server, output_directory.path(), &[]);

        assert_eq!(
            sampled.status.code(),
            Some(0),
            "{}",
            standard_error_text(&sampled)
        );
        assert_eq!(printed_image_contents(&sampled), [png_bytes_for("fresh")]);
        assert_eq!(
            standard_error_text(&sampled),
            "exchanged 1 of 1 requested frames from `cam/frame` (2 bags examined over 2 tap \
             rounds)\n\
             retried 1 recycled frame against newer bags: stale#1\n"
        );
    }

    #[test]
    fn a_bag_without_the_named_field_is_counted_rather_than_fatal() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&[
                bag_publishing_no_surface_id(),
                bag_publishing_surface_id("s#1"),
            ])],
            [("s#1", image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(&stub_local_api_server, output_directory.path(), &[]);

        assert_eq!(
            sampled.status.code(),
            Some(0),
            "{}",
            standard_error_text(&sampled)
        );
        assert_eq!(printed_image_contents(&sampled), [png_bytes_for("one")]);
        assert!(
            standard_error_text(&sampled).contains(
                "1 bag carried no surface id in the named field — name the right one with \
                 `--field`\n"
            ),
            "{}",
            standard_error_text(&sampled)
        );
    }

    /// Enough bags to satisfy the stride are asked for, not just the frame count.
    #[test]
    fn every_nth_bag_selects_the_stride() {
        let labels = ["a", "b", "c", "d", "e", "f"];
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&labels.map(|label| {
                bag_publishing_surface_id(&format!("s#{label}"))
            }))],
            labels.map(|label| (format!("s#{label}"), image_answer(label))),
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
            output_directory.path(),
            &["--count", "2", "--every", "3"],
        );

        assert_eq!(
            sampled.status.code(),
            Some(0),
            "{}",
            standard_error_text(&sampled)
        );
        assert_eq!(
            printed_image_contents(&sampled),
            [png_bytes_for("a"), png_bytes_for("d")]
        );
        assert_eq!(
            stub_local_api_server.recorded_tool_calls()[0].tool_arguments["count"],
            6
        );
    }

    /// A stride reset per round would exchange `a` then `c`, the first bag of each round,
    /// reporting a stride it did not apply.
    #[test]
    fn the_stride_runs_across_tap_rounds_rather_than_restarting() {
        let stub_local_api_server = stub_tapping(
            &[
                tap_result_text(
                    &["a", "b"].map(|label| bag_publishing_surface_id(&format!("s#{label}"))),
                ),
                tap_result_text(
                    &["c", "d"].map(|label| bag_publishing_surface_id(&format!("s#{label}"))),
                ),
            ],
            ["a", "b", "c", "d"].map(|label| (format!("s#{label}"), image_answer(label))),
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
            output_directory.path(),
            &["--count", "2", "--every", "3"],
        );

        assert_eq!(
            sampled.status.code(),
            Some(0),
            "{}",
            standard_error_text(&sampled)
        );
        assert_eq!(
            printed_image_contents(&sampled),
            [png_bytes_for("a"), png_bytes_for("d")],
            "the stride restarted at each tap round"
        );
    }

    /// A harness reading the directory must not take "fewer frames than I asked for" as "this is
    /// all the channel had"; the one frame that landed is still named.
    #[test]
    fn a_short_sample_exits_nonzero() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&[bag_publishing_surface_id("s#1")])],
            [("s#1", image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
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

    #[test]
    fn a_refusal_that_cannot_be_retried_stops_the_run() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&[bag_publishing_surface_id("s#1")])],
            [(
                "s#1",
                StubSurfaceImageAnswer::refusal(501, "no conversion arm"),
            )],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(&stub_local_api_server, output_directory.path(), &[]);

        assert_eq!(sampled.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&sampled),
            "exchanged 0 of 1 requested frames from `cam/frame` (1 bags examined over 1 tap \
             round)\n\
             error: exchange of surface `s#1` answered 501: no conversion arm\n"
        );
        assert_eq!(standard_output_text(&sampled), "");
    }

    /// A PNG on disk whose path was never printed is evidence a harness cannot use and a human
    /// will not find, so the stop is reported beside the frames rather than instead of them.
    #[test]
    fn frames_that_landed_before_a_fatal_stop_are_still_printed() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&[
                bag_publishing_surface_id("s#1"),
                bag_publishing_surface_id("s#2"),
            ])],
            [
                ("s#1", image_answer("one")),
                (
                    "s#2",
                    StubSurfaceImageAnswer::refusal(404, "no such surface"),
                ),
            ],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
            output_directory.path(),
            &["--count", "2"],
        );

        assert_eq!(sampled.status.code(), Some(1));
        let printed_image_contents = printed_image_contents(&sampled);
        assert_eq!(printed_image_contents, [png_bytes_for("one")]);
        assert!(
            standard_error_text(&sampled).contains("no such surface"),
            "{}",
            standard_error_text(&sampled)
        );
        assert_eq!(
            png_files_in(output_directory.path()),
            printed_image_contents.len()
        );
    }

    /// The tap hex-previews only a bounded prefix of a large bag; decoding that prefix would hand
    /// back a bag missing its later fields, possibly the surface id.
    #[test]
    fn a_bag_the_tap_truncated_stops_the_run_by_name() {
        let whole_bag = framed_bag(
            &msgpack_named_map(&[
                ("surface_id", "s#1".into()),
                ("filler", "x".repeat(200).into()),
            ]),
            SLICE_HOLDS_ONLY_THE_BAG,
        );
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&[
                whole_bag[..whole_bag.len() - 32].to_vec()
            ])],
            Vec::<(String, StubSurfaceImageAnswer)>::new(),
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(&stub_local_api_server, output_directory.path(), &[]);

        assert_eq!(sampled.status.code(), Some(1));
        assert!(
            standard_error_text(&sampled).contains("truncated"),
            "{}",
            standard_error_text(&sampled)
        );
    }

    /// Counting a capped bag as "published no surface id" would blame the channel for something
    /// this client could not read, and retrying it would never converge.
    #[test]
    fn a_bag_past_the_taps_preview_cap_stops_the_run_and_names_the_size() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text_capping_bags(
                &[bag_publishing_surface_id("s#1")],
                &[0],
                true,
            )],
            [("s#1", image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(&stub_local_api_server, output_directory.path(), &[]);

        assert_eq!(sampled.status.code(), Some(1));
        let reported = standard_error_text(&sampled);
        assert!(
            reported.contains("past the prefix `tap` previews"),
            "{reported}"
        );
        assert!(
            reported.contains("9000 bytes"),
            "the diagnosis must name the size that did not fit: {reported}"
        );
        assert!(
            reported.contains("`tatolab exchange <surface-id> --out <dir>`"),
            "the id form still reaches such a frame: {reported}"
        );
    }

    /// The size is the tool's to state and may be missing; losing the cap would misdiagnose the
    /// bag as one this client could not decode.
    #[test]
    fn a_capped_bag_with_no_stated_size_is_still_diagnosed_as_capped() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text_capping_bags(
                &[bag_publishing_surface_id("s#1")],
                &[0],
                false,
            )],
            [("s#1", image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(&stub_local_api_server, output_directory.path(), &[]);

        assert_eq!(sampled.status.code(), Some(1));
        let reported = standard_error_text(&sampled);
        assert!(
            reported.contains("is larger than, past the prefix `tap` previews"),
            "{reported}"
        );
    }

    /// Bag 0 is selected but publishes no id, so the loop must reach the capped bag 1 and pass it
    /// by; without that the run finishes on bag 0 and never proves where the cap check sits.
    #[test]
    fn the_stride_steps_over_an_oversized_bag_rather_than_dying_on_it() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text_capping_bags(
                &[
                    bag_publishing_no_surface_id(),
                    bag_publishing_surface_id("s#2"),
                    bag_publishing_surface_id("s#3"),
                ],
                &[1],
                true,
            )],
            [("s#3", image_answer("three"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
            output_directory.path(),
            &["--count", "1", "--every", "2"],
        );

        assert_eq!(
            sampled.status.code(),
            Some(0),
            "a capped bag the stride skipped ended a run that never needed it: {}",
            standard_error_text(&sampled)
        );
        assert_eq!(printed_image_contents(&sampled), [png_bytes_for("three")]);
    }

    /// Bag 1 is past the preview cap and `--every 2` never selects it.
    #[test]
    fn a_bag_the_stride_skips_cannot_kill_the_run_by_being_oversized() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text_capping_bags(
                &[
                    bag_publishing_surface_id("s#1"),
                    bag_publishing_surface_id("s#2"),
                ],
                &[1],
                true,
            )],
            [("s#1", image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
            output_directory.path(),
            &["--count", "1", "--every", "2"],
        );

        assert_eq!(
            sampled.status.code(),
            Some(0),
            "{}",
            standard_error_text(&sampled)
        );
        assert_eq!(printed_image_contents(&sampled), [png_bytes_for("one")]);
    }

    /// Bag 0 is readable and bag 1 is not; failing the whole round would throw away a frame
    /// already exchanged.
    #[test]
    fn an_oversized_bag_does_not_discard_the_readable_bags_beside_it() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text_capping_bags(
                &[
                    bag_publishing_surface_id("s#1"),
                    bag_publishing_surface_id("s#2"),
                ],
                &[1],
                true,
            )],
            [("s#1", image_answer("one"))],
        );
        let output_directory = tempfile::tempdir().unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
            output_directory.path(),
            &["--count", "2"],
        );

        assert_eq!(sampled.status.code(), Some(1));
        assert_eq!(printed_image_contents(&sampled), [png_bytes_for("one")]);
        assert!(
            standard_error_text(&sampled).contains("9000 bytes"),
            "{}",
            standard_error_text(&sampled)
        );
    }

    /// The second frame's file name is already a directory, so its write fails after the first
    /// frame landed — the filesystem half of the promise the report keeps.
    #[test]
    fn a_write_that_fails_still_names_the_frames_that_landed() {
        let stub_local_api_server = stub_tapping(
            &[tap_result_text(&[
                bag_publishing_surface_id("s#1"),
                bag_publishing_surface_id("s#2"),
            ])],
            [("s#1", image_answer("one")), ("s#2", image_answer("two"))],
        );
        let output_directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(output_directory.path().join("0001-s_2.png")).unwrap();

        let sampled = sample_the_channel(
            &stub_local_api_server,
            output_directory.path(),
            &["--count", "2"],
        );

        assert_eq!(sampled.status.code(), Some(1));
        assert_eq!(printed_image_contents(&sampled), [png_bytes_for("one")]);
        let reported = standard_error_text(&sampled);
        assert!(
            reported.contains(&format!(
                "error: could not write into `{}`: ",
                output_directory.path().display()
            )),
            "{reported}"
        );
    }

    /// `--out` spelled with `.` components prints each path without them.
    #[test]
    fn a_written_path_prints_without_the_outputs_current_directory_components() {
        let stub_local_api_server = stub_answering_surface_images([("s#1", image_answer("one"))]);
        let scratch_directory = tempfile::tempdir().unwrap();
        let output_directory = scratch_directory.path().join("frames");

        let exchanged = exchange_on_the_sole_runtime(
            &stub_local_api_server,
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
}
