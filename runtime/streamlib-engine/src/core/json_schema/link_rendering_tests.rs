// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::graph::{
    GraphEdgeWithComponents, InputLinkPortRef, Link, LinkState, LinkStateComponent,
    OutputLinkPortRef,
};

/// The field is the state a link was created in; wiring records its
/// outcome on a component. Rendering reads the component first, so a
/// `graph` read after a connect says `wired` at the top level rather than
/// a permanent `pending` beside a `components.state` that disagrees.
#[test]
fn a_wired_links_top_level_state_comes_from_its_wiring_component() {
    let mut link = Link::between(
        OutputLinkPortRef::new("Psrc", "out1"),
        InputLinkPortRef::new("Pdst", "in1"),
    );
    let rendered = |link: &Link| {
        serde_json::to_value(LinkOutput::of_a_link(
            link,
            &crate::core::json_schema::NodeNamesByProcessorId::holding_no_node(),
        ))
        .unwrap()
    };
    assert_eq!(rendered(&link)["state"], "pending");

    link.insert(LinkStateComponent(LinkState::Wired));
    assert_eq!(rendered(&link)["state"], "wired");
}

/// The two places a link renders its state have to say the same thing.
///
/// Fail-without-fix: render `components` untouched and a link its helper
/// opened comes back `{"state": "wired", "components": {"state":
/// "Pending"}}` — on the surface the MCP instructions tell an agent to
/// read.
#[test]
fn the_rendered_state_and_the_components_map_never_disagree() {
    use crate::core::graph::OutOfProcessLinkWireRepliesComponent;
    use crate::core::processors::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};

    let mut link = Link::between(
        OutputLinkPortRef::new("Psrc", "out1"),
        InputLinkPortRef::new("Pdst", "in1"),
    );
    link.insert(LinkStateComponent(LinkState::Pending));
    let helpers_answer = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
    link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![
        std::sync::Arc::clone(&helpers_answer),
    ]));
    let rendered = |link: &Link| {
        serde_json::to_value(LinkOutput::of_a_link(
            link,
            &crate::core::json_schema::NodeNamesByProcessorId::holding_no_node(),
        ))
        .unwrap()
    };

    assert_eq!(rendered(&link)["state"], "pending");
    assert_eq!(rendered(&link)["components"]["state"], "Pending");

    helpers_answer.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    assert_eq!(rendered(&link)["state"], "wired");
    assert_eq!(rendered(&link)["components"]["state"], "Wired");
}

/// The same agreement on the arm that carries a reason.
#[test]
fn a_refused_links_reason_rides_beside_a_state_both_renderings_agree_on() {
    use crate::core::graph::OutOfProcessLinkWireRepliesComponent;
    use crate::core::processors::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};

    let mut link = Link::between(
        OutputLinkPortRef::new("Psrc", "out1"),
        InputLinkPortRef::new("Pdst", "in1"),
    );
    link.insert(LinkStateComponent(LinkState::Pending));
    let helpers_answer = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
    helpers_answer.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
        reason: "BufferSizeExceedsMaxSupportedBufferSizeOfService".to_string(),
    });
    link.insert_component_without_rendering_it(OutOfProcessLinkWireRepliesComponent(vec![
        helpers_answer,
    ]));

    let rendered = serde_json::to_value(LinkOutput::of_a_link(
        &link,
        &crate::core::json_schema::NodeNamesByProcessorId::holding_no_node(),
    ))
    .unwrap();
    assert_eq!(rendered["state"], "error");
    assert_eq!(rendered["components"]["state"], "Error");
    assert_eq!(
        rendered["error_reason"],
        "BufferSizeExceedsMaxSupportedBufferSizeOfService"
    );
}

/// A link nothing out of process is answering for carries no reason key at
/// all, so a reader that finds one knows the link is refused.
#[test]
fn a_link_that_was_never_refused_renders_no_reason_key() {
    let mut link = Link::between(
        OutputLinkPortRef::new("Psrc", "out1"),
        InputLinkPortRef::new("Pdst", "in1"),
    );
    link.insert(LinkStateComponent(LinkState::Wired));
    let rendered = serde_json::to_value(LinkOutput::of_a_link(
        &link,
        &crate::core::json_schema::NodeNamesByProcessorId::holding_no_node(),
    ))
    .unwrap();
    assert!(
        rendered.get("error_reason").is_none(),
        "an ordinary link's shape is unchanged by this key: {rendered}"
    );
}
