// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab run`, `stop`, `start`, `rm`, `rename`, `streams` and `expose` run as a user runs
//! them, against a stub local API at an isolated machine's fixed socket: the tool each calls, the
//! arguments it sends, the one line or table it prints, and how a refusal reads. A verb naming one
//! stream resolves its argument through `list_streams` — by id, by a display name one stream
//! holds, or by the working directory — and the tool sees the id alone.

#![cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]

mod common;

use std::collections::HashMap;
use std::path::Path;

use serde_json::json;

use common::isolated_machine_directories::IsolatedMachineDirectories;
use common::stub_local_api_server::{
    RecordedToolCall, StubLocalApiScript, StubLocalApiServer, StubToolAnswer,
    StubToolCallTransport, failed_run_stream_tool_result_text, run_stream_tool_result_text,
    stop_stream_tool_result_text,
};
use common::tatolab_binary_run::{standard_error_text, standard_output_text};

/// One stream as `list_streams` lists it, loaded from `project_directory`.
fn listed_stream(
    stream_id: &str,
    display_name: &str,
    project_directory: &Path,
) -> serde_json::Value {
    json!({
        "stream": stream_id, "display_name": display_name, "state": "running",
        "location": {"project_directory": project_directory, "stream_function": "stream:main"},
        "node_count": 3, "failed_because": null,
    })
}

/// A machine whose runtime lists `listed_streams` and answers `tool_name` with `tool_answer`.
fn a_runtime_listing_and_answering(
    listed_streams: Vec<serde_json::Value>,
    tool_name: &str,
    tool_answer: StubToolAnswer,
) -> (IsolatedMachineDirectories, StubLocalApiServer) {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tool_answers_by_name: HashMap::from([
                (
                    "list_streams".to_owned(),
                    vec![StubToolAnswer::tool_result(
                        &json!({ "streams": listed_streams }).to_string(),
                    )],
                ),
                (tool_name.to_owned(), vec![tool_answer]),
            ]),
            ..StubLocalApiScript::default()
        });
    (isolated_machine_directories, stub_local_api_server)
}

/// A machine whose runtime holds the one stream `cam` and answers `tool_name` with `tool_answer`.
fn a_runtime_answering(
    tool_name: &str,
    tool_answer: StubToolAnswer,
) -> (IsolatedMachineDirectories, StubLocalApiServer) {
    a_runtime_listing_and_answering(
        vec![listed_stream("cam", "Camera", Path::new("/srv/cam"))],
        tool_name,
        tool_answer,
    )
}

/// Run `tatolab_arguments` in `working_directory` to its exit.
fn run_tatolab_in(
    isolated_machine_directories: &IsolatedMachineDirectories,
    working_directory: &Path,
    tatolab_arguments: &[&str],
) -> std::process::Output {
    isolated_machine_directories
        .tatolab_command(tatolab_arguments)
        .current_dir(working_directory)
        .output()
        .unwrap()
}

/// Run `tatolab_arguments` to a successful exit, answering what it printed.
fn printed_by_a_successful(
    isolated_machine_directories: &IsolatedMachineDirectories,
    tatolab_arguments: &[&str],
) -> String {
    let finished = isolated_machine_directories.run_tatolab(tatolab_arguments);
    assert_eq!(
        finished.status.code(),
        Some(0),
        "{tatolab_arguments:?}: {}",
        standard_error_text(&finished)
    );
    assert_eq!(standard_error_text(&finished), "", "{tatolab_arguments:?}");
    standard_output_text(&finished)
}

#[test]
fn run_loads_the_stream_prints_its_id_and_how_to_follow_it_and_returns() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "run_stream",
        StubToolAnswer::tool_result(&run_stream_tool_result_text(
            "cam",
            &canonical_project_directory,
            3,
            &[],
        )),
    );

    let printed = printed_by_a_successful(
        &isolated_machine_directories,
        &[
            "run",
            "stream.py:main",
            "--dir",
            project_directory.path().to_str().unwrap(),
            "--name",
            "cam",
        ],
    );

    assert_eq!(
        printed,
        format!(
            "cam running as \"main\" (project {0}, stream:main)\n  follow it: tatolab logs -f --dir {0}\n",
            canonical_project_directory.display()
        )
    );
    assert_eq!(
        stub_local_api_server.recorded_tool_calls(),
        [RecordedToolCall {
            tool_name: "run_stream".to_owned(),
            tool_arguments: json!({
                "project_directory": canonical_project_directory,
                "stream_function": "stream.py:main",
                "name": "cam",
            }),
            tool_call_transport: StubToolCallTransport::StreamableHttpPost,
        }]
    );
    assert_eq!(
        stub_local_api_server.recorded_mcp_stdio_request_heads(),
        [],
        "a load is one call over POST /mcp, never a connection of its own"
    );
}

