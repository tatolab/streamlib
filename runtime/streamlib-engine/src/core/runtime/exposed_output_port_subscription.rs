// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's one seam for the runtime's publisher: the output ports every
//! loaded stream exposes, read again on each change, and a subscription to one
//! of them through its channel's reserved slot.

use std::sync::{Arc, Weak};

use iceoryx2::identifiers::UniquePublisherId;
use parking_lot::Mutex;

use super::LoadedStreamInThisRuntime;
use super::loaded_stream_output_port_exposure::{
    refuse_a_reader_the_port_does_not_allow, the_output_port_named_on_this_graph,
};
use super::reserved_slot_forwarder::{
    BagReceivedOnTheReservedSlot, ReservedSlotBagForwarding, ReservedSlotForwarder,
    ReservedSlotOfAChannelToForward, start_reserved_slot_forwarder,
};
use super::runtime::EngineResourcesSharedByEveryStream;
use crate::core::compiler::compiler_ops::{
    SourceSideHeldOpenForTheRuntimesPublisher, open_the_source_side_for_the_runtimes_publisher,
};
use crate::core::descriptors::CatalogEntry;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    ExposedOutputPortsComponent, GraphNodeWithComponents, LevelOfAnExposedOutputPortNow,
    OutputLinkPortRef, OutputPortExposureLevel, OutputPortReaderOutsideItsStream,
    ProcessorUniqueId,
};
use crate::core::processors::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
use crate::core::pubsub::{Event, EventListener, PUBSUB, topics};
use crate::iceoryx2::{BagsAGapInTheNumberingSaysWereLost, FrameHeader, MessageReadFromInputPort};

/// One output port a loaded stream exposes, as it stands now.
#[derive(Debug, Clone, PartialEq)]
pub struct ExposedOutputPortOfALoadedStream {
    /// The stream's cast name.
    pub stream_name: String,
    /// The node's name.
    pub node_name: String,
    /// The node's id for this load, which a node removed and added again
    /// under its name does not keep.
    pub node_id: ProcessorUniqueId,
    /// The port's cast name.
    pub port_name: String,
    /// Private or public; an internal port is not exposed.
    pub level: OutputPortExposureLevel,
    /// The port's current catalog entry, if it carries one.
    pub catalog_entry: Option<CatalogEntry>,
}

/// The exposed output ports of every stream loaded in one runtime, read again
/// after each change to them, to their catalog entries, or to the loaded
/// streams. Changes that arrive together are read once.
pub struct ExposureChangeReceiver {
    engine: Weak<EngineResourcesSharedByEveryStream>,
    change_arrived: Arc<tokio::sync::Notify>,
    _listener: Arc<Mutex<dyn EventListener>>,
}

/// Wakes an [`ExposureChangeReceiver`] for every event on
/// [`topics::OUTPUT_PORT_EXPOSURE`].
struct WakeTheExposureChangeReceiver {
    change_arrived: Arc<tokio::sync::Notify>,
}

impl EventListener for WakeTheExposureChangeReceiver {
    fn on_event(&mut self, _event: &Event) -> Result<()> {
        self.change_arrived.notify_one();
        Ok(())
    }
}

impl ExposureChangeReceiver {
    pub(crate) fn subscribed_on(engine: &Arc<EngineResourcesSharedByEveryStream>) -> Result<Self> {
        let change_arrived = Arc::new(tokio::sync::Notify::new());
        // The first read answers at once with the set as it stands.
        change_arrived.notify_one();
        let listener: Arc<Mutex<dyn EventListener>> =
            Arc::new(Mutex::new(WakeTheExposureChangeReceiver {
                change_arrived: Arc::clone(&change_arrived),
            }));
        PUBSUB.subscribe(topics::OUTPUT_PORT_EXPOSURE, Arc::clone(&listener))?;
        Ok(Self {
            engine: Arc::downgrade(engine),
            change_arrived,
            _listener: listener,
        })
    }

    /// The exposed output ports of every loaded stream: at once on the first
    /// call, then once something changed since the previous one. `None` once
    /// the engine is gone.
    pub async fn next_exposed_output_ports(
        &mut self,
    ) -> Option<Vec<ExposedOutputPortOfALoadedStream>> {
        self.changed().await;
        self.the_exposed_output_ports_now().await
    }

