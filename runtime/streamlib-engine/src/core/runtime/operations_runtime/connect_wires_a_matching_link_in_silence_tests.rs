// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link between two ports of one family wires in silence: connect
//! emits no warning.

use std::sync::Arc;

use serde_json::Value;

use parking_lot::Mutex;

use super::{
    LiveGraphChangeOnOneStream, connect_impl, disconnect_impl,
    mark_the_node_and_each_of_its_links_pending_deletion, the_node_this_stream_names,
};
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::descriptors::ProcessorClassImportPath;
use crate::core::descriptors::{PortDescriptor, ProcessorClassShortName, ProcessorDescriptor};
use crate::core::graph::{
    GraphEdgeWithComponents, InputLinkPortRef, LinkUniqueId, OutputLinkPortRef,
    PendingDeletionComponent, ProcessorUniqueId,
};
use crate::core::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use crate::core::pubsub::{Event, LoadedStreamIdentity, PUBSUB, RuntimeEvent};
use crate::core::test_support::CapturedTracingWarnings;
use crate::core::{Error, Result};

/// The stream every fixture compiler's events publish under.
fn the_fixtures_stream() -> LoadedStreamIdentity {
    LoadedStreamIdentity {
        runtime_id: "Rconnectsilence".to_string(),
        stream_id: "main".to_string(),
        stream_tag: crate::core::runtime::LoadedStreamTag::numbered_for_a_test(1),
    }
}

/// A graph change on a fixture compiler that is not started.
fn a_fixture_graph_change(compiler: Arc<Compiler>) -> LiveGraphChangeOnOneStream {
    LiveGraphChangeOnOneStream {
        compiler,
        live: None,
        stream: the_fixtures_stream(),
    }
}

const PRODUCER_TYPE: &str = "ConnectSilenceProducer";
const CONSUMER_TYPE: &str = "ConnectSilenceConsumer";

fn class_short_name(ty: &str) -> ProcessorClassShortName {
    ProcessorClassShortName::new(ty).unwrap()
}

fn producer_class_path() -> ProcessorClassImportPath {
    ProcessorClassImportPath::new(format!("{}::{PRODUCER_TYPE}", module_path!())).unwrap()
}

fn consumer_class_path() -> ProcessorClassImportPath {
    ProcessorClassImportPath::new(format!("{}::{CONSUMER_TYPE}", module_path!())).unwrap()
}

/// Register the producer and consumer descriptors this module wires.
fn register_producer_and_consumer_descriptors() {
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        let mut producer = ProcessorDescriptor::new(
            class_short_name(PRODUCER_TYPE),
            producer_class_path(),
            "producer",
        );
        producer.outputs.push(PortDescriptor::new(
            "out",
            "output",
            true,
            crate::core::descriptors::PortFamilyDeclaration::of_family(
                crate::core::descriptors::PortFamily::Image,
            ),
        ));
        PROCESSOR_REGISTRY
            .register_descriptor_only(producer)
            .expect("register producer descriptor");

        let mut consumer = ProcessorDescriptor::new(
            class_short_name(CONSUMER_TYPE),
            consumer_class_path(),
            "consumer",
        );
        consumer.inputs.push(
            PortDescriptor::new(
                "in",
                "input",
                true,
                crate::core::descriptors::PortFamilyDeclaration::of_family(
                    crate::core::descriptors::PortFamily::Image,
                ),
            )
            .with_delivery_profile("newest"),
        );
        PROCESSOR_REGISTRY
            .register_descriptor_only(consumer)
            .expect("register consumer descriptor");
    });
}