#[test]
fn run_takes_no_detach_flag() {
    let project_directory = tempfile::tempdir().unwrap();
    let (isolated_machine_directories, stub_local_api_server) =
        a_runtime_answering("run_stream", StubToolAnswer::tool_failure("unreached"));

    for detach_flag in ["-d", "--detach"] {
        let refused = isolated_machine_directories.run_tatolab(&[
            "run",
            detach_flag,
            "--dir",
            project_directory.path().to_str().unwrap(),
        ]);

        assert_eq!(refused.status.code(), Some(2), "{detach_flag}");
        assert!(
            standard_error_text(&refused).contains(&format!("unexpected argument '{detach_flag}'")),
            "{detach_flag}: {}",
            standard_error_text(&refused)
        );
    }
    assert_eq!(stub_local_api_server.recorded_tool_calls(), []);
}

/// The compile's standard error reaches the user's terminal: each line on `tatolab`'s stderr,
/// and the running lines alone on its stdout.
#[test]
fn run_prints_each_line_the_compile_wrote_on_its_standard_error() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    let compile_warnings = [
        "tatolab: the cross-floor check found 1 thing binding this app to one floor (Linux or macOS). The app starts anyway.",
        "  processors/effect.py:4: imports `cupy`",
    ];
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "run_stream",
        StubToolAnswer::tool_result(&run_stream_tool_result_text(
            "cam",
            &canonical_project_directory,
            3,
            &compile_warnings,
        )),
    );

    let finished = isolated_machine_directories.run_tatolab(&[
        "run",
        "--dir",
        project_directory.path().to_str().unwrap(),
    ]);

    assert_eq!(
        finished.status.code(),
        Some(0),
        "{}",
        standard_error_text(&finished)
    );
    assert_eq!(
        standard_error_text(&finished),
        format!("{}\n{}\n", compile_warnings[0], compile_warnings[1])
    );
    assert_eq!(
        standard_output_text(&finished),
        format!(
            "cam running as \"main\" (project {0}, stream:main)\n  follow it: tatolab logs -f --dir {0}\n",
            canonical_project_directory.display()
        )
    );
}

#[test]
fn run_sends_a_file_as_the_stream_function() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "run_stream",
        StubToolAnswer::tool_result(&run_stream_tool_result_text(
            "other",
            &canonical_project_directory,
            1,
            &[],
        )),
    );

    printed_by_a_successful(
        &isolated_machine_directories,
        &[
            "run",
            "-f",
            "other.py",
            "--dir",
            project_directory.path().to_str().unwrap(),
        ],
    );

    assert_eq!(
        stub_local_api_server.recorded_arguments_of("run_stream"),
        [json!({
            "project_directory": canonical_project_directory,
            "stream_function": "other.py",
        })]
    );
}

#[test]
fn a_refused_load_prints_the_runtimes_refusal_and_exits_one() {
    let project_directory = tempfile::tempdir().unwrap();
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "run_stream",
        StubToolAnswer::tool_failure(
            "a stream named `cam` is already loaded from /srv/other; load it under another name \
             with --name",
        ),
    );

    let refused = isolated_machine_directories.run_tatolab(&[
        "run",
        "--dir",
        project_directory.path().to_str().unwrap(),
    ]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        "error: run_stream failed: a stream named `cam` is already loaded from /srv/other; load \
         it under another name with --name\n"
    );
    assert_eq!(standard_output_text(&refused), "");
}

/// A stream whose code fails is answered, not refused: `run` prints it failed with its traceback
/// on its standard output, offers no `logs -f` for it, and exits 1.
#[test]
fn run_prints_a_failed_stream_with_its_traceback_and_exits_one() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "run_stream",
        StubToolAnswer::tool_result(&failed_run_stream_tool_result_text(
            "cam",
            &canonical_project_directory,
            "Traceback (most recent call last):\n  File \"stream.py\", line 6, in main\n\
             ValueError: bad wiring",
        )),
    );

    let finished = isolated_machine_directories.run_tatolab(&[
        "run",
        "--dir",
        project_directory.path().to_str().unwrap(),
    ]);

    assert_eq!(
        finished.status.code(),
        Some(1),
        "{}",
        standard_error_text(&finished)
    );
    assert_eq!(standard_error_text(&finished), "");
    assert_eq!(
        standard_output_text(&finished),
        "cam failed: Traceback (most recent call last):\n  File \"stream.py\", line 6, in main\n\
         ValueError: bad wiring\n"
    );
}