    /// Wait until something changed since the previous wait; at once the first
    /// time. Dropping the wait before it ends loses no change.
    pub async fn changed(&mut self) {
        self.change_arrived.notified().await;
    }

    /// The exposed output ports of every loaded stream as they stand, or `None`
    /// once the engine is gone. A read that fails is logged and made again.
    pub async fn the_exposed_output_ports_now(
        &self,
    ) -> Option<Vec<ExposedOutputPortOfALoadedStream>> {
        loop {
            let engine = self.engine.clone();
            // A stream's graph lock is held across a compile, which may take a
            // processor's whole setup, so the read waits off the async workers.
            let read = tokio::task::spawn_blocking(move || {
                Some(
                    engine
                        .upgrade()?
                        .exposed_output_ports_of_every_loaded_stream(),
                )
            })
            .await;
            match read {
                Ok(exposed_output_ports) => return exposed_output_ports,
                Err(read_failure) => {
                    tracing::error!(
                        "the exposed output ports could not be read, and are read again in \
                         {A_FAILED_READ_OF_THE_EXPOSED_SET_IS_MADE_AGAIN_AFTER:?}: {read_failure}"
                    );
                    tokio::time::sleep(A_FAILED_READ_OF_THE_EXPOSED_SET_IS_MADE_AGAIN_AFTER).await;
                }
            }
        }
    }
}

/// How long after a failed read of the exposed set it is read again.
const A_FAILED_READ_OF_THE_EXPOSED_SET_IS_MADE_AGAIN_AFTER: std::time::Duration =
    std::time::Duration::from_secs(1);

impl LoadedStreamInThisRuntime {
    /// Every output port this stream exposes now, with its level and current
    /// catalog entry.
    pub fn exposed_output_ports(&self) -> Vec<ExposedOutputPortOfALoadedStream> {
        self.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(())
                .iter()
                .filter_map(|node| {
                    let exposed_ports = node.get::<ExposedOutputPortsComponent>()?;
                    let catalog_entries = node.catalog_entries_of_output_ports();
                    Some(
                        exposed_ports
                            .exposed_ports_and_their_levels()
                            .map(|(port_name, level)| ExposedOutputPortOfALoadedStream {
                                stream_name: self.stream_name().to_string(),
                                node_name: node.display_name.clone(),
                                node_id: node.id.clone(),
                                port_name: port_name.to_string(),
                                level,
                                catalog_entry: catalog_entries.current_entry_of(port_name),
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .flatten()
                .collect()
        })
    }

    /// Subscribe to output port `port_name` of node `node_name` through its
    /// channel's reserved slot, holding the port's channel open whether or not
    /// a link out of it exists. Refused while the port is internal, while its
    /// node does not run, and while the slot is held.
    pub(crate) async fn subscribe_to_an_exposed_output_port_reading_its_reserved_slot(
        &self,
        node_name: &str,
        port_name: &str,
    ) -> Result<ExposedOutputPortSubscription> {
        let iceoryx2_node = self
            .engine_resources_shared_by_every_stream()
            .iceoryx2_node
            .clone();
        let compiler = Arc::clone(&self.compiler);
        let stream_name = self.stream_name().to_string();
        let node_name = node_name.to_string();
        let port_name = port_name.to_string();
        // A stream's graph lock is held across a compile, which may take a
        // processor's whole setup, so the subscription waits off the async workers.
        tokio::task::spawn_blocking(
            crate::core::logging::carrying_this_threads_loaded_stream_log_route(move || {
                subscribe_holding_the_source_side_open(
                    &compiler,
                    iceoryx2_node,
                    stream_name,
                    &node_name,
                    &port_name,
                )
            }),
        )
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "a subscription to an exposed port failed to join: {join_failure}"
            ))
        })?
    }
}