/// Fresh compiler holding one producer and one consumer node, plus the
/// wiring refs for the producer's `out` and the consumer's `in`.
fn compiler_holding_a_producer_and_consumer_node()
-> (Arc<Compiler>, OutputLinkPortRef, InputLinkPortRef) {
    let compiler = Arc::new(Compiler::new());
    let (from_id, to_id): (ProcessorUniqueId, ProcessorUniqueId) = compiler.scope(|graph, _tx| {
        let from = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(producer_class_path(), Value::Null))
            .expect("the node is named")
            .first()
            .expect("producer node must be created")
            .id
            .clone();
        let to = graph
            .traversal_mut()
            .add_v(ProcessorSpec::new(consumer_class_path(), Value::Null))
            .expect("the node is named")
            .first()
            .expect("consumer node must be created")
            .id
            .clone();
        (from, to)
    });
    (
        compiler,
        OutputLinkPortRef::new(from_id, "out"),
        InputLinkPortRef::new(to_id, "in"),
    )
}

#[test]
fn connect_wires_a_producer_to_a_consumer_without_warning() {
    register_producer_and_consumer_descriptors();
    let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();
    let (result, captured) = CapturedTracingWarnings::captured_while(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
            .block_on(connect_impl(a_fixture_graph_change(compiler), from, to))
    });

    result.expect("connect wires two Image ports");
    assert!(
        captured.is_empty(),
        "connect must emit no WARN when wiring a link; captured: {captured:?}"
    );
}

/// Removing a processor removes each of its links as a link, ahead of the
/// node, so the compile reclaims both endpoints' ports through the link
/// path. Revert lock: log only `RemoveProcessor` and the node's removal
/// cascades its edges out of the graph with the peer's publisher,
/// subscriber and notifier still held against their channels.
#[test]
fn removing_a_processor_logs_every_incident_link_ahead_of_the_node() {
    register_producer_and_consumer_descriptors();
    let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();
    let consumer_id = to.processor_id().clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime");

    let link_id = runtime
        .block_on(connect_impl(
            a_fixture_graph_change(Arc::clone(&compiler)),
            from,
            to,
        ))
        .expect("the link is created");
    compiler
        .scope(|graph, tx| {
            mark_the_node_and_each_of_its_links_pending_deletion(graph, tx, &consumer_id)
        })
        .expect("the consumer is marked for removal");

    let logged = compiler.logged_pending_operations();
    let link_removal = logged
        .iter()
        .position(|op| matches!(op, PendingOperation::RemoveLink(id) if *id == link_id))
        .expect("the link's own removal is logged");
    let node_removal = logged
        .iter()
        .position(|op| matches!(op, PendingOperation::RemoveProcessor(id) if *id == consumer_id))
        .expect("the node's removal is logged");
    assert!(
        link_removal < node_removal,
        "the link goes before the node it hangs off; logged {logged:?}"
    );
    let link_is_marked_for_deletion = compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .e(&link_id)
            .first()
            .map(|link| link.has::<PendingDeletionComponent>())
            .unwrap_or(false)
    });
    assert!(
        link_is_marked_for_deletion,
        "the link is marked pending deletion like the node"
    );
}

/// Run `connect` on a current-thread runtime, the way the two tests above
/// do.
fn connect_on_this_thread(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<LinkUniqueId> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime")
        .block_on(connect_impl(
            a_fixture_graph_change(Arc::clone(compiler)),
            from,
            to,
        ))
}

/// The display name the graph gave one of the fixture's nodes.
fn the_display_name_the_graph_gave(
    compiler: &Arc<Compiler>,
    processor_id: &ProcessorUniqueId,
) -> String {
    compiler.scope(|graph, _tx| {
        graph
            .traversal()
            .v(processor_id)
            .first()
            .expect("the node is in the graph")
            .display_name
            .clone()
    })
}

/// Run `disconnect` on a current-thread runtime, the way `connect` is run.
fn disconnect_on_this_thread(compiler: &Arc<Compiler>, link_id: LinkUniqueId) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime")
        .block_on(disconnect_impl(
            a_fixture_graph_change(Arc::clone(compiler)),
            link_id,
        ))
}

