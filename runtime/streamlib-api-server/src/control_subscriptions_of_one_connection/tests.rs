// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A connection following a stream's input values alone is sent `missed`, the change that tells
/// it to read the stream again when its listener lost events or the stream was loaded again, and
/// no other graph change; one following the graph is sent every graph change.
///
/// Mental-revert: sending a graph change only to a connection following the graph drops the
/// `missed` a follower of input values alone needs, so it never learns it missed a value.
#[test]
fn a_follower_of_input_values_alone_is_sent_missed_and_no_other_graph_change() {
    let following = |what| {
        let mut followed = StreamFollowed::default();
        followed.follow(what);
        followed
    };
    let input_values_alone = following(WhatAConnectionFollowsOfAStream::InputValues);
    let graph = following(WhatAConnectionFollowsOfAStream::Graph);
    for change in [
        StreamGraphChangeKindOnTheWire::Edited,
        StreamGraphChangeKindOnTheWire::NodeStateChanged,
        StreamGraphChangeKindOnTheWire::PlaybackStateChanged,
        StreamGraphChangeKindOnTheWire::Missed,
    ] {
        assert!(graph.is_sent_the_graph_change(change), "{change:?}");
        assert_eq!(
            input_values_alone.is_sent_the_graph_change(change),
            change == StreamGraphChangeKindOnTheWire::Missed,
            "{change:?}"
        );
    }
    assert!(
        !StreamFollowed::default().is_sent_the_graph_change(StreamGraphChangeKindOnTheWire::Missed)
    );
}

/// The wire names a node's state as `graph` renders it.
///
/// Mental-revert: a wire spelling other than the engine's `Display` leaves a client reading
/// one name from `graph` and another from `stream_graph_changed`.
#[test]
fn a_node_state_on_the_wire_is_spelled_as_graph_renders_it() {
    for node_state in [
        NodeState::Pending,
        NodeState::Idle,
        NodeState::Running,
        NodeState::Paused,
        NodeState::Stopping,
        NodeState::Stopped,
        NodeState::Error,
        NodeState::Restarting,
    ] {
        assert_eq!(
            serde_json::to_value(node_state_on_the_wire(node_state))
                .expect("a node state serializes"),
            serde_json::json!(node_state.to_string())
        );
    }
}
