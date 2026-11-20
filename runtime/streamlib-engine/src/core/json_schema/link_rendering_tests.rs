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
        serde_json::to_value(rendered_link(
            link,
            &crate::core::json_schema::NodeNamesByNodeRunId::holding_no_node(),
            None,
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
    use crate::core::nodes::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};

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
        serde_json::to_value(rendered_link(
            link,
            &crate::core::json_schema::NodeNamesByNodeRunId::holding_no_node(),
            None,
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
    use crate::core::nodes::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};

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

    let rendered = serde_json::to_value(rendered_link(
        &link,
        &crate::core::json_schema::NodeNamesByNodeRunId::holding_no_node(),
        None,
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
    let rendered = serde_json::to_value(rendered_link(
        &link,
        &crate::core::json_schema::NodeNamesByNodeRunId::holding_no_node(),
        None,
    ))
    .unwrap();
    assert!(
        rendered.get("error_reason").is_none(),
        "an ordinary link's shape is unchanged by this key: {rendered}"
    );
}

/// The verdict of a link from an output of `output` to an input of `input`, among no conversions.
fn the_verdict_between(
    output: &crate::core::descriptors::PinType,
    input: &crate::core::descriptors::PinType,
) -> PinLinkVerdict {
    use crate::core::descriptors::{PinLinkEnd, PinLinkInputEnd, pin_link_verdict};
    let output = PinLinkEnd {
        node_name: "source",
        port_name: "out",
        member_name: None,
        pin_type: output,
        meaning: None,
        flow: None,
    };
    let sinks_type = crate::core::descriptors::NodeClassImportPath::new("nodes.sink:Sink").unwrap();
    let input = PinLinkInputEnd {
        end: PinLinkEnd {
            node_name: "sink",
            port_name: "in",
            pin_type: input,
            ..output
        },
        node_type: &sinks_type,
        input_mode: None,
    };
    pin_link_verdict(output, input, &[])
}

/// A link between two typed ends the verdict reads for free says so, and warns of nothing it
/// did not warn of.
#[test]
fn a_link_whose_input_reads_its_output_for_free_says_it_converts_for_free() {
    use crate::core::descriptors::PinType;
    let link = Link::between(
        OutputLinkPortRef::new("Psrc", "out1"),
        InputLinkPortRef::new("Pdst", "in1"),
    );
    let verdict = the_verdict_between(
        &PinType::Integer { exact_size: None },
        &PinType::Float { exact_size: None },
    );

    let rendered = serde_json::to_value(rendered_link(
        &link,
        &crate::core::json_schema::NodeNamesByNodeRunId::holding_no_node(),
        Some(&verdict),
    ))
    .unwrap();

    assert_eq!(rendered["free_conversion"], true, "{rendered}");
    assert!(rendered.get("warnings").is_none(), "{rendered}");
}

/// A link the verdict warns of carries each warning as `check_link` answers it, in the words
/// the runtime writes for it.
#[test]
fn a_warned_link_carries_the_warnings_its_verdict_gave() {
    use crate::core::descriptors::PinType;
    use streamlib_node_schema::pin_type::{PinLinkWarning, VectorComponentCount};
    let link = Link::between(
        OutputLinkPortRef::new("Psrc", "out1"),
        InputLinkPortRef::new("Pdst", "in1"),
    );
    let verdict = the_verdict_between(
        &PinType::Vector {
            component_count: VectorComponentCount::Four,
        },
        &PinType::Color {},
    );

    let rendered = serde_json::to_value(rendered_link(
        &link,
        &crate::core::json_schema::NodeNamesByNodeRunId::holding_no_node(),
        Some(&verdict),
    ))
    .unwrap();

    assert_eq!(
        rendered["warnings"],
        serde_json::json!([{
            "warning": "vector_read_as_a_color",
            "words": PinLinkWarning::VectorReadAsAColor.to_string(),
        }]),
        "{rendered}"
    );
    assert_eq!(rendered["free_conversion"], true, "{rendered}");
}

/// A link that takes what it carries as it is, and one with an untyped end, which no verdict
/// judges, say neither.
#[test]
fn a_link_that_converts_nothing_says_neither() {
    use crate::core::descriptors::PinType;
    let link = Link::between(
        OutputLinkPortRef::new("Psrc", "out1"),
        InputLinkPortRef::new("Pdst", "in1"),
    );
    let float = PinType::Float { exact_size: None };
    let as_it_is = the_verdict_between(&float, &float);
    for verdict in [Some(&as_it_is), None] {
        let rendered = serde_json::to_value(rendered_link(
            &link,
            &crate::core::json_schema::NodeNamesByNodeRunId::holding_no_node(),
            verdict,
        ))
        .unwrap();
        assert!(
            rendered.get("free_conversion").is_none() && rendered.get("warnings").is_none(),
            "{rendered}"
        );
    }
}

/// Give the port `port_name` on `direction`'s side of `node` in `graph` the pin type
/// `pin_type`.
fn give_a_port_the_pin_type(
    graph: &mut crate::core::graph::Graph,
    node: &crate::core::graph::NodeRunId,
    direction: crate::core::PortDirection,
    port_name: &str,
    pin_type: crate::core::descriptors::PinType,
) {
    let node = graph.traversal_mut().v(node);
    let node = node.first_mut().unwrap();
    let port = node
        .ports
        .on_mut(direction)
        .iter_mut()
        .find(|port| port.name == port_name)
        .unwrap();
    port.pin = Some(crate::core::descriptors::ResolvedPin {
        pin_type,
        flow: crate::core::descriptors::PinFlow::Value,
    });
}

#[test]
fn a_carried_link_between_typed_ends_says_it_converts_for_free_as_its_two_ends_judge_it() {
    use crate::core::descriptors::PinType;
    let (mut graph, lfo, reader, _) =
        crate::core::compiler::compiler_ops::function_node_pool::tests::a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
    give_a_port_the_pin_type(
        &mut graph,
        &lfo,
        crate::core::PortDirection::Output,
        "result",
        PinType::Integer { exact_size: None },
    );
    give_a_port_the_pin_type(
        &mut graph,
        &reader,
        crate::core::PortDirection::Input,
        "in1",
        PinType::Float { exact_size: None },
    );

    let rendered = graph.to_graph_response("rig".to_string());

    assert_eq!(rendered.links.len(), 1, "{:?}", rendered.links);
    assert!(
        rendered.links[0].marks.free_conversion,
        "an Integer read as a Float through the pool converts for free: {:?}",
        rendered.links[0]
    );
}
