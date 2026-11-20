// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Windowed audio ports: the contract each resolves at wire time and the one inbound link each takes.

use super::*;

fn add_mock_device_matched_audio_consumer(graph: &mut Graph) -> String {
    crate::core::test_support::ensure_test_mocks_registered();
    graph
        .traversal_mut()
        .add_v(NodeSpec::new(
            crate::core::test_support::MockDeviceMatchedAudioConsumerNode::node_class_import_path(),
            serde_json::Value::Null,
        ))
        .expect("the node is named")
        .first()
        .expect("mock_device_matched_audio_consumer_node must be in the registry")
        .id
        .to_string()
}

/// A port declaring the five values resolves to them at wire time, and the
/// mailbox that port gets is sized by the contract rather than the profile.
#[test]
fn a_windowed_destinations_contract_resolves_at_wire_time() {
    let mut graph = Graph::new();
    let dest_id = add_mock_windowed_audio_consumer(&mut graph);

    let windowing =
        audio_windowing_declared_by_input_port_of(&graph, &dest_id.as_str().into(), "audio")
            .expect("a declared contract resolves")
            .expect("the port declares one");

    let AudioWindowDeclarationOfAnInputPort::StatedOutright(contract) = windowing else {
        panic!("a port stating five values resolves to them at wire time");
    };
    assert_eq!(contract.sample_rate, 16_000);
    assert_eq!(contract.channels, Some(1));
    assert_eq!(contract.window_size, 512);
    assert_eq!(contract.hop, 512);
}

/// A port with no contract resolves to none, and nothing about it moves.
#[test]
fn a_port_declaring_no_contract_resolves_to_none() {
    let mut graph = Graph::new();
    let dest_id = add_mock_reactive_input_only(&mut graph);

    let windowing =
        audio_windowing_declared_by_input_port_of(&graph, &dest_id.as_str().into(), "in1")
            .expect("resolution succeeds");
    assert!(windowing.is_none());
}

#[test]
fn a_match_device_contract_wires_awaiting_its_device_rather_than_refusing() {
    let mut graph = Graph::new();
    let dest_id = add_mock_device_matched_audio_consumer(&mut graph);

    let windowing =
        audio_windowing_declared_by_input_port_of(&graph, &dest_id.as_str().into(), "audio")
            .expect("a sentinel is not a wiring error by itself")
            .expect("the port declares one");

    assert_eq!(
        windowing,
        AudioWindowDeclarationOfAnInputPort::MatchesItsNodesDeviceStream
    );
}

/// The whole loop the sentinel needs, at the seam that has to close it: an
/// app-process destination declaring `match_device` is wired awaiting its
/// device, its node is given the shared settled contracts, and once its own
/// `setup()` settles one `graph` renders those values on the port that
/// settled them.
///
/// Mentally revert the publish and the node keeps rendering the sentinel
/// for the whole run — a port whose `graph` entry is a declaration nobody
/// can act on rather than the format it is actually converting to.
#[test]
fn a_settled_contract_reaches_graph_on_the_port_that_settled_it() {
    use crate::core::test_support::MockDeviceMatchedAudioConsumerNode;

    let mut graph = Graph::new();
    let dest_id = add_mock_device_matched_audio_consumer(&mut graph);
    let dest_unique_id: NodeRunId = dest_id.as_str().into();
    let (dest, _, dest_input) = attach_mock_instance::<MockDeviceMatchedAudioConsumerNode::Node>(
        &mut graph,
        dest_unique_id.as_ref(),
    );
    let dest_input = dest_input.expect("a windowed consumer holds input mailboxes");

    let (channel, notify_service) =
        open_test_link_services("match-device-graph", WINDOWED_PORT_SUBSCRIBER_RING_DEPTH);
    wire_rust_dest(
        &mut graph,
        &dest_unique_id,
        &dest,
        "audio",
        &"L-match-device".into(),
        "psource/audio_out",
        DeliveryProfile::Ordered.resolve(),
        &channel,
        &notify_service,
        Some(AudioWindowDeclarationOfAnInputPort::MatchesItsNodesDeviceStream),
        crate::iceoryx2::BundleMembersAtTheRead::default(),
    )
    .expect("a sentinel wires rather than refusing");

    assert_eq!(
        dest_input.input_ports_still_awaiting_their_device_stream_format(),
        vec!["audio".to_string()],
        "the wiring runs before setup(), so the port waits rather than windowing"
    );
    assert_eq!(
        rendered_audio_window_of(&mut graph, &dest_unique_id),
        serde_json::json!({ "resolved_from": "match_device" }),
        "nothing has opened a device yet, and a guess in its place would be a lie"
    );

    dest_input
        .settle_a_ports_device_matched_audio_window_contract(
            "audio",
            &crate::iceoryx2::AudioWindowContractMatchingADeviceStream {
                device_stream_format: crate::core::context::AudioStreamFormat {
                    sample_rate: 44_100,
                    channels: 2,
                    sample_format: crate::core::context::AudioSampleFormat::F32,
                },
                window_size_in_per_channel_samples: 441,
                hop_in_per_channel_samples: 441,
            },
        )
        .expect("the node's own setup() settles it");

    assert_eq!(
        rendered_audio_window_of(&mut graph, &dest_unique_id),
        serde_json::json!({
            "resolved_from": "device",
            "sample_rate": 44_100,
            "channels": 2,
            "dtype": "f32",
            "window_size": 441,
            "hop": 441,
        }),
        "graph renders what the device gave, live, off the port that settled it"
    );
}