#[test]
fn stop_queues_the_streams_stop_and_says_so() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "stop_stream",
        StubToolAnswer::tool_result(&stop_stream_tool_result_text("cam")),
    );

    let printed = printed_by_a_successful(&isolated_machine_directories, &["stop", "cam"]);

    assert_eq!(printed, "R-1 queued: stop of cam\n");
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("stop_stream"),
        [json!({"stream": "cam"})]
    );
}

#[test]
fn start_queues_the_streams_start_and_says_so() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "start_stream",
        StubToolAnswer::tool_result(r#"{"action":"R-2","stream":"cam"}"#),
    );

    let printed = printed_by_a_successful(&isolated_machine_directories, &["start", "cam"]);

    assert_eq!(printed, "R-2 queued: start of cam\n");
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("start_stream"),
        [json!({"stream": "cam"})]
    );
}

#[test]
fn rm_queues_the_removal_of_the_working_directorys_project_or_the_one_dir_names() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    let removal_answer =
        json!({"action": "R-3", "project_directory": canonical_project_directory}).to_string();
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "remove_project",
        StubToolAnswer::tool_result(&removal_answer),
    );

    let in_the_project = run_tatolab_in(
        &isolated_machine_directories,
        project_directory.path(),
        &["rm"],
    );
    let naming_it = isolated_machine_directories.run_tatolab(&[
        "rm",
        "--dir",
        project_directory.path().to_str().unwrap(),
    ]);

    for finished in [&in_the_project, &naming_it] {
        assert_eq!(
            finished.status.code(),
            Some(0),
            "{}",
            standard_error_text(finished)
        );
        assert_eq!(
            standard_output_text(finished),
            format!(
                "R-3 queued: remove of {}\n",
                canonical_project_directory.display()
            )
        );
    }
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("remove_project"),
        [
            json!({"project_directory": canonical_project_directory}),
            json!({"project_directory": canonical_project_directory}),
        ]
    );
}

#[test]
fn streams_prints_the_table_of_what_the_runtime_holds() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "list_streams",
        StubToolAnswer::tool_result(
            &json!({"streams": [
                {"stream": "cam-main-0a1b2c", "display_name": "main", "state": "running",
                 "location": {"project_directory": "/srv/cam", "stream_function": "stream:main"},
                 "node_count": 3},
                {"stream": "mic-listen-9f8e7d", "display_name": "Desk mic", "state": "stopped",
                 "location": {"project_directory": "/srv/mic", "stream_function": "stream:listen"},
                 "node_count": null},
            ]})
            .to_string(),
        ),
    );

    let printed = printed_by_a_successful(&isolated_machine_directories, &["streams"]);

    assert_eq!(
        printed,
        "ID                 NAME      STATE    NODES  PROJECT   FUNCTION\n\
         cam-main-0a1b2c    main      running  3      /srv/cam  stream:main\n\
         mic-listen-9f8e7d  Desk mic  stopped  -      /srv/mic  stream:listen\n"
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("list_streams"),
        [json!({})]
    );
}

#[test]
fn streams_says_so_when_the_runtime_holds_none() {
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "list_streams",
        StubToolAnswer::tool_result(r#"{"streams":[]}"#),
    );

    assert_eq!(
        printed_by_a_successful(&isolated_machine_directories, &["streams"]),
        "No streams in this runtime.\n"
    );
}

#[test]
fn expose_sends_private_public_or_internal_by_its_flag() {
    for (level_flags, wire_level) in [
        (&[][..], "private"),
        (&["--public"][..], "public"),
        (&["--remove"][..], "internal"),
    ] {
        let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
            "expose_port",
            StubToolAnswer::tool_result(
                &json!({
                    "stream": "cam",
                    "node": "effect",
                    "port": "video",
                    "level": wire_level,
                    "recorded": true,
                })
                .to_string(),
            ),
        );

        let printed = printed_by_a_successful(
            &isolated_machine_directories,
            &[&["expose", "cam", "effect", "video"], level_flags].concat(),
        );

        assert_eq!(
            printed,
            format!("cam/effect/video is {wire_level} (recorded; it holds across restarts)\n")
        );
        assert_eq!(
            stub_local_api_server.recorded_arguments_of("expose_port"),
            [json!({"stream": "cam", "node": "effect", "port": "video", "level": wire_level})],
            "{level_flags:?}"
        );
    }
}