fn subscribe_holding_the_source_side_open(
    compiler: &crate::core::compiler::Compiler,
    iceoryx2_node: crate::iceoryx2::Iceoryx2Node,
    stream_name: String,
    node_name: &str,
    port_name: &str,
) -> Result<ExposedOutputPortSubscription> {
    let (node_display_name, port_cast, level_now, source_side) =
        compiler.scope(|graph, _tx| -> Result<_> {
            refuse_a_reader_the_port_does_not_allow(
                graph,
                &stream_name,
                node_name,
                port_name,
                OutputPortReaderOutsideItsStream::ElsewhereOnThisMachine,
            )?;
            let (node, port_cast) =
                the_output_port_named_on_this_graph(graph, &stream_name, node_name, port_name)?;
            if !node.runs_its_processor() {
                return Err(Error::Link(format!(
                    "node `{}` of the stream `{stream_name}` is not running, so its port \
                     `{port_cast}` publishes nothing yet",
                    node.display_name,
                )));
            }
            let node_display_name = node.display_name.clone();
            let level_now = node
                .get::<ExposedOutputPortsComponent>()
                .and_then(|exposed_ports| exposed_ports.level_now_of(&port_cast))
                .ok_or_else(|| {
                    Error::Link(format!(
                        "port `{port_cast}` of node `{node_display_name}` is not exposed"
                    ))
                })?;
            let source = OutputLinkPortRef::new(node.id.clone(), port_cast.as_str());
            let source_side =
                open_the_source_side_for_the_runtimes_publisher(graph, &source, &iceoryx2_node)?;
            Ok((node_display_name, port_cast, level_now, source_side))
        })?;
    let forwarder = start_reserved_slot_forwarder(
        ReservedSlotOfAChannelToForward {
            iceoryx2_node,
            channel: source_side.channel_service_name.clone(),
            sizing: source_side.channel_sizing,
            ring_depth: crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
            items_to_forward_before_ending: None,
        },
        ExposedOutputPortMessageForwarding::<UniquePublisherId>::default(),
    )?;
    Ok(ExposedOutputPortSubscription {
        forwarder,
        source_side,
        level_now,
        stream_name,
        node_name: node_display_name,
        port_name: port_cast,
    })
}

/// Whether the source of an exposed port has opened its end of the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExposedOutputPortSourceWiring {
    /// The source publishes on the channel.
    Wired,
    /// The source's helper process has not yet said whether it opened its port.
    AwaitingItsHelpersAnswer,
    /// The source's helper process could not open its port, and said why.
    RefusedByItsHelper { reason: String },
}

/// A live read of one exposed output port: each message its node writes, as
/// written, with the bags lost ahead of it. Dropping it frees the channel's
/// reserved slot, then lets go of the port's channel.
pub struct ExposedOutputPortSubscription {
    // Dropped first: the reader stops before the channel it reads is released.
    forwarder: ReservedSlotForwarder<MessageReadFromInputPort>,
    source_side: SourceSideHeldOpenForTheRuntimesPublisher,
    level_now: LevelOfAnExposedOutputPortNow,
    stream_name: String,
    node_name: String,
    port_name: String,
}

impl std::fmt::Debug for ExposedOutputPortSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExposedOutputPortSubscription")
            .field("stream_name", &self.stream_name)
            .field("node_name", &self.node_name)
            .field("port_name", &self.port_name)
            .finish_non_exhaustive()
    }
}

impl ExposedOutputPortSubscription {
    /// The next message the port's node wrote, or `None` once the reader has
    /// ended.
    pub async fn next_message(&mut self) -> Option<MessageReadFromInputPort> {
        self.forwarder.recv().await
    }

    /// Whether the port's source has opened its end of the channel; a helper
    /// source answers after the subscription returns.
    pub fn source_wiring(&self) -> ExposedOutputPortSourceWiring {
        the_source_wiring_its_answer_says(
            self.source_side
                .wire_reply_awaited_from_the_source
                .as_deref(),
        )
    }

    /// The level the port is at now, set the moment the level is, and
    /// internal for good once the port is no longer exposed or its node left.
    pub fn level_now(&self) -> OutputPortExposureLevel {
        self.level_now.read()
    }

    /// The stream the port belongs to.
    pub fn stream_name(&self) -> &str {
        &self.stream_name
    }

    /// The node the port belongs to.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// The port's cast name.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }
}

/// What a source's answer about opening its end of the channel says, `None`
/// for a source with nothing to answer.
fn the_source_wiring_its_answer_says(
    wire_reply: Option<&OutOfProcessLinkWireReply>,
) -> ExposedOutputPortSourceWiring {
    let Some(wire_reply) = wire_reply else {
        return ExposedOutputPortSourceWiring::Wired;
    };
    match wire_reply.the_far_sides_answer() {
        None => ExposedOutputPortSourceWiring::AwaitingItsHelpersAnswer,
        Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide) => {
            ExposedOutputPortSourceWiring::Wired
        }
        Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide { reason }) => {
            ExposedOutputPortSourceWiring::RefusedByItsHelper { reason }
        }
    }
}

