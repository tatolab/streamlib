// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A start whose WIRE phase cannot open a link's channel: refused by name, and torn down cleanly.

use super::*;
use crate::core::nodes::NodeSpec;
use crate::core::test_support::{
    MockInputOnlyNode, MockOutputOnlyNode, ensure_test_mocks_registered,
};
use crate::core::{InputLinkPortRef, OutputLinkPortRef};

/// A start whose link's channel is already held with room for fewer subscribers than the link
/// needs is refused naming the channel and what the open asked for, and the stream it leaves
/// unloads and ends without crashing the runtime.
///
/// Mental-revert: the refusal was iceoryx2's bare variant, naming no channel; and the teardown
/// after a failed WIRE phase is the one that segfaulted (exit 139) on a buffer-size refusal.
#[test]
#[serial]
fn a_start_whose_channel_cannot_open_is_refused_by_name_and_unloads_cleanly() {
    ensure_test_mocks_registered();
    let project_directory = a_project_directory_this_test_owns();
    let runner = Runner::new().expect("Runner::new");
    let stream = an_empty_stream_loaded_into(&runner, project_directory.path(), "wiring");
    let source = stream
        .add_node(NodeSpec::new(
            MockOutputOnlyNode::node_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("the source is added");
    let reader = stream
        .add_node(NodeSpec::new(
            MockInputOnlyNode::node_class_import_path(),
            serde_json::json!({}),
        ))
        .expect("the reader is added");
    stream
        .connect(
            OutputLinkPortRef::new(source.as_str(), "out1"),
            InputLinkPortRef::new(reader.as_str(), "in1"),
        )
        .expect("the link is queued");
    let channel_service_name = crate::iceoryx2::source_channel_name(source.as_str(), "out1")
        .expect("the source's channel has a name")
        .into_string();
    let _held_with_room_for_one_subscriber = runner
        .iceoryx2_node()
        .open_or_create_service(&channel_service_name, 1, 16)
        .expect("the channel is held before the start");

    let refusal = stream
        .start()
        .expect_err("a start whose link's channel cannot open is refused")
        .to_string();

    assert!(
        refusal.contains(&format!("'{channel_service_name}'")),
        "{refusal}"
    );
    assert!(refusal.contains("room for"), "{refusal}");
    runner
        .unload_stream("wiring")
        .expect("the stream a failed start leaves unloads cleanly");
    assert!(stream.has_ended());
}