#[test]
fn an_exposure_the_runtime_could_not_record_warns_naming_why_and_exits_zero() {
    let (isolated_machine_directories, _stub_local_api_server) = a_runtime_answering(
        "expose_port",
        StubToolAnswer::tool_result(
            &json!({
                "stream": "cam",
                "node": "effect",
                "port": "video",
                "level": "public",
                "recorded": false,
                "not_recorded_because": "the record /state/streams/cam.json cannot be written",
            })
            .to_string(),
        ),
    );

    let finished =
        isolated_machine_directories.run_tatolab(&["expose", "cam", "effect", "video", "--public"]);

    assert_eq!(finished.status.code(), Some(0), "the level changed live");
    assert_eq!(
        standard_output_text(&finished),
        "cam/effect/video is public (live only: not recorded)\n"
    );
    assert_eq!(
        standard_error_text(&finished),
        "warning: cam/effect/video changed live but was not recorded as the owner's ruling, so a \
         restart of the runtime puts back the level it had: the record \
         /state/streams/cam.json cannot be written\n"
    );
}

#[test]
fn a_restriction_the_runtime_refused_for_its_record_exits_one_printing_nothing_on_stdout() {
    let refusal = "the port `effect/video` of the stream `cam` was not restricted to \
                   internal, and is still public: the owner's ruling could not be recorded";
    let (isolated_machine_directories, _stub_local_api_server) =
        a_runtime_answering("expose_port", StubToolAnswer::tool_failure(refusal));

    let refused =
        isolated_machine_directories.run_tatolab(&["expose", "cam", "effect", "video", "--remove"]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        format!("error: expose_port failed: {refusal}\n")
    );
    assert_eq!(standard_output_text(&refused), "");
}

#[test]
fn a_stream_the_runtime_does_not_hold_is_refused_listing_the_streams_before_any_action() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "stop_stream",
        StubToolAnswer::tool_result(&stop_stream_tool_result_text("cam")),
    );

    let refused = isolated_machine_directories.run_tatolab(&["stop", "nope"]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        "error: no stream in this runtime has the id or the display name \"nope\":\n  cam  \
         \"Camera\"\n"
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("stop_stream"),
        Vec::<serde_json::Value>::new()
    );
}

#[test]
fn an_empty_stream_argument_is_refused_and_never_falls_back_to_the_working_directory() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_listing_and_answering(
        vec![listed_stream(
            "hello-main-3f2a9c",
            "main",
            &canonical_project_directory,
        )],
        "stop_stream",
        StubToolAnswer::tool_result(&stop_stream_tool_result_text("hello-main-3f2a9c")),
    );

    let refused = run_tatolab_in(
        &isolated_machine_directories,
        project_directory.path(),
        &["stop", ""],
    );

    assert_eq!(refused.status.code(), Some(1));
    assert!(
        standard_error_text(&refused).contains("an empty stream argument names no stream"),
        "{}",
        standard_error_text(&refused)
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("stop_stream"),
        Vec::<serde_json::Value>::new()
    );
}

#[test]
fn a_display_name_one_stream_holds_reaches_the_tool_as_that_streams_id() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_listing_and_answering(
        vec![
            listed_stream("hello-main-3f2a9c", "Hello camera", Path::new("/srv/hello")),
            listed_stream("desk-main-0a1b2c", "Desk", Path::new("/srv/desk")),
        ],
        "stop_stream",
        StubToolAnswer::tool_result(&stop_stream_tool_result_text("hello-main-3f2a9c")),
    );

    printed_by_a_successful(&isolated_machine_directories, &["stop", "Hello camera"]);

    assert_eq!(
        stub_local_api_server.recorded_arguments_of("stop_stream"),
        [json!({"stream": "hello-main-3f2a9c"})]
    );
}