/// One node's `"audio"` input port as `graph` renders its window contract.
fn rendered_audio_window_of(graph: &mut Graph, proc_id: &NodeRunId) -> serde_json::Value {
    let node = graph
        .traversal_mut()
        .v(proc_id)
        .first()
        .expect("the node must be in the graph");
    serde_json::to_value(crate::core::json_schema::GraphNodeOutput::from(node))
        .expect("a node renders")["ports"]["inputs"][0]["audio_window"]
        .clone()
}

#[test]
fn a_match_device_port_on_a_helper_placed_destination_is_refused_at_wire_time() {
    let mut graph = Graph::new();
    let dest_id = add_mock_device_matched_audio_consumer(&mut graph);
    attach_node_instance(
        &mut graph,
        &dest_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );

    let dest_link_wiring = out_of_process_link_wiring_of(&graph, &dest_id.as_str().into())
        .expect("a helper stub's node carries its link wiring");
    let refusal = wire_subprocess_dest(
        &mut graph,
        &dest_link_wiring,
        &dest_id.as_str().into(),
        "audio",
        "pabc/out1",
        "pdef/notify",
        DeliveryProfile::Ordered.resolve(),
        sizing_of_a_two_subscriber_test_channel(),
        1,
        &"L-helper-windowed".into(),
        Some(AudioWindowDeclarationOfAnInputPort::MatchesItsNodesDeviceStream),
        &crate::iceoryx2::BundleMembersAtTheRead::default(),
    )
    .expect_err("a helper child cannot settle a sentinel")
    .to_string();

    assert!(
        refusal.contains("match_device")
            && refusal.contains("setup()")
            && refusal.contains("audio"),
        "the refusal must name the sentinel, where it resolves, and the port; got {refusal}"
    );
}

/// A contract the parent settled rides the envelope as five values, so the
/// child opens its own stage on the same numbers and never sees a sentinel.
#[test]
fn a_settled_contract_reaches_a_helper_placed_destination_as_five_values() {
    let mut graph = Graph::new();
    let dest_id = add_mock_device_matched_audio_consumer(&mut graph);
    let dest_instance = attach_node_instance(
        &mut graph,
        &dest_id,
        NodeInstance::new(Box::new(OutOfProcessSpawnHostStub::default())),
    );

    let settled = crate::iceoryx2::ResolvedAudioWindowContract::from_a_device_stream_format(
        &crate::iceoryx2::AudioWindowContractMatchingADeviceStream {
            device_stream_format: crate::core::context::AudioStreamFormat {
                sample_rate: 48_000,
                channels: 2,
                sample_format: crate::core::context::AudioSampleFormat::F32,
            },
            window_size_in_per_channel_samples: 480,
            hop_in_per_channel_samples: 480,
        },
    )
    .expect("a device format settles a contract");

    let dest_link_wiring = out_of_process_link_wiring_of(&graph, &dest_id.as_str().into())
        .expect("a helper stub's node carries its link wiring");
    wire_subprocess_dest(
        &mut graph,
        &dest_link_wiring,
        &dest_id.as_str().into(),
        "audio",
        "pabc/out1",
        "pdef/notify",
        DeliveryProfile::Ordered.resolve(),
        sizing_of_a_two_subscriber_test_channel(),
        1,
        &"L-helper-settled".into(),
        Some(AudioWindowDeclarationOfAnInputPort::StatedOutright(settled)),
        &crate::iceoryx2::BundleMembersAtTheRead::default(),
    )
    .expect("a settled contract renders onto the envelope");

    let recorded = dest_instance
        .lock()
        .out_of_process_link_wiring()
        .expect("the stub records its own wiring")
        .as_setup_command_ports();
    assert_eq!(
        recorded["inputs"][0]["audio_window"],
        serde_json::json!({
            "sample_rate": 48_000,
            "channels": 2,
            "dtype": "f32",
            "window_size": 480,
            "hop": 480,
        }),
        "the child reads five values, never the sentinel that produced them"
    );
}

