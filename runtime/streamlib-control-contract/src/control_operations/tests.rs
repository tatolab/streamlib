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