/// An exposed port's forwarding: each bag's payload, stamp and group flag,
/// with every bag lost ahead of it — overwritten in the ring, unreadable, or
/// dropped into a full receiver — told on the next message forwarded. A run is
/// one publisher's unbroken numbering.
struct ExposedOutputPortMessageForwarding<OneUnbrokenRun> {
    bags_the_ring_overwrote: BagsAGapInTheNumberingSaysWereLost<OneUnbrokenRun>,
    lost_ahead_of_the_next_message_forwarded: u64,
}

impl<OneUnbrokenRun> Default for ExposedOutputPortMessageForwarding<OneUnbrokenRun> {
    fn default() -> Self {
        Self {
            bags_the_ring_overwrote: BagsAGapInTheNumberingSaysWereLost::default(),
            lost_ahead_of_the_next_message_forwarded: 0,
        }
    }
}

impl<OneUnbrokenRun: Copy + PartialEq> ExposedOutputPortMessageForwarding<OneUnbrokenRun> {
    fn message_from_a_bag_numbered(
        &mut self,
        run: OneUnbrokenRun,
        sequence_number: u64,
        framed_bag: &[u8],
    ) -> Option<MessageReadFromInputPort> {
        self.lost_ahead_of_the_next_message_forwarded += self
            .bags_the_ring_overwrote
            .how_many_were_lost_before(run, sequence_number);
        let Some(payload) = FrameHeader::read_payload_from_slice(framed_bag) else {
            self.lost_ahead_of_the_next_message_forwarded += 1;
            return None;
        };
        let header = FrameHeader::read_from_slice(framed_bag);
        Some(MessageReadFromInputPort {
            payload_bytes: payload.to_vec(),
            timestamp_ns: header.timestamp_ns,
            group_boundary: header.group_boundary(),
            lost_since_last_read: std::mem::take(
                &mut self.lost_ahead_of_the_next_message_forwarded,
            ),
        })
    }

    fn count_a_message_dropped_into_a_full_receiver(&mut self, dropped: MessageReadFromInputPort) {
        self.lost_ahead_of_the_next_message_forwarded += dropped.lost_since_last_read + 1;
    }
}

impl ReservedSlotBagForwarding for ExposedOutputPortMessageForwarding<UniquePublisherId> {
    type Forwarded = MessageReadFromInputPort;

    fn forwarded_from(
        &mut self,
        bag: BagReceivedOnTheReservedSlot<'_>,
    ) -> Option<MessageReadFromInputPort> {
        self.message_from_a_bag_numbered(bag.publisher, bag.sequence_number, bag.framed_bag)
    }

    fn dropped_into_a_full_receiver(&mut self, dropped: MessageReadFromInputPort, _channel: &str) {
        self.count_a_message_dropped_into_a_full_receiver(dropped);
    }
}

#[cfg(test)]
mod tests {
    //! The seam against a loaded stream read as running without the GPU
    //! context a real start takes: the source node holds the processor
    //! instance a start leaves it with, and nothing compiles.

    use std::time::Duration;

    use serde_json::Value;

    use super::*;
    use crate::core::descriptors::{BinaryEntry, CatalogDataTrackMode};
    use crate::core::graph::ProcessorInstanceWithItsOutOfProcessLinkWiring;
    use crate::core::processors::{ProcessorInstance, ProcessorSpec};
    use crate::core::runtime::{OptionsForLoadingOneStream, Runner, RuntimeOperations};
    use crate::core::test_support::{
        MockOutputOnlyProcessor, a_temporary_directory_at_owner_only_mode,
        ensure_test_mocks_registered,
    };
    use crate::iceoryx2::{FRAME_HEADER_SIZE, MessageGroupBoundary, OutputWriterInner};

    /// How long a message or a change has to arrive.
    const ARRIVES_WITHIN: Duration = Duration::from_secs(5);

    /// A stream `a` whose node `camera` runs, writing through the output
    /// writer handed back, with no link out of either port.
    struct ARunningCameraWithNoLinks {
        runner: Arc<Runner>,
        stream: Arc<LoadedStreamInThisRuntime>,
        camera_output: Arc<OutputWriterInner>,
        async_runtime: tokio::runtime::Runtime,
        _project_directory: tempfile::TempDir,
    }

