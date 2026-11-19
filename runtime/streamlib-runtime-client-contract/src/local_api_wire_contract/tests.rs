// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use serde::Serialize;

use super::*;

#[test]
fn a_runtime_description_names_its_log_folder_or_null_when_it_keeps_none() {
    let described = |runtime_log_directory: Option<&str>| DescribeRuntimeToolResult {
        version: "0.55.1".to_owned(),
        build_id: "0.55.1+0123abc.9f8e7d".to_owned(),
        up_for_ms: 266_400_000,
        runtime_log_directory: runtime_log_directory.map(PathBuf::from),
    };
    for (description, expected_wire_text) in [
        (
            described(Some("/home/u/.local/state/tatolab/logs")),
            r#"{"version":"0.55.1","build_id":"0.55.1+0123abc.9f8e7d","up_for_ms":266400000,"runtime_log_directory":"/home/u/.local/state/tatolab/logs"}"#,
        ),
        (
            described(None),
            r#"{"version":"0.55.1","build_id":"0.55.1+0123abc.9f8e7d","up_for_ms":266400000,"runtime_log_directory":null}"#,
        ),
    ] {
        let wire_text = serde_json::to_string(&description).unwrap();
        assert_eq!(wire_text, expected_wire_text);
        assert_eq!(
            serde_json::from_str::<DescribeRuntimeToolResult>(&wire_text).unwrap(),
            description
        );
    }
}

