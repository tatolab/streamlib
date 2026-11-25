// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An Asset input of a node whose config class held its asset as a bare UUID string reads that
//! string, saved in a document or set by an older edit, as the asset it names, and the empty
//! string as no asset; a value in the form an Asset takes now, and every other input's value,
//! is read as saved.

use serde_json::{Value, json};
use uuid::Uuid;

use super::stream_graph_operation_under_the_current_port_names::the_operation_under_the_current_port_names;
use crate::core::descriptors::{
    PinFlow, PinType, PortFamily, PortFamilyDeclaration, ProjectAssetMedia, ResolvedPin,
};
use crate::core::graph::{GraphNodePorts, PortInfo};
use streamlib_project_repository::StreamGraphOperation;

const PICTURE: Uuid = Uuid::from_u128(0x4a55_e700_0000_4000_8000_0000_0000_0001);
const ASSET: &str = "0f0e0d0c-0b0a-4909-8807-060504030201";

/// The ports of a picture node: its Asset input `asset`, once a config key holding a UUID
/// string, and its String input `caption`.
fn ports_of_a_picture_node() -> GraphNodePorts {
    let typed = |name: &str, pin_type: PinType| PortInfo {
        pin: Some(ResolvedPin {
            pin_type,
            flow: PinFlow::Value,
        }),
        ..PortInfo::declaring_only_a_family(
            name,
            "",
            PortFamilyDeclaration::of_family(PortFamily::Records),
        )
    };
    GraphNodePorts {
        inputs: vec![
            typed(
                "asset",
                PinType::Asset {
                    media: vec![ProjectAssetMedia::Image],
                },
            ),
            typed("caption", PinType::String {}),
        ],
        outputs: Vec::new(),
    }
}

fn read(values: Value) -> Value {
    let Value::Object(values) = values else {
        panic!("the values are an object");
    };
    Value::Object(ports_of_a_picture_node().input_values_under_the_current_names(values))
}

/// Mental-revert: holding each value as saved leaves the bare string on `asset`, which the
/// check of a stored Asset refuses, so the document would not load.
#[test]
fn a_document_saved_with_an_asset_as_its_uuid_reads_it_as_the_asset_and_the_empty_string_as_none() {
    assert_eq!(
        read(json!({ "asset": ASSET, "caption": "a string stays" })),
        json!({ "asset": { "asset": ASSET }, "caption": "a string stays" })
    );
    assert_eq!(
        read(json!({ "asset": "", "caption": "" })),
        json!({ "caption": "" })
    );
    let held_now = json!({ "asset": { "asset": ASSET, "from": ASSET } });
    assert_eq!(read(held_now.clone()), held_now);
    assert_eq!(read(json!({})), json!({}));
}

/// The set an older edit recorded, its value then and after each a UUID string or the empty
/// string, sets and restores the asset in the form it takes now; an edit of a member of an
/// input, and an edit of another input, are as recorded.
///
/// Mental-revert: leaving `from` and `to` as recorded hands a bare string to the set, which
/// refuses it, so undoing an older edit of the asset fails.
#[test]
fn an_older_edit_setting_an_asset_by_its_uuid_sets_the_asset_and_undoes_to_none() {
    let under_the_current_form = |operation| {
        the_operation_under_the_current_port_names(operation, |node| {
            (node == PICTURE).then(ports_of_a_picture_node)
        })
    };
    let set = |input: &str, from: Option<Value>, to: Option<Value>| {
        StreamGraphOperation::SetNodeInputValue {
            node: PICTURE,
            input: input.to_owned(),
            member: None,
            from,
            to,
        }
    };
    assert_eq!(
        under_the_current_form(set("asset", Some(json!("")), Some(json!(ASSET)))),
        set("asset", None, Some(json!({ "asset": ASSET })))
    );
    let caption = set("caption", Some(json!("")), Some(json!(ASSET)));
    assert_eq!(under_the_current_form(caption.clone()), caption);
    let of_a_member = StreamGraphOperation::SetNodeInputValue {
        node: PICTURE,
        input: "asset".to_owned(),
        member: Some("asset".to_owned()),
        from: Some(json!(ASSET)),
        to: Some(json!(ASSET)),
    };
    assert_eq!(under_the_current_form(of_a_member.clone()), of_a_member);
}