/// In a project directory one stream was loaded from, every verb naming a stream acts on it
/// without naming it — `expose` with its two positionals.
#[test]
fn every_stream_verb_acts_on_the_stream_loaded_from_the_working_directory_when_none_is_named() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    for (tatolab_arguments, tool_name, tool_answer, sent) in [
        (
            &["stop"][..],
            "stop_stream",
            stop_stream_tool_result_text("hello-main-3f2a9c"),
            json!({"stream": "hello-main-3f2a9c"}),
        ),
        (
            &["start"][..],
            "start_stream",
            json!({"action": "R-2", "stream": "hello-main-3f2a9c"}).to_string(),
            json!({"stream": "hello-main-3f2a9c"}),
        ),
        (
            &["rename", "Hello camera"][..],
            "rename_stream",
            json!({"stream": "hello-main-3f2a9c", "display_name": "Hello camera"}).to_string(),
            json!({"stream": "hello-main-3f2a9c", "display_name": "Hello camera"}),
        ),
        (
            &["expose", "effect", "video"][..],
            "expose_port",
            json!({"stream": "hello-main-3f2a9c", "node": "effect", "port": "video",
                   "level": "private", "recorded": true})
            .to_string(),
            json!({"stream": "hello-main-3f2a9c", "node": "effect", "port": "video",
                   "level": "private"}),
        ),
    ] {
        let (isolated_machine_directories, stub_local_api_server) = a_runtime_listing_and_answering(
            vec![
                listed_stream("hello-main-3f2a9c", "main", &canonical_project_directory),
                listed_stream("desk-main-0a1b2c", "main", Path::new("/srv/desk")),
            ],
            tool_name,
            StubToolAnswer::tool_result(&tool_answer),
        );

        let finished = run_tatolab_in(
            &isolated_machine_directories,
            project_directory.path(),
            tatolab_arguments,
        );

        assert_eq!(
            finished.status.code(),
            Some(0),
            "{tatolab_arguments:?}: {}",
            standard_error_text(&finished)
        );
        assert_eq!(
            stub_local_api_server.recorded_arguments_of(tool_name),
            [sent],
            "{tatolab_arguments:?}"
        );
    }
}

#[test]
fn a_directory_several_streams_were_loaded_from_is_refused_listing_them_and_nothing_is_called() {
    let project_directory = tempfile::tempdir().unwrap();
    let canonical_project_directory = project_directory.path().canonicalize().unwrap();
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_listing_and_answering(
        vec![
            listed_stream("rig-front-0a1b2c", "front", &canonical_project_directory),
            listed_stream("rig-back-9f8e7d", "back", &canonical_project_directory),
        ],
        "stop_stream",
        StubToolAnswer::tool_result(&stop_stream_tool_result_text("rig-front-0a1b2c")),
    );

    let refused = run_tatolab_in(
        &isolated_machine_directories,
        project_directory.path(),
        &["stop"],
    );

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        format!(
            "error: 2 streams in this runtime were loaded from {}; name one by its id:\n  \
             rig-front-0a1b2c  \"front\"\n  rig-back-9f8e7d  \"back\"\n",
            canonical_project_directory.display()
        )
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("stop_stream"),
        Vec::<serde_json::Value>::new()
    );
}

#[test]
fn a_display_name_several_streams_hold_is_refused_listing_their_ids() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_listing_and_answering(
        vec![
            listed_stream("hello-main-3f2a9c", "main", Path::new("/srv/hello")),
            listed_stream("desk-main-0a1b2c", "main", Path::new("/srv/desk")),
        ],
        "rename_stream",
        StubToolAnswer::tool_result("{}"),
    );

    let refused = isolated_machine_directories.run_tatolab(&["rename", "main", "Hello camera"]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        "error: 2 streams are shown as \"main\"; name one by its id:\n  hello-main-3f2a9c  \
         \"main\"\n  desk-main-0a1b2c  \"main\"\n"
    );
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("rename_stream"),
        Vec::<serde_json::Value>::new()
    );
}

#[test]
fn rename_shows_a_named_stream_under_the_display_name_as_given() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "rename_stream",
        StubToolAnswer::tool_result(r#"{"stream":"cam","display_name":"  Desk cam ✨"}"#),
    );

    let printed = printed_by_a_successful(
        &isolated_machine_directories,
        &["rename", "cam", "  Desk cam ✨"],
    );

    assert_eq!(printed, "cam is shown as \"  Desk cam ✨\"\n");
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("rename_stream"),
        [json!({"stream": "cam", "display_name": "  Desk cam ✨"})]
    );
}

#[test]
fn expose_takes_the_stream_as_its_first_of_three_positionals_and_refuses_four() {
    let (isolated_machine_directories, stub_local_api_server) = a_runtime_answering(
        "expose_port",
        StubToolAnswer::tool_result(
            &json!({"stream": "cam", "node": "effect", "port": "video", "level": "public",
                    "recorded": true})
            .to_string(),
        ),
    );

    printed_by_a_successful(
        &isolated_machine_directories,
        &["expose", "Camera", "effect", "video", "--public"],
    );
    let refused =
        isolated_machine_directories.run_tatolab(&["expose", "cam", "effect", "video", "extra"]);

    assert_eq!(
        stub_local_api_server.recorded_arguments_of("expose_port"),
        [json!({"stream": "cam", "node": "effect", "port": "video", "level": "public"})]
    );
    assert_eq!(
        refused.status.code(),
        Some(2),
        "clap refuses a fourth positional"
    );
}