#[test]
fn the_machine_runtime_lock_holders_answer_is_its_pid_alone() {
    let answer = MachineRuntimeLockHolderPidAnswer { pid: 4242 };
    let wire_text = serde_json::to_string(&answer).unwrap();
    assert_eq!(wire_text, r#"{"pid":4242}"#);
    assert_eq!(
        serde_json::from_str::<MachineRuntimeLockHolderPidAnswer>(&wire_text).unwrap(),
        answer
    );
}

/// Each stream-action result in the order and spelling the runtime writes its keys.
#[test]
fn every_stream_action_result_serializes_its_keys_in_wire_order() {
    fn wire_text(result: &impl Serialize) -> String {
        serde_json::to_string(result).unwrap()
    }

    assert_eq!(
        wire_text(&RemoveProjectToolResult {
            action: "R-9".to_owned(),
            project_directory: PathBuf::from("/srv/project"),
        }),
        r#"{"action":"R-9","project_directory":"/srv/project"}"#
    );
    assert_eq!(
        wire_text(&ListStreamsToolResult {
            streams: vec![
                ListStreamsToolResultStream {
                    stream: "cam-main-0a1b2c".to_owned(),
                    display_name: "Desk camera".to_owned(),
                    location: Some(StreamLocationOnTheWire {
                        project_directory: PathBuf::from("/srv/cam"),
                        project: uuid::Uuid::from_u128(1),
                        stream_asset: uuid::Uuid::from_u128(2),
                    }),
                    state: ListedStreamState::Running,
                    condition: Some(StreamConditionOnTheWire {
                        load: StreamLoadOnTheWire::Loaded,
                        playback: StreamPlaybackOnTheWire::Playing,
                        health: StreamHealthOnTheWire::Degraded,
                        diagnostics: vec![DiagnosticOnTheWire::of(
                            DiagnosticKindOnTheWire::NodeNotReady,
                            DiagnosticPlaceOnTheWire::Node {
                                node_id: "n-1".to_owned(),
                                node_name: "camera".to_owned(),
                            },
                            "waiting for its first frame".to_owned(),
                        )],
                    }),
                    node_count: Some(4),
                    failed_because: None,
                    capture_devices_whose_access_was_never_asked_for: vec![
                        CaptureDeviceOnTheWire::Camera,
                        CaptureDeviceOnTheWire::Microphone,
                    ],
                    action: None,
                    step: None,
                },
                ListStreamsToolResultStream {
                    stream: "main".to_owned(),
                    display_name: "main".to_owned(),
                    location: None,
                    state: ListedStreamState::Failed,
                    condition: Some(StreamConditionOnTheWire {
                        load: StreamLoadOnTheWire::NotLoaded,
                        playback: StreamPlaybackOnTheWire::Stopped,
                        health: StreamHealthOnTheWire::Failed,
                        diagnostics: vec![DiagnosticOnTheWire::of(
                            DiagnosticKindOnTheWire::RecordedByAnEarlierRuntime,
                            DiagnosticPlaceOnTheWire::Stream,
                            "an earlier runtime recorded it".to_owned(),
                        )],
                    }),
                    node_count: None,
                    failed_because: Some("an earlier runtime recorded it".to_owned()),
                    capture_devices_whose_access_was_never_asked_for: Vec::new(),
                    action: None,
                    step: None,
                },
                ListStreamsToolResultStream {
                    stream: "desk-0a1b2c".to_owned(),
                    display_name: "desk".to_owned(),
                    location: Some(StreamLocationOnTheWire {
                        project_directory: PathBuf::from("/srv/desk"),
                        project: uuid::Uuid::from_u128(3),
                        stream_asset: uuid::Uuid::from_u128(4),
                    }),
                    state: ListedStreamState::Preparing,
                    condition: Some(StreamConditionOnTheWire {
                        load: StreamLoadOnTheWire::Loading,
                        playback: StreamPlaybackOnTheWire::Stopped,
                        health: StreamHealthOnTheWire::Healthy,
                        diagnostics: Vec::new(),
                    }),
                    node_count: None,
                    failed_because: None,
                    capture_devices_whose_access_was_never_asked_for: Vec::new(),
                    action: Some("Rabc-3".to_owned()),
                    step: Some(StreamActionStepOnTheWire::MakingTheVenv),
                },
            ],
        }),
        r#"{"streams":[{"stream":"cam-main-0a1b2c","display_name":"Desk camera","location":{"project_directory":"/srv/cam","project":"00000000-0000-0000-0000-000000000001","stream_asset":"00000000-0000-0000-0000-000000000002"},"state":"running","condition":{"load":"loaded","playback":"playing","health":"degraded","diagnostics":[{"kind":"node_not_ready","place":{"on":"node","node_id":"n-1","node_name":"camera"},"words":"waiting for its first frame","recovery":"wait_for_the_node"}]},"node_count":4,"failed_because":null,"capture_devices_whose_access_was_never_asked_for":["camera","microphone"],"action":null,"step":null},{"stream":"main","display_name":"main","location":null,"state":"failed","condition":{"load":"not_loaded","playback":"stopped","health":"failed","diagnostics":[{"kind":"recorded_by_an_earlier_runtime","place":{"on":"stream"},"words":"an earlier runtime recorded it","recovery":"remove_the_stream"}]},"node_count":null,"failed_because":"an earlier runtime recorded it","capture_devices_whose_access_was_never_asked_for":[],"action":null,"step":null},{"stream":"desk-0a1b2c","display_name":"desk","location":{"project_directory":"/srv/desk","project":"00000000-0000-0000-0000-000000000003","stream_asset":"00000000-0000-0000-0000-000000000004"},"state":"preparing","condition":{"load":"loading","playback":"stopped","health":"healthy","diagnostics":[]},"node_count":null,"failed_because":null,"capture_devices_whose_access_was_never_asked_for":[],"action":"Rabc-3","step":"making_the_venv"}]}"#
    );
    assert_eq!(
        wire_text(&ProjectStreamActionQueuedToolResult {
            action: "Rabc-3".to_owned(),
            project_directory: PathBuf::from("/srv/desk"),
        }),
        r#"{"action":"Rabc-3","project_directory":"/srv/desk"}"#
    );
    assert_eq!(
        wire_text(&CreateProjectToolResult {
            action: "Rabc-4".to_owned(),
            stream: "brisk-otter-main-0a1b2c".to_owned(),
            project_directory: PathBuf::from("/home/me/streams/brisk-otter"),
        }),
        r#"{"action":"Rabc-4","stream":"brisk-otter-main-0a1b2c","project_directory":"/home/me/streams/brisk-otter"}"#
    );
    assert_eq!(
        wire_text(&ListSamplesToolResult {
            samples: vec![ListSamplesToolResultSample {
                name: "camera-halftone".to_owned(),
                title: "Camera halftone".to_owned(),
                summary: "Your webcam printed as newsprint.".to_owned(),
            }],
        }),
        r#"{"samples":[{"name":"camera-halftone","title":"Camera halftone","summary":"Your webcam printed as newsprint."}]}"#
    );
    assert_eq!(
        wire_text(&DescribeStreamActionToolResult {
            action: "Rabc-3".to_owned(),
            project_directory: PathBuf::from("/srv/desk"),
            kind: StreamActionKindOnTheWire::Load,
            state: StreamActionStateOnTheWire::InProgress,
            step: Some(StreamActionStepOnTheWire::Starting),
            venv_sync: None,
            streams: vec![DescribeStreamActionToolResultStream {
                stream: "desk-main-0a1b2c".to_owned(),
                display_name: "main".to_owned(),
                state: None,
            }],
            failed_because: None,
        }),
        r#"{"action":"Rabc-3","project_directory":"/srv/desk","kind":"load","state":"in_progress","step":"starting","venv_sync":null,"streams":[{"stream":"desk-main-0a1b2c","display_name":"main","state":null}],"failed_because":null}"#
    );
    assert_eq!(
        wire_text(&ProjectVenvSyncOnTheWire {
            phase: ProjectVenvSyncPhaseOnTheWire::Downloading,
            packages_resolved: Some(8),
            packages_prepared: None,
            packages_installed: None,
            downloads_started: 2,
            downloads_done: 1,
            packages: vec![
                ProjectVenvSyncPackageOnTheWire {
                    name: "numpy".to_owned(),
                    version: None,
                    kind: ProjectVenvSyncPackageKindOnTheWire::Distribution,
                    state: ProjectVenvSyncPackageStateOnTheWire::Downloading,
                    download_size_bytes: Some(16_672_358),
                },
                ProjectVenvSyncPackageOnTheWire {
                    name: "cpython-3.12.15-macos-aarch64-none".to_owned(),
                    version: None,
                    kind: ProjectVenvSyncPackageKindOnTheWire::PythonInterpreter,
                    state: ProjectVenvSyncPackageStateOnTheWire::Downloaded,
                    download_size_bytes: Some(34_288_435),
                },
            ],
        }),
        r#"{"phase":"downloading","packages_resolved":8,"packages_prepared":null,"packages_installed":null,"downloads_started":2,"downloads_done":1,"packages":[{"name":"numpy","version":null,"kind":"distribution","state":"downloading","download_size_bytes":16672358},{"name":"cpython-3.12.15-macos-aarch64-none","version":null,"kind":"python_interpreter","state":"downloaded","download_size_bytes":34288435}]}"#
    );
    assert_eq!(
        wire_text(&CancelStreamActionToolResult {
            action: "Rabc-3".to_owned(),
            state: StreamActionStateOnTheWire::Cancelled,
        }),
        r#"{"action":"Rabc-3","state":"cancelled"}"#
    );
    assert_eq!(
        wire_text(&PausePlaybackToolResult {
            project_directory: PathBuf::from("/srv/project"),
            streams: vec!["project-main-1a2b3c".to_owned()],
        }),
        r#"{"project_directory":"/srv/project","streams":["project-main-1a2b3c"]}"#
    );
    assert_eq!(
        wire_text(&ResumePlaybackToolResult {
            project_directory: PathBuf::from("/srv/project"),
            streams: vec!["project-main-1a2b3c".to_owned()],
        }),
        r#"{"project_directory":"/srv/project","streams":["project-main-1a2b3c"]}"#
    );
    assert_eq!(
        wire_text(&LogsToolResult {
            project_directory: PathBuf::from("/srv/project"),
            records: vec![serde_json::json!({"message": "hello"})],
            next_after: "2:17".to_owned(),
            records_no_longer_held: false,
        }),
        r#"{"project_directory":"/srv/project","records":[{"message":"hello"}],"next_after":"2:17","records_no_longer_held":false}"#
    );
}

/// The `Display` spelling a client prints is the spelling the wire carries.
#[test]
fn each_state_displays_as_the_wire_spells_it() {
    for state in [
        ListedStreamState::Running,
        ListedStreamState::Stopped,
        ListedStreamState::Paused,
        ListedStreamState::Closed,
        ListedStreamState::Failed,
        ListedStreamState::Preparing,
        ListedStreamState::Stopping,
    ] {
        assert_eq!(
            serde_json::to_value(state).unwrap(),
            serde_json::Value::String(state.to_string())
        );
    }
}

/// Asset results carry the UUID, kind, path and content hash under the names clients read.
#[test]
fn an_asset_transaction_result_spells_its_fields_as_the_wire_carries_them() {
    let result = AssetTransactionToolResult {
        asset: ProjectAssetOnTheWire {
            asset: "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11".to_owned(),
            kind: "media".to_owned(),
            path: "media/intro.mp4".to_owned(),
            content_sha256: "ab".repeat(32),
            content_byte_len: 12,
            import_settings: serde_json::Map::new(),
        },
        transaction: "0b6f4e1d-3c55-4b0e-8d1e-6e2f0f9b7c21".to_owned(),
        commit: "c".repeat(40),
        action: None,
    };
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::json!({
            "asset": {
                "asset": "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11",
                "kind": "media",
                "path": "media/intro.mp4",
                "content_sha256": "ab".repeat(32),
                "content_byte_len": 12,
                "import_settings": {},
            },
            "transaction": "0b6f4e1d-3c55-4b0e-8d1e-6e2f0f9b7c21",
            "commit": "c".repeat(40),
        })
    );
}