    impl ARunningCameraWithNoLinks {
        fn load() -> Self {
            ensure_test_mocks_registered();
            let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
            let runner = Runner::new().expect("the engine builds");
            let stream = runner
                .load_an_empty_stream(
                    OptionsForLoadingOneStream::in_project_directory(project_directory.path())
                        .named("a"),
                )
                .expect("the stream loads");
            stream
                .add_processor(
                    ProcessorSpec::new(
                        MockOutputOnlyProcessor::processor_class_import_path(),
                        Value::Null,
                    )
                    .with_display_name("camera".to_string()),
                )
                .expect("the node is added");
            let mut instance = ProcessorInstance::new(Box::new(
                <MockOutputOnlyProcessor::Processor as crate::core::GeneratedProcessor>::from_config(
                    Default::default(),
                )
                .expect("the mock constructs"),
            ));
            instance
                .install_iceoryx2_resources()
                .expect("the mock accepts its iceoryx2 resources");
            let camera_output = instance
                .iceoryx2_output_writer_inner()
                .expect("an output-only mock holds an output writer");
            stream.compiler.scope(|graph, _tx| {
                let camera_id = graph
                    .traversal()
                    .v_with_node_name("camera")
                    .first()
                    .expect("the node is in the stream")
                    .id
                    .clone();
                ProcessorInstanceWithItsOutOfProcessLinkWiring::from(instance).attach_to(
                    graph
                        .traversal_mut()
                        .v(&camera_id)
                        .first_mut()
                        .expect("the node is in the stream"),
                );
            });
            Self {
                runner,
                stream,
                camera_output,
                async_runtime: tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .unwrap(),
                _project_directory: project_directory,
            }
        }

        fn subscribe(&self, port_name: &str) -> Result<ExposedOutputPortSubscription> {
            self.async_runtime.block_on(
                self.stream
                    .subscribe_to_an_exposed_output_port("camera".into(), port_name.into()),
            )
        }

        fn next_message(
            &self,
            subscription: &mut ExposedOutputPortSubscription,
        ) -> MessageReadFromInputPort {
            self.async_runtime
                .block_on(async {
                    tokio::time::timeout(ARRIVES_WITHIN, subscription.next_message()).await
                })
                .expect("a message arrives")
                .expect("the subscription is open")
        }

        fn write(&self, payload: &[u8], timestamp_ns: i64, group_boundary: MessageGroupBoundary) {
            self.camera_output
                .write_bytes_with_timestamp("out1", payload, timestamp_ns, group_boundary)
                .expect("the camera writes");
        }
    }

    fn a_message(
        payload: &[u8],
        timestamp_ns: i64,
        group_boundary: MessageGroupBoundary,
        lost_since_last_read: u64,
    ) -> MessageReadFromInputPort {
        MessageReadFromInputPort {
            payload_bytes: payload.to_vec(),
            timestamp_ns,
            group_boundary,
            lost_since_last_read,
        }
    }

    /// An exposed port no link reads still publishes once subscribed to, each
    /// message as written with its stamp and group flag; dropping the
    /// subscription lets the port's channel go.
    #[test]
    fn a_subscription_reads_what_an_unlinked_exposed_port_writes_as_written() {
        use MessageGroupBoundary::{ContinuesGroup, StartsGroup};
        let camera = ARunningCameraWithNoLinks::load();
        camera
            .stream
            .set_output_port_exposure_level("camera", "out1", OutputPortExposureLevel::Private)
            .unwrap();
        assert!(!camera.camera_output.has_channel_publisher("out1"));

        let mut subscription = camera
            .subscribe("out1")
            .expect("an exposed port subscribes");
        assert_eq!(
            (
                subscription.stream_name(),
                subscription.node_name(),
                subscription.port_name()
            ),
            ("a", "camera", "out1")
        );
        assert!(camera.camera_output.has_channel_publisher("out1"));
        camera.write(b"key", 10, StartsGroup);
        camera.write(b"delta", 20, ContinuesGroup);
        camera.write(b"next key", 30, StartsGroup);
        assert_eq!(
            [(); 3].map(|()| camera.next_message(&mut subscription)),
            [
                a_message(b"key", 10, StartsGroup, 0),
                a_message(b"delta", 20, ContinuesGroup, 0),
                a_message(b"next key", 30, StartsGroup, 0),
            ]
        );

        drop(subscription);
        assert!(
            !camera.camera_output.has_channel_publisher("out1"),
            "the port's channel goes with its last reader"
        );
    }