/// A node name this runtime does not hold is refused by name, listing the
/// names it holds.
#[test]
fn a_node_name_this_runtime_does_not_hold_is_refused_listing_what_it_holds() {
    register_producer_and_consumer_descriptors();
    let (compiler, from, _to) = compiler_holding_a_producer_and_consumer_node();
    let displayed = the_display_name_the_graph_gave(&compiler, from.processor_id());

    let refusal = the_node_this_stream_names(&compiler, "main", "NoSuchProcessor")
        .expect_err("a node name this runtime does not hold is refused")
        .to_string();

    assert!(
        refusal.contains("no node in the stream `main` is named \"NoSuchProcessor\""),
        "{refusal}"
    );
    assert!(refusal.contains(&displayed), "{refusal}");
}

/// The disconnect events name each end's own port on a link whose two port
/// names differ.
///
/// Mental-revert: rebuild either endpoint from the other's port name — the
/// shape this path carried until #2292 — and a link `out` → `in` announces
/// itself as `out` → `out`, so a listener keyed on the port it watches
/// never sees the disconnect.
#[test]
fn the_disconnect_events_name_each_ends_own_port_when_the_two_differ() {
    use crate::core::pubsub::{EventListener, topics};

    register_producer_and_consumer_descriptors();
    let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();
    let link_id = connect_on_this_thread(&compiler, from.clone(), to.clone())
        .expect("the fixture's two nodes connect");

    struct RecordingTheDisconnectEvents(Arc<Mutex<Vec<(String, String)>>>);
    impl EventListener for RecordingTheDisconnectEvents {
        fn on_event(&mut self, event: &Event) -> Result<()> {
            if let Event::OfALoadedStream {
                event:
                    RuntimeEvent::RuntimeWillDisconnect {
                        from_port, to_port, ..
                    }
                    | RuntimeEvent::RuntimeDidDisconnect {
                        from_port, to_port, ..
                    },
                ..
            } = event
            {
                self.0.lock().push((from_port.clone(), to_port.clone()));
            }
            Ok(())
        }

        fn on_events_lost(&mut self, _lost_event_count: u64) -> Result<()> {
            Ok(())
        }
    }

    let announced = Arc::new(Mutex::new(Vec::new()));
    let listener: Arc<Mutex<dyn EventListener>> = Arc::new(Mutex::new(
        RecordingTheDisconnectEvents(Arc::clone(&announced)),
    ));
    PUBSUB
        .subscribe(
            &topics::loaded_stream(&the_fixtures_stream()),
            Arc::clone(&listener),
        )
        .expect("subscribe establishes the subscriber");

    disconnect_on_this_thread(&compiler, link_id).expect("the link disconnects");

    // Delivery is not synchronous with the publish.
    let gave_up_at = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < gave_up_at && announced.lock().len() < 2 {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let announced = announced.lock().clone();
    assert_eq!(
        announced.len(),
        2,
        "both the will- and did-disconnect events are published: {announced:?}"
    );
    for (from_port, to_port) in announced {
        assert_eq!(from_port, from.to_string(), "the source end names `out`");
        assert_eq!(to_port, to.to_string(), "the destination end names `in`");
    }
}

/// A destination this runtime does not hold, or a port it does not have,
/// is refused with the typed error a caller can act on.
#[test]
fn a_destination_this_runtime_cannot_take_is_refused_with_a_typed_error() {
    register_producer_and_consumer_descriptors();
    let (compiler, from, to) = compiler_holding_a_producer_and_consumer_node();

    assert!(matches!(
        connect_on_this_thread(
            &compiler,
            from.clone(),
            InputLinkPortRef::new("Pnobody", "in")
        ),
        Err(Error::ProcessorNotFound(_))
    ));
    assert!(matches!(
        connect_on_this_thread(
            &compiler,
            from,
            InputLinkPortRef::new(to.processor_id().clone(), "no_such_port")
        ),
        Err(Error::ProcessorPortNotFound { .. })
    ));
}