/// A source's origin rides flat beside its listing, under `source`.
#[test]
fn a_node_type_source_spells_its_origin_flat_and_keeps_a_null_default() {
    let wire = serde_json::json!({
        "source": "dependency",
        "project": "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11",
        "folder": "gripper",
        "version": "ab".repeat(32),
        "project_name": "Gripper",
        "node_types": [{
            "type": "nodes.gripper:Gripper",
            "kind": "function",
            "description": "",
            "category": null,
            "inputs": [{"name": "grip", "family": "channels", "description": "", "annotation": "float | None", "pin_default": null}],
            "outputs": null,
            "path": "nodes/gripper.py",
            "line": 4,
        }],
        "graph_generators": [],
        "unreadable_modules": [],
    });

    let source: NodeTypeSourceReadWithoutRunningIt = serde_json::from_value(wire.clone()).unwrap();

    assert_eq!(
        source.origin,
        NodeTypeSourceOrigin::Dependency {
            project: "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11".to_owned(),
            folder: "gripper".to_owned(),
            version: "ab".repeat(32),
            project_name: Some("Gripper".to_owned()),
        }
    );
    assert_eq!(serde_json::to_value(&source).unwrap(), wire);
}

/// The project's interpreter lists an import's source with no project name, which the runtime
/// names from the import's project file.
#[test]
fn an_imports_source_listed_with_no_project_name_reads_with_none() {
    let source: NodeTypeSourceReadWithoutRunningIt = serde_json::from_value(serde_json::json!({
        "source": "dependency",
        "project": "6f1c2a52-8f0e-4d55-9a57-1f1b8f6f0a11",
        "folder": "gripper",
        "version": "ab".repeat(32),
        "node_types": [],
        "graph_generators": [],
        "unreadable_modules": [],
    }))
    .unwrap();

    assert!(matches!(
        source.origin,
        NodeTypeSourceOrigin::Dependency {
            project_name: None,
            ..
        }
    ));
}

