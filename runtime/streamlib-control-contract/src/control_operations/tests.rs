// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::BTreeSet;

use super::*;

#[test]
fn every_registered_operation_has_a_unique_method_name() {
    let method_names: BTreeSet<&str> = CONTROL_OPERATIONS
        .iter()
        .map(|declaration| declaration.method_name)
        .collect();
    assert_eq!(method_names.len(), CONTROL_OPERATIONS.len());
}

#[test]
fn every_registered_operation_is_documented_versioned_and_served() {
    for declaration in CONTROL_OPERATIONS {
        assert!(
            !declaration.one_line_doc.is_empty(),
            "{}",
            declaration.method_name
        );
        assert!(
            !declaration.one_line_doc.contains('\n'),
            "{}",
            declaration.method_name
        );
        assert!(
            !declaration.agent_usage_note.is_empty(),
            "{}",
            declaration.method_name
        );
        assert!(
            (1..=CONTROL_CONTRACT_VERSION).contains(&declaration.since_contract_version),
            "{}",
            declaration.method_name
        );
        assert!(
            !declaration.exposures.is_empty(),
            "{}",
            declaration.method_name
        );
    }
}

#[test]
fn every_registered_method_name_parses_to_the_call_of_that_name_or_is_refused_for_its_params() {
    for declaration in CONTROL_OPERATIONS {
        match ControlOperationCall::parse(declaration.method_name, None) {
            Ok(call) => assert_eq!(call.method_name(), declaration.method_name),
            Err(ControlOperationCallParseRefusal::InvalidParams { method_name, .. }) => {
                assert_eq!(method_name, declaration.method_name)
            }
            Err(refusal) => panic!("{refusal}"),
        }
    }
}

#[test]
fn an_unknown_method_and_unknown_params_are_refused_by_name() {
    assert_eq!(
        ControlOperationCall::parse("expose_port", None).unwrap_err(),
        ControlOperationCallParseRefusal::NoSuchMethod {
            method_name: "expose_port".to_owned()
        }
    );
    let refusal =
        ControlOperationCall::parse("list_streams", Some(serde_json::json!({ "stream": "x" })))
            .unwrap_err();
    assert!(matches!(
        refusal,
        ControlOperationCallParseRefusal::InvalidParams {
            method_name: "list_streams",
            ..
        }
    ));
}

#[test]
fn a_graph_edit_names_its_stream_by_its_stream_id_or_by_its_stream_assets_uuid() {
    use streamlib_project_repository::GraphEditStream;

    let stream_asset = uuid::Uuid::new_v4();
    let Ok(ControlOperationCall::Transact(transact)) = ControlOperationCall::parse(
        "transact",
        Some(serde_json::json!({ "operations": [
            { "operation": "remove_node", "stream": "main-0a1b2c", "node": "camera" },
            { "operation": "remove_node", "stream": stream_asset, "node": "camera" },
        ] })),
    ) else {
        panic!("a transact naming a stream by either name parses");
    };
    assert_eq!(
        transact
            .operations
            .iter()
            .map(|operation| operation.stream().clone())
            .collect::<Vec<_>>(),
        [
            GraphEditStream::StreamId("main-0a1b2c".to_owned()),
            GraphEditStream::StreamAsset(stream_asset),
        ]
    );
    let Ok(ControlOperationCall::SetNodeInputValue(set)) = ControlOperationCall::parse(
        "set_node_input_value",
        Some(
            serde_json::json!({ "stream": "main-0a1b2c", "node": "camera",
                                 "input": "gain", "value": 1.0 }),
        ),
    ) else {
        panic!("set_node_input_value naming a stream by its stream id parses");
    };
    assert_eq!(
        set.stream,
        GraphEditStream::StreamId("main-0a1b2c".to_owned())
    );
}

#[test]
fn the_audio_capture_device_listing_names_the_default_output_monitor_alias_as_the_runtime_spells_it()
 {
    use streamlib_runtime_client_contract::local_api_wire_contract::DEFAULT_OUTPUT_MONITOR_DEVICE_ID;
    assert!(
        ListAudioCaptureDevices::AGENT_USAGE_NOTE.contains(DEFAULT_OUTPUT_MONITOR_DEVICE_ID),
        "the note names the alias a microphone's device_id takes, in the runtime's spelling"
    );
}