    /// Messages a reader that fell behind never got are told on the next
    /// message it reads: every message written is read or counted lost.
    #[test]
    fn a_subscription_that_fell_behind_tells_its_losses_on_the_next_message() {
        let camera = ARunningCameraWithNoLinks::load();
        camera
            .stream
            .set_output_port_exposure_level("camera", "out1", OutputPortExposureLevel::Public)
            .unwrap();
        let mut subscription = camera.subscribe("out1").unwrap();
        // The first message read is the numbering's baseline.
        camera.write(b"first", 0, MessageGroupBoundary::StartsGroup);
        assert_eq!(
            camera.next_message(&mut subscription).payload_bytes,
            b"first"
        );

        let written_while_not_reading = 400u64;
        for index in 0..written_while_not_reading {
            camera.write(
                &index.to_le_bytes(),
                index as i64,
                MessageGroupBoundary::StartsGroup,
            );
        }
        std::thread::sleep(Duration::from_millis(200));

        let mut read = 0u64;
        let mut lost = 0u64;
        let mut count = |message: MessageReadFromInputPort| {
            read += 1;
            lost += message.lost_since_last_read;
            message.payload_bytes
        };
        // Everything queued is read before `last`, so the receiver has room for it.
        while let Ok(Some(message)) = camera.async_runtime.block_on(async {
            tokio::time::timeout(Duration::from_millis(200), subscription.next_message()).await
        }) {
            count(message);
        }
        camera.write(b"last", -1, MessageGroupBoundary::StartsGroup);
        while count(camera.next_message(&mut subscription)) != b"last" {}
        assert!(
            lost > 0,
            "a reader that never read past its ring loses messages"
        );
        assert_eq!(
            read + lost,
            written_while_not_reading + 1,
            "every message is read or told as lost"
        );
    }

    /// A subscription reads its port's level the moment it is set, with no
    /// read of the exposed set between; once internal it stays internal, even
    /// when the port is exposed again, since that is another exposure.
    #[test]
    fn a_subscription_reads_a_level_the_moment_it_is_set() {
        use OutputPortExposureLevel::{Internal, Private, Public};
        let camera = ARunningCameraWithNoLinks::load();
        camera
            .stream
            .set_output_port_exposure_level("camera", "out1", Public)
            .unwrap();
        let subscription = camera.subscribe("out1").unwrap();
        assert_eq!(subscription.level_now(), Public);

        for level in [Private, Internal, Public] {
            camera
                .stream
                .set_output_port_exposure_level("camera", "out1", level)
                .unwrap();
            let read_as = if level == Public { Internal } else { level };
            assert_eq!(subscription.level_now(), read_as, "after setting {level}");
        }
    }

    #[test]
    fn an_internal_port_and_a_node_not_running_are_refused() {
        let camera = ARunningCameraWithNoLinks::load();
        let refusal = camera
            .subscribe("out1")
            .expect_err("an internal port is refused");
        assert!(
            matches!(refusal, Error::OutputPortNotExposedToTheReader(_)),
            "{refusal:?}"
        );

        camera
            .stream
            .add_processor(
                ProcessorSpec::new(
                    MockOutputOnlyProcessor::processor_class_import_path(),
                    Value::Null,
                )
                .with_display_name("idle".to_string()),
            )
            .unwrap();
        camera
            .stream
            .set_output_port_exposure_level("idle", "out1", OutputPortExposureLevel::Private)
            .unwrap();
        let refusal = camera
            .async_runtime
            .block_on(
                camera
                    .stream
                    .subscribe_to_an_exposed_output_port("idle".into(), "out1".into()),
            )
            .expect_err("a node that does not run is refused");
        assert!(refusal.to_string().contains("not running"), "{refusal}");
    }