#[test]
fn a_refusal_carries_its_code_and_leaves_out_every_place_it_does_not_name() {
    let refusal = ProjectActionRefusalOnTheWire::of_the_action(
        "transact",
        ControlOperationRefusalCode::ParamsDoNotMatchSchema,
        "missing field `stream`",
    );

    assert_eq!(
        serde_json::to_value(&refusal).unwrap(),
        serde_json::json!({
            "refusal_code": "params_do_not_match_schema",
            "action": "transact",
            "runtime_words": "missing field `stream`",
        })
    );
}

#[test]
fn a_refusal_code_a_client_does_not_know_reads_as_refused() {
    let refusal: ProjectActionRefusalOnTheWire = serde_json::from_value(serde_json::json!({
        "refusal_code": "a_code_added_after_this_client",
        "action": "transact",
        "runtime_words": "no",
    }))
    .unwrap();

    assert_eq!(refusal.refusal_code, ControlOperationRefusalCode::Refused);
}

#[test]
fn a_refusal_from_a_runtime_that_sends_no_code_reads_as_refused() {
    let refusal: ProjectActionRefusalOnTheWire = serde_json::from_value(serde_json::json!({
        "action": "transact",
        "refused_operation_index": 1,
        "runtime_words": "no",
    }))
    .unwrap();

    assert_eq!(refusal.refusal_code, ControlOperationRefusalCode::Refused);
    assert_eq!(refusal.refused_operation_index, Some(1));
}

