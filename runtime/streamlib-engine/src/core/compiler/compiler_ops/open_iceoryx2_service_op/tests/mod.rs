// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Fixtures shared by the iceoryx2 service operation's tests.

use super::channel_service_naming_and_sizing::channel_max_subscribers;
use super::link_destination_side::{wire_rust_dest, wire_subprocess_dest};
use super::link_source_side::{wire_rust_source, wire_subprocess_source};
use super::*;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::NodeRunId;
use crate::core::error::Result;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, GraphNodeWithComponents,
    Iceoryx2ServicesHeldOpenForLinkComponent, InputLinkPortRef, LinkState, LinkStateComponent,
    LinkUniqueId, OutputLinkPortRef,
};
use crate::core::nodes::{NodeInstance, OutOfProcessLinkWireReply, OutOfProcessLinkWiringEnvelope};
use crate::iceoryx2::{
    AudioWindowDeclarationOfAnInputPort, ChannelEgressConfig, ChannelSizing, ChannelTrustTier,
    DeliveryProfile, DeliveryResolution, Iceoryx2Node, InboundLinkName,
    RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL, WINDOWED_PORT_SUBSCRIBER_RING_DEPTH,
};
use streamlib_ipc_types::{MAX_DESTINATIONS_PER_CHANNEL, MAX_INBOUND_LINKS_PER_DESTINATION};

use crate::core::execution::{ExecutionConfig, ProcessExecution};
use crate::core::graph::NodeInstanceWithItsOutOfProcessLinkWiring;
use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
use crate::core::nodes::{DynGeneratedNode, NodeSpec, OutOfProcessLinkWireOutcome};
use crate::core::test_support::{
    CapturedTracingWarnings, ReclaimedLink, RecordingOutOfProcessFarSideLinkDelivery,
    add_mock_input_only, add_mock_output_only,
};
use crate::core::{NodeDescriptor, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::iceoryx2::MessageGroupBoundary;

mod channel_service_naming_and_sizing_tests;
mod link_into_a_setting_read_by_the_engine_tests;
mod link_state_and_channel_depth_tests;
mod native_link_loss_count_tests;
mod out_of_process_link_tests;
mod windowed_audio_channel_depth_tests;
mod windowed_audio_contract_tests;

struct OutOfProcessSpawnHostStub {
    link_wiring: Arc<OutOfProcessLinkWiringEnvelope>,
}

impl OutOfProcessSpawnHostStub {
    fn with_a_far_side_past_its_setup_command(
        far_side: RecordingOutOfProcessFarSideLinkDelivery,
        far_side_process_execution: ProcessExecution,
    ) -> Self {
        let link_wiring = Arc::new(OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
            far_side_process_execution,
        ));
        link_wiring
            .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), far_side)
            .expect("a stub's setup command always goes out");
        Self { link_wiring }
    }
}

/// Reactive, the mode a Python class with inputs takes unless it says otherwise.
impl Default for OutOfProcessSpawnHostStub {
    fn default() -> Self {
        Self::with_a_far_side_past_its_setup_command(
            RecordingOutOfProcessFarSideLinkDelivery::default(),
            ProcessExecution::Reactive,
        )
    }
}