/// Fan-in legally interleaves N producers' blocks in one mailbox, and two
/// sample streams interleaved into one accumulator is not a mix — it is
/// garbage windows. A windowed port takes exactly one inbound link.
#[test]
fn a_second_inbound_link_into_a_windowed_port_is_refused_naming_the_port_and_both_links() {
    let mut graph = Graph::new();
    let first_source = add_mock_output_only(&mut graph);
    let second_source = add_mock_output_only(&mut graph);
    let dest_id = add_mock_windowed_audio_consumer(&mut graph);

    let mut wired = Vec::new();
    for source in [&first_source, &second_source] {
        let link = graph
            .traversal_mut()
            .add_e(
                OutputLinkPortRef::new(source, "out1"),
                InputLinkPortRef::new(&dest_id, "audio"),
            )
            .first()
            .expect("the link is added")
            .id
            .to_string();
        wired.push(link);
    }

    let dest_unique_id: NodeRunId = dest_id.as_str().into();
    // The first link is fine on its own.
    let second_link: LinkUniqueId = wired[1].as_str().into();
    let refusal = refuse_a_second_inbound_link_into_a_windowed_port(
        &graph,
        &dest_unique_id,
        "audio",
        &second_link,
    )
    .expect_err("a windowed port accepts exactly one inbound link");

    let rendered = refusal.to_string();
    assert!(
        rendered.contains("audio") && rendered.contains(&wired[0]) && rendered.contains(&wired[1]),
        "the refusal must name the port and both links; got {rendered}"
    );
}

/// The port's only inbound link must not refuse itself.
#[test]
fn the_one_inbound_link_a_windowed_port_takes_is_not_refused() {
    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let dest_id = add_mock_windowed_audio_consumer(&mut graph);
    let link = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&source_id, "out1"),
            InputLinkPortRef::new(&dest_id, "audio"),
        )
        .first()
        .expect("the link is added")
        .id
        .to_string();

    refuse_a_second_inbound_link_into_a_windowed_port(
        &graph,
        &dest_id.as_str().into(),
        "audio",
        &link.as_str().into(),
    )
    .expect("one inbound link is what a windowed port takes");
}

/// A link on its way out of the graph is not a second inbound one, or a
/// disconnect followed by a reconnect would refuse itself.
#[test]
fn a_disconnected_link_does_not_count_against_a_windowed_ports_one_inbound_link() {
    let mut graph = Graph::new();
    let old_source = add_mock_output_only(&mut graph);
    let new_source = add_mock_output_only(&mut graph);
    let dest_id = add_mock_windowed_audio_consumer(&mut graph);

    let departed = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&old_source, "out1"),
            InputLinkPortRef::new(&dest_id, "audio"),
        )
        .first()
        .expect("the link is added")
        .id
        .to_string();
    graph
        .traversal_mut()
        .e(LinkUniqueId::from(departed.as_str()))
        .first_mut()
        .expect("the departing link is in the graph")
        .insert(LinkStateComponent(LinkState::Disconnected));

    let reconnecting = graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(&new_source, "out1"),
            InputLinkPortRef::new(&dest_id, "audio"),
        )
        .first()
        .expect("the link is added")
        .id
        .to_string();

    refuse_a_second_inbound_link_into_a_windowed_port(
        &graph,
        &dest_id.as_str().into(),
        "audio",
        &reconnecting.as_str().into(),
    )
    .expect("a reconnect past a disconnected link is one inbound link, not two");
}