    /// The exposed set answers at once, then again after each change: a
    /// level set, an entry replaced; an internal port is not in it.
    #[test]
    fn the_exposed_set_is_read_at_once_and_again_after_each_change() {
        let camera = ARunningCameraWithNoLinks::load();
        let mut receiver = camera.runner.exposure_changes().unwrap();
        let next_exposed = |receiver: &mut ExposureChangeReceiver| {
            camera
                .async_runtime
                .block_on(async {
                    tokio::time::timeout(ARRIVES_WITHIN, receiver.next_exposed_output_ports()).await
                })
                .expect("the set is read again")
                .expect("the engine is up")
        };
        assert_eq!(next_exposed(&mut receiver), []);

        camera
            .stream
            .set_output_port_exposure_level("camera", "out1", OutputPortExposureLevel::Public)
            .unwrap();
        camera
            .stream
            .set_output_port_exposure_level("camera", "out2", OutputPortExposureLevel::Internal)
            .unwrap();
        let camera_id = camera.stream.the_node_named("camera").unwrap().processor_id;
        let exposed_out1 = ExposedOutputPortOfALoadedStream {
            stream_name: "a".to_string(),
            node_name: "camera".to_string(),
            node_id: camera_id,
            port_name: "out1".to_string(),
            level: OutputPortExposureLevel::Public,
            catalog_entry: None,
        };
        assert_eq!(
            next_exposed(&mut receiver),
            std::slice::from_ref(&exposed_out1)
        );

        let entry: CatalogEntry = BinaryEntry {
            mode: CatalogDataTrackMode::Snapshot,
            mime: Some("application/msgpack".to_string()),
        }
        .into();
        camera.stream.compiler.scope(|graph, _tx| {
            graph
                .traversal()
                .v(())
                .iter()
                .find(|node| node.display_name == "camera")
                .unwrap()
                .catalog_entries_of_output_ports()
                .replace_entry_of("out1", entry.clone())
                .unwrap();
        });
        let mut wait_for_the_entry = || loop {
            let exposed = next_exposed(&mut receiver);
            if exposed[0].catalog_entry.is_some() {
                return exposed;
            }
        };
        assert_eq!(
            wait_for_the_entry(),
            [ExposedOutputPortOfALoadedStream {
                catalog_entry: Some(entry),
                ..exposed_out1
            }]
        );
    }

    /// A gap in the numbering, an unreadable bag and a message dropped into a
    /// full receiver are each told on the next message forwarded.
    #[test]
    fn the_forwarding_tells_a_gap_an_unreadable_bag_and_a_drop_on_the_next_message() {
        let mut forwarding = ExposedOutputPortMessageForwarding::<u64>::default();
        let framed = |payload: &[u8]| {
            let mut bag = vec![0u8; FRAME_HEADER_SIZE + payload.len()];
            FrameHeader::new(
                "out1",
                7,
                payload.len() as u32,
                MessageGroupBoundary::StartsGroup,
            )
            .unwrap()
            .write_to_slice(&mut bag);
            bag[FRAME_HEADER_SIZE..].copy_from_slice(payload);
            bag
        };
        let run = 1u64;
        let first = forwarding
            .message_from_a_bag_numbered(run, 0, &framed(b"a"))
            .unwrap();
        assert_eq!(
            first,
            a_message(b"a", 7, MessageGroupBoundary::StartsGroup, 0)
        );
        assert!(
            forwarding
                .message_from_a_bag_numbered(run, 3, &[0u8; 4])
                .is_none()
        );
        let dropped = forwarding
            .message_from_a_bag_numbered(run, 4, &framed(b"b"))
            .unwrap();
        assert_eq!(
            dropped.lost_since_last_read, 3,
            "two overwritten, one unreadable"
        );
        forwarding.count_a_message_dropped_into_a_full_receiver(dropped);
        assert_eq!(
            forwarding
                .message_from_a_bag_numbered(run, 5, &framed(b"c"))
                .unwrap()
                .lost_since_last_read,
            4,
            "the dropped message and the losses it carried"
        );
    }

    #[test]
    fn the_source_wiring_says_what_a_helper_answered_or_that_none_is_owed() {
        assert_eq!(
            the_source_wiring_its_answer_says(None),
            ExposedOutputPortSourceWiring::Wired
        );
        let owed = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        assert_eq!(
            the_source_wiring_its_answer_says(Some(&owed)),
            ExposedOutputPortSourceWiring::AwaitingItsHelpersAnswer
        );
        owed.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
        assert_eq!(
            the_source_wiring_its_answer_says(Some(&owed)),
            ExposedOutputPortSourceWiring::Wired
        );
        let refused = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        refused.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
            reason: "no such service".to_string(),
        });
        assert_eq!(
            the_source_wiring_its_answer_says(Some(&refused)),
            ExposedOutputPortSourceWiring::RefusedByItsHelper {
                reason: "no such service".to_string()
            }
        );
    }
}