#[test]
fn a_refusal_names_a_file_place_and_a_suggested_call_as_the_wire_spells_them() {
    let refusal = ProjectActionRefusalOnTheWire {
        refused_params_pointer: Some("/operations/0/stream".to_owned()),
        refused_file: Some(ProjectFilePlaceOnTheWire {
            path: "nodes/blur.py".to_owned(),
            line: Some(4),
            column: None,
        }),
        suggested_call: Some(SuggestedOperationCall {
            operation: "read_project_file".to_owned(),
            params: serde_json::Map::from_iter([(
                "path".to_owned(),
                serde_json::Value::from("nodes/blur.py"),
            )]),
        }),
        ..ProjectActionRefusalOnTheWire::of_the_action(
            "write_project_file",
            ControlOperationRefusalCode::Refused,
            "changed since it was read",
        )
    };

    let on_the_wire = serde_json::to_value(&refusal).unwrap();

    assert_eq!(
        on_the_wire["refused_params_pointer"],
        "/operations/0/stream"
    );
    assert_eq!(
        on_the_wire["refused_file"],
        serde_json::json!({"path": "nodes/blur.py", "line": 4})
    );
    assert_eq!(
        on_the_wire["suggested_call"],
        serde_json::json!({"operation": "read_project_file", "params": {"path": "nodes/blur.py"}})
    );
    assert_eq!(
        serde_json::from_value::<ProjectActionRefusalOnTheWire>(on_the_wire).unwrap(),
        refusal
    );
}