impl DynGeneratedNode for OutOfProcessSpawnHostStub {
    fn __generated_setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn name(&self) -> &str {
        "OutOfProcessSpawnHostStub"
    }
    fn descriptor(&self) -> Option<NodeDescriptor> {
        None
    }
    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::new(ProcessExecution::Manual)
    }
    fn has_iceoryx2_outputs(&self) -> bool {
        false
    }
    fn has_iceoryx2_inputs(&self) -> bool {
        false
    }
    fn set_iceoryx2_resources(
        &mut self,
        _output_writer: Option<crate::iceoryx2::OutputWriter>,
        _input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> Result<()> {
        Ok(())
    }
    fn iceoryx2_output_writer_inner(&self) -> Option<Arc<crate::iceoryx2::OutputWriterInner>> {
        None
    }
    fn iceoryx2_input_mailboxes_inner(&self) -> Option<Arc<crate::iceoryx2::InputMailboxesInner>> {
        None
    }
    fn out_of_process_link_wiring(&self) -> Option<Arc<OutOfProcessLinkWiringEnvelope>> {
        Some(Arc::clone(&self.link_wiring))
    }
    fn to_runtime_json(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Attach `instance` to `proc_id` through the attach the spawn op takes, so
/// the wiring path can reach it.
fn attach_node_instance(
    graph: &mut Graph,
    proc_id: &str,
    instance: NodeInstance,
) -> Arc<Mutex<NodeInstance>> {
    NodeInstanceWithItsOutOfProcessLinkWiring::from(instance).attach_to(
        graph
            .traversal_mut()
            .v(proc_id)
            .first_mut()
            .expect("the node must exist"),
    )
}

/// The input entry a helper stub was handed for `link_id`.
fn input_entry_recorded_on(
    instance: &Arc<Mutex<NodeInstance>>,
    link_id: &LinkUniqueId,
) -> serde_json::Value {
    instance
        .lock()
        .out_of_process_link_wiring()
        .expect("the stub records its own wiring")
        .as_setup_command_ports()["inputs"]
        .as_array()
        .expect("the envelope renders its inputs as an array")
        .iter()
        .find(|entry| entry["link_id"] == serde_json::json!(link_id.as_str()))
        .cloned()
        .expect("the link's entry is on the envelope")
}

fn attach_mock_instance<P>(
    graph: &mut Graph,
    proc_id: &str,
) -> (
    Arc<Mutex<NodeInstance>>,
    Option<Arc<crate::iceoryx2::OutputWriterInner>>,
    Option<Arc<crate::iceoryx2::InputMailboxesInner>>,
)
where
    P: crate::core::GeneratedNode + DynGeneratedNode + Send + 'static,
    P::Config: Default,
{
    let mut instance = NodeInstance::new(Box::new(
        P::from_config(Default::default()).expect("the mock constructs from its default config"),
    ));
    instance
        .install_iceoryx2_resources()
        .expect("the mock accepts its iceoryx2 resources");
    let output_inner = instance.iceoryx2_output_writer_inner();
    let input_inner = instance.iceoryx2_input_mailboxes_inner();
    let instance = attach_node_instance(graph, proc_id, instance);
    (instance, output_inner, input_inner)
}

/// Sizing for a test channel with room for one destination and the
/// publisher's reserved slot.
fn sizing_of_a_two_subscriber_test_channel() -> ChannelSizing {
    ChannelSizing {
        max_subscribers: 2,
        channel_service_creation_depth: DeliveryProfile::ORDERED_DEPTH,
    }
}

/// A service name no concurrent test — or an earlier run that recycled this
/// pid — can collide with on iceoryx2's machine-global `/dev/shm` namespace.
/// A collision surfaces as `DoesNotSupportRequestedMinBufferSize` against
/// the stale service, not as a clean failure.
fn unique_service_name(tag: &str) -> String {
    format!(
        "test/wiring/{tag}/{}",
        mint_machine_global_unique_name_suffix()
    )
}

/// The channel and notify services one wired link needs, on one node, the
/// channel created `channel_service_creation_depth` deep.
fn open_test_link_services(
    tag: &str,
    channel_service_creation_depth: usize,
) -> (
    crate::iceoryx2::Iceoryx2Service,
    crate::iceoryx2::Iceoryx2NotifyService,
) {
    let node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
    let channel = node
        .open_or_create_service(
            &unique_service_name(&format!("{tag}/channel")),
            2,
            channel_service_creation_depth,
        )
        .expect("the channel service must open");
    let notify = node
        .open_or_create_notify_service(&unique_service_name(&format!("{tag}/notify")), 1)
        .expect("the notify service must open");
    (channel, notify)
}

/// Wire one Rust→Rust link end to end the way the compiler op does, into a
/// reactive or a self-driven destination, and hand back the two sides'
/// iceoryx2 state.
fn wire_one_test_link<Destination>(
    tag: &str,
    destination_is_reactive: bool,
) -> (
    Arc<crate::iceoryx2::OutputWriterInner>,
    Arc<crate::iceoryx2::InputMailboxesInner>,
)
where
    Destination: crate::core::GeneratedNode + DynGeneratedNode + Send + 'static,
    Destination::Config: Default,
{
    use crate::core::test_support::MockOutputOnlyNode;

    let mut graph = Graph::new();
    let source_id = add_mock_output_only(&mut graph);
    let (source, source_output, _) =
        attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
    let source_output = source_output.expect("an output-only mock holds an output writer");

    let dest_id = if destination_is_reactive {
        add_mock_reactive_input_only(&mut graph)
    } else {
        add_mock_input_only(&mut graph)
    };
    let dest_unique_id: NodeRunId = dest_id.as_str().into();
    let (dest, _, dest_input) = attach_mock_instance::<Destination>(&mut graph, &dest_id);
    let dest_input = dest_input.expect("an input-only mock holds input mailboxes");

    let (channel, notify_service) = open_test_link_services(tag, DeliveryProfile::ORDERED_DEPTH);
    let link_id: LinkUniqueId = format!("L-{tag}").as_str().into();

    wire_rust_source(
        &mut graph,
        &source_id.as_str().into(),
        &source,
        "out1",
        &link_id,
        &channel,
        &notify_service,
        ChannelEgressConfig {
            service_name: unique_service_name(tag),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 4096,
            chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    )
    .expect("the source side wires");
    wire_rust_dest(
        &mut graph,
        &dest_unique_id,
        &dest,
        "in1",
        &link_id,
        "psource/out1",
        DeliveryProfile::Newest.resolve(),
        &channel,
        &notify_service,
        None,
        crate::iceoryx2::BundleMembersAtTheRead::default(),
    )
    .expect("the destination side wires");

    (source_output, dest_input)
}

/// One native link wired through both sides of the op the way the compiler
/// runs them, with the source's refusals and the destination's losses
/// counted where `graph` reads them.
struct NativeLinkWiredForLossCounting {
    graph: Graph,
    source_id: NodeRunId,
    dest_id: NodeRunId,
    link_id: LinkUniqueId,
    source_output: Arc<crate::iceoryx2::OutputWriterInner>,
    dest_input: Arc<crate::iceoryx2::InputMailboxesInner>,
    // Held so the channel outlives the test's writes.
    _channel: crate::iceoryx2::Iceoryx2Service,
}

impl NativeLinkWiredForLossCounting {
    fn wire(tag: &str, dest_delivery: DeliveryResolution, source_ceiling_bytes: usize) -> Self {
        use crate::core::test_support::{MockInputOnlyNode, MockOutputOnlyNode};

        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let (source, source_output, _) =
            attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
        let dest_id = add_mock_input_only(&mut graph);
        let (dest, _, dest_input) =
            attach_mock_instance::<MockInputOnlyNode::Node>(&mut graph, &dest_id);
        let source_id: NodeRunId = source_id.as_str().into();
        let dest_id: NodeRunId = dest_id.as_str().into();

        let (channel, notify_service) =
            open_test_link_services(tag, DeliveryProfile::ORDERED_DEPTH);
        let link_id: LinkUniqueId = format!("L-{tag}").as_str().into();
        wire_rust_source(
            &mut graph,
            &source_id,
            &source,
            "out1",
            &link_id,
            &channel,
            &notify_service,
            ChannelEgressConfig {
                service_name: unique_service_name(tag),
                trust_tier: ChannelTrustTier::Trusted,
                expected_payload_bytes: 4096,
                chunk_ceiling_bytes: source_ceiling_bytes,
            },
        )
        .expect("the source side wires");
        wire_rust_dest(
            &mut graph,
            &dest_id,
            &dest,
            "in1",
            &link_id,
            "psource/out1",
            dest_delivery,
            &channel,
            &notify_service,
            None,
            crate::iceoryx2::BundleMembersAtTheRead::default(),
        )
        .expect("the destination side wires");

        Self {
            graph,
            source_id,
            dest_id,
            link_id,
            source_output: source_output.expect("an output-only mock holds an output writer"),
            dest_input: dest_input.expect("an input-only mock holds input mailboxes"),
            _channel: channel,
        }
    }

    /// What `graph` renders under `metrics` for `proc_id`, or `None` for no key.
    fn rendered_metrics_of(&mut self, proc_id: &NodeRunId) -> Option<serde_json::Value> {
        self.graph
            .traversal_mut()
            .v(proc_id)
            .first()
            .expect("the node's node must be in the graph")
            .render_components()
            .metrics
            .map(|metrics| serde_json::to_value(metrics).unwrap())
    }

    fn write_bags(&self, bag_count: usize) {
        for bag in 0..bag_count {
            self.source_output
                .write_bytes_with_timestamp(
                    "out1",
                    b"a bag",
                    bag as i64,
                    MessageGroupBoundary::StartsGroup,
                )
                .expect("the source publishes onto the wired channel");
        }
    }
}

/// Add a link from `source_id`'s `out1` to `dest_id`'s `in1`.
fn add_link_from_out1_to_in1(graph: &mut Graph, source_id: &str, dest_id: &str) -> LinkUniqueId {
    add_link_from_out1_to(graph, source_id, dest_id, "in1")
}

/// Add a link from `source_id`'s `out1` to `dest_id`'s `dest_port`.
fn add_link_from_out1_to(
    graph: &mut Graph,
    source_id: &str,
    dest_id: &str,
    dest_port: &str,
) -> LinkUniqueId {
    graph
        .traversal_mut()
        .add_e(
            OutputLinkPortRef::new(source_id, "out1"),
            InputLinkPortRef::new(dest_id, dest_port),
        )
        .first()
        .expect("the link must exist")
        .id
        .clone()
}

/// The depth the live channel `source_id`'s `out1` publishes to was created
/// at, as a helper opening its own end would find it.
fn creation_depth_a_helper_opening_out1_finds(source_id: &str) -> usize {
    let channel_service_name = channel_service_name(&OutputLinkPortRef::new(source_id, "out1"))
        .expect("the mock's output port derives a channel name");
    Iceoryx2Node::for_this_test_process()
        .open_or_create_service(
            &channel_service_name,
            MAX_DESTINATIONS_PER_CHANNEL + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
            DeliveryProfile::NEWEST_DEPTH,
        )
        .expect("a helper opening at its own port's depth joins or creates the service")
        .channel_service_creation_depth()
}

fn add_mock_reactive_input_only(graph: &mut Graph) -> String {
    crate::core::test_support::ensure_test_mocks_registered();
    graph
        .traversal_mut()
        .add_v(NodeSpec::new(
            crate::core::test_support::MockReactiveInputOnlyNode::node_class_import_path(),
            serde_json::Value::Null,
        ))
        .expect("the node is named")
        .first()
        .expect("mock_reactive_input_only_node must be in the registry")
        .id
        .to_string()
}

fn add_mock_windowed_audio_consumer(graph: &mut Graph) -> String {
    crate::core::test_support::ensure_test_mocks_registered();
    graph
        .traversal_mut()
        .add_v(NodeSpec::new(
            crate::core::test_support::MockWindowedAudioConsumerNode::node_class_import_path(),
            serde_json::Value::Null,
        ))
        .expect("the node is named")
        .first()
        .expect("mock_windowed_audio_consumer_node must be in the registry")
        .id
        .to_string()
}

/// An app-process source wired through the op to an app-process windowed
/// consumer, with the handles a test drives the link through.
struct NativeLinkIntoAWindowedPort {
    graph: Graph,
    dest_id: NodeRunId,
    link_id: LinkUniqueId,
    source_output: Arc<crate::iceoryx2::OutputWriterInner>,
    dest_input: Arc<crate::iceoryx2::InputMailboxesInner>,
}

impl NativeLinkIntoAWindowedPort {
    fn wire() -> Self {
        use crate::core::test_support::MockOutputOnlyNode;

        let mut graph = Graph::new();
        let source_id = add_mock_output_only(&mut graph);
        let (_, source_output, _) =
            attach_mock_instance::<MockOutputOnlyNode::Node>(&mut graph, &source_id);
        let windowed = add_a_windowed_consumer_linked_from_out1(&mut graph, &source_id);
        open_iceoryx2_service(
            &mut graph,
            &windowed.link_id,
            &Iceoryx2Node::for_this_test_process(),
        )
        .expect("the windowed consumer wires");

        Self {
            graph,
            dest_id: windowed.dest_id.as_str().into(),
            link_id: windowed.link_id,
            source_output: source_output.expect("an output-only mock holds an output writer"),
            dest_input: windowed.dest_input,
        }
    }

    fn rendered_metrics_of_the_destination(&self) -> serde_json::Value {
        self.graph
            .traversal()
            .v(&self.dest_id)
            .first()
            .expect("the destination's node must be in the graph")
            .render_components()
            .metrics
            .map(|metrics| serde_json::to_value(metrics).unwrap())
            .unwrap_or_default()
    }
}

/// An app-process windowed consumer added to the graph and linked, unwired,
/// from a source's `out1` into its `audio` port.
struct WindowedConsumerLinkedFromOut1 {
    dest_id: String,
    link_id: LinkUniqueId,
    dest_input: Arc<crate::iceoryx2::InputMailboxesInner>,
}

fn add_a_windowed_consumer_linked_from_out1(
    graph: &mut Graph,
    source_id: &str,
) -> WindowedConsumerLinkedFromOut1 {
    use crate::core::test_support::MockWindowedAudioConsumerNode;

    let dest_id = add_mock_windowed_audio_consumer(graph);
    let (_, _, dest_input) =
        attach_mock_instance::<MockWindowedAudioConsumerNode::Node>(graph, &dest_id);
    let link_id = add_link_from_out1_to(graph, source_id, &dest_id, "audio");
    WindowedConsumerLinkedFromOut1 {
        dest_id,
        link_id,
        dest_input: dest_input.expect("a windowed consumer holds input mailboxes"),
    }
}