#[test]
fn a_graph_answer_reads_as_one_stream_or_every_stream_and_writes_back_as_it_was_read() {
    let one_stream = serde_json::json!({
        "stream": "rig-main-7bd4ff",
        "nodes": [{
            "id": "1c1c1c1c-1c1c-4c1c-8c1c-1c1c1c1c1c1c",
            "type": "tatolab.stream:Mixer",
            "kind": "process",
            "name": "mixer",
            "config": {"frame_width": 640},
            "config_checksum": 13757715674185714284_u64,
            "ports": {
                "inputs": [
                    {"name": "gain", "description": "", "port_kind": "data",
                     "delivery_profile": "newest", "pin_type": {"type": "float"},
                     "flow": "value", "value": 0.25, "value_comes_from": "live"},
                    {"name": "trim", "description": "", "port_kind": "data",
                     "delivery_profile": "newest", "value": 0.5, "value_comes_from": "saved"},
                    {"name": "pan", "description": "", "port_kind": "data",
                     "delivery_profile": "newest", "pin_default": 0.0, "value": 0.0,
                     "value_comes_from": "default"},
                    {"name": "level", "description": "", "port_kind": "data",
                     "delivery_profile": "newest", "value_comes_from": "wire"},
                    {"name": "bias", "description": "", "port_kind": "data",
                     "delivery_profile": "newest", "pin_default": null, "value": null},
                    {"name": "video", "description": "", "port_kind": "data",
                     "delivery_profile": "newest"},
                ],
                "outputs": [
                    {"name": "out", "description": "", "port_kind": "data",
                     "delivery_profile": null, "family": {"family": "channels"}},
                ],
            },
            "input_values": {"frame_width": 640, "trim": 0.5},
            "pins": {"trim": 0.5},
            "components": {
                "state": "Running",
                "failed_because": null,
                "process_raising_because": null,
                "not_ready_because": null,
                "metrics": {"frames_dropped": 2, "dropped_bags_by_link": {"L1": 2},
                            "refused_bags_by_output_port": {},
                            "written_bags_by_output_port": {"out": 40}},
                "host": {"inside_the_read_of": ["reader"]},
            },
        }],
        "links": [{"id": "L1", "source": {"node": "a", "port": "out"},
                   "target": {"node": "mixer", "port": "level"}, "capacity": 4,
                   "state": "wired", "free_conversion": true,
                   "warnings": [{"warning": "vector_read_as_a_color"}],
                   "components": {"state": "Wired"}}],
        "clock": {"mode": "stepped", "step_ns": 1000, "pace": null, "stop_after_steps": 3},
        "machine": "rig",
    });
    let every_stream = serde_json::json!({ "machine": "rig", "streams": [one_stream.clone()] });

    let read_one: GraphToolResult = serde_json::from_value(one_stream.clone()).unwrap();
    let GraphToolResult::OneStream(graph) = &read_one else {
        panic!("one stream's graph reads as one stream: {read_one:?}");
    };
    let inputs = &graph.nodes[0].ports.inputs;
    let comes_from: Vec<Option<InputValueComesFromOnTheWire>> = inputs
        .iter()
        .map(|input| {
            input
                .value_read
                .as_ref()
                .and_then(|read| read.value_comes_from)
        })
        .collect();
    assert_eq!(
        comes_from,
        [
            Some(InputValueComesFromOnTheWire::Live),
            Some(InputValueComesFromOnTheWire::Saved),
            Some(InputValueComesFromOnTheWire::Default),
            Some(InputValueComesFromOnTheWire::Wire),
            None,
            None,
        ]
    );
    assert_eq!(
        inputs[4].value_read,
        Some(InputValueReadOnTheWire {
            value: Some(serde_json::Value::Null),
            value_comes_from: None,
        }),
        "an input reading nothing reads a null value"
    );
    assert_eq!(
        (
            &inputs[5].value_read,
            &graph.nodes[0].ports.outputs[0].value_read
        ),
        (&None, &None),
        "an input with neither key, and an output, read no value"
    );
    assert_eq!(
        inputs
            .iter()
            .map(|input| input.pin_default.clone())
            .collect::<Vec<_>>(),
        [
            None,
            None,
            Some(serde_json::json!(0.0)),
            None,
            Some(serde_json::Value::Null),
            None
        ],
        "a declared null default reads apart from no default"
    );
    assert!(inputs[0].pin.is_some() && inputs[1].pin.is_none());
    let link = &graph.links[0];
    assert!(
        link.marks.free_conversion,
        "a link that converts for free says so: {link:?}"
    );
    assert_eq!(
        link.marks.warnings,
        [PinLinkWarningOnTheWire::VectorReadAsAColor],
        "a warned link carries its warnings"
    );
    assert_eq!(
        graph.nodes[0].components.state,
        Some(NodeStateOnTheWire::Running)
    );
    assert_eq!(serde_json::to_value(&read_one).unwrap(), one_stream);

    let read_every: GraphToolResult = serde_json::from_value(every_stream.clone()).unwrap();
    assert!(
        matches!(read_every, GraphToolResult::EveryStream(_)),
        "every stream's graph reads as every stream: {read_every:?}"
    );
    assert_eq!(serde_json::to_value(&read_every).unwrap(), every_stream);

    let mut with_an_edit_not_live = one_stream.clone();
    let mut node_not_live = one_stream["nodes"][0].clone();
    node_not_live["id"] = serde_json::json!("2d2d2d2d-2d2d-4d2d-8d2d-2d2d2d2d2d2d");
    node_not_live["name"] = serde_json::json!("second_mixer");
    node_not_live["live"] = serde_json::json!(false);
    let mut link_not_live = one_stream["links"][0].clone();
    link_not_live["target"]["node"] = serde_json::json!("second_mixer");
    link_not_live["live"] = serde_json::json!(false);
    with_an_edit_not_live["nodes"]
        .as_array_mut()
        .unwrap()
        .push(node_not_live);
    with_an_edit_not_live["links"]
        .as_array_mut()
        .unwrap()
        .push(link_not_live);
    let read_not_live: GraphToolResult =
        serde_json::from_value(with_an_edit_not_live.clone()).unwrap();
    let GraphToolResult::OneStream(graph) = &read_not_live else {
        panic!("one stream's graph reads as one stream: {read_not_live:?}");
    };
    assert_eq!(
        (graph.nodes[0].live, graph.nodes[1].live),
        (None, Some(false)),
        "a part not live yet says so, and a live one says nothing"
    );
    assert_eq!(
        (graph.links[0].live, graph.links[1].live),
        (None, Some(false))
    );
    assert_eq!(
        serde_json::to_value(&read_not_live).unwrap(),
        with_an_edit_not_live
    );
}

#[test]
fn an_edit_not_live_is_spelled_as_the_wire_carries_it_and_recovered_by_a_restart() {
    let diagnostic = DiagnosticOnTheWire::of(
        DiagnosticKindOnTheWire::EditNotLive,
        DiagnosticPlaceOnTheWire::Stream,
        "its graph document holds an edit its running graph did not take".to_owned(),
    );
    assert_eq!(
        serde_json::to_value(&diagnostic).unwrap(),
        serde_json::json!({
            "kind": "edit_not_live",
            "place": { "on": "stream" },
            "words": "its graph document holds an edit its running graph did not take",
            "recovery": "restart_the_playback",
        })
    );
}

#[test]
fn each_node_kind_and_language_is_named_as_it_is_written() {
    for kind in GraphNodeKindOnTheWire::EVERY_KIND {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.name());
    }
    for language in FunctionNodeLanguageOnTheWire::EVERY_LANGUAGE {
        assert_eq!(serde_json::to_value(language).unwrap(), language.name());
    }
}

#[test]
fn a_condition_value_this_client_does_not_know_reads_as_its_sets_fallback() {
    let read: StreamConditionOnTheWire = serde_json::from_value(serde_json::json!({
        "load": "resident",
        "playback": "rewinding",
        "health": "wobbly",
        "diagnostics": [{
            "kind": "gpu_lost",
            "place": { "on": "stream" },
            "words": "the GPU was lost",
            "recovery": "replace_the_gpu",
        }],
    }))
    .unwrap();
    assert_eq!(read.load, StreamLoadOnTheWire::NotLoaded);
    assert_eq!(read.playback, StreamPlaybackOnTheWire::Stopped);
    assert_eq!(read.health, StreamHealthOnTheWire::Failed);
    assert_eq!(read.diagnostics[0].kind, DiagnosticKindOnTheWire::Other);
    assert_eq!(read.diagnostics[0].words, "the GPU was lost");
    assert_eq!(
        read.diagnostics[0].recovery,
        DiagnosticRecoveryOnTheWire::RetryTheLoad
    );
    assert_eq!(
        serde_json::from_value::<NodeHealthOnTheWire>(serde_json::json!("flickering")).unwrap(),
        NodeHealthOnTheWire::Failed
    );
}

#[test]
fn a_listing_from_a_runtime_before_conditions_reads_with_none() {
    let listed: ListStreamsToolResultStream = serde_json::from_value(serde_json::json!({
        "stream": "main", "display_name": "main", "location": null, "state": "running",
        "node_count": 1, "failed_because": null,
        "capture_devices_whose_access_was_never_asked_for": [], "action": null, "step": null,
    }))
    .unwrap();
    assert_eq!(listed.condition, None);
}
