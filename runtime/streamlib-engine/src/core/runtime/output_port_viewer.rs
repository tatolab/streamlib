// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A viewer's read of any output port on the machine: the runtime holds the
//! port's channel open and hands the viewer what it needs to subscribe to the
//! channel's reserved slot from its own process, reading the latest message
//! the port's node wrote without changing the stream's graph.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use iceoryx2::identifiers::UniquePublisherId;

use super::LoadedStreamInThisRuntime;
use super::exposed_output_port_subscription::{
    ExposedOutputPortSourceWiring, LossTallyOfAReservedSlotReader,
    the_running_node_and_its_output_port, the_source_wiring_its_answer_says,
};
use crate::core::compiler::compiler_ops::{
    SourceSideHeldOpenForAReaderOnTheReservedSlot,
    open_the_source_side_for_a_reader_on_the_reserved_slot,
};
use crate::core::error::{Error, Result};
use crate::core::graph::OutputLinkPortRef;
use crate::iceoryx2::{
    ChannelDataServiceSubscriber, ChannelReservedSlotSubscribeError, Iceoryx2Node, Iceoryx2Service,
    MessageReadFromInputPort, data_channel_payload_bytes_of,
};

/// The ring a viewer's subscriber takes: one sample, so a viewer always reads
/// the latest message and the channel overwrites what it fell behind on.
pub const OUTPUT_PORT_VIEWER_RING_DEPTH: usize = 1;

/// How long an open waits for a helper process to open the port it runs.
const A_HELPER_SOURCE_OPENS_ITS_PORT_WITHIN: Duration = Duration::from_secs(10);

/// How often an open asks whether a helper source has answered.
const HOW_OFTEN_AN_OPEN_ASKS_FOR_A_HELPERS_ANSWER: Duration = Duration::from_millis(10);

/// What a viewer needs to read an output port's channel from its own process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPortViewerChannelHandOff {
    /// The stream the port belongs to.
    pub stream: String,
    /// The node the port belongs to.
    pub node: String,
    /// The port's cast name.
    pub port: String,
    /// The root of the iceoryx2 domain the channel lives in.
    pub iceoryx2_domain_root: PathBuf,
    /// The channel's iceoryx2 service name.
    pub channel_service_name: String,
}

/// A viewer's read of one output port, held open by the runtime: the port's
/// channel and its source's side stay open until this drops.
pub struct OutputPortViewerHeldOpen {
    hand_off: OutputPortViewerChannelHandOff,
    source_side: SourceSideHeldOpenForAReaderOnTheReservedSlot,
}

impl std::fmt::Debug for OutputPortViewerHeldOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputPortViewerHeldOpen")
            .field("hand_off", &self.hand_off)
            .finish_non_exhaustive()
    }
}

impl OutputPortViewerHeldOpen {
    /// What the viewer subscribes with.
    pub fn hand_off(&self) -> &OutputPortViewerChannelHandOff {
        &self.hand_off
    }

    fn source_wiring(&self) -> ExposedOutputPortSourceWiring {
        the_source_wiring_its_answer_says(
            self.source_side
                .wire_reply_awaited_from_the_source
                .as_deref(),
        )
    }
}

impl LoadedStreamInThisRuntime {
    /// Hold output port `port_name` of node `node_name` open for a viewer and
    /// say how to subscribe to its channel's reserved slot. The graph is not
    /// changed. Answers once the port's source has opened its end of the
    /// channel, and refused while the node does not run or when its helper
    /// process could not open the port.
    pub(crate) async fn open_a_viewer_of_an_output_port_holding_its_channel(
        &self,
        node_name: &str,
        port_name: &str,
    ) -> Result<OutputPortViewerHeldOpen> {
        let iceoryx2_node = self
            .engine_resources_shared_by_every_stream()
            .iceoryx2_node
            .clone();
        let iceoryx2_domain_root = self
            .engine_resources_shared_by_every_stream()
            .runtime_directory
            .iceoryx2_domain_root();
        let compiler = Arc::clone(&self.compiler);
        let stream_id = self.stream_id().to_string();
        let node_name = node_name.to_string();
        let port_name = port_name.to_string();
        // A stream's graph lock is held across a compile, which may take a
        // processor's whole setup, so the open waits off the async workers.
        let viewer_held_open = tokio::task::spawn_blocking(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                let (node_display_name, port_cast, source_side) =
                    compiler.scope(|graph, _tx| -> Result<_> {
                        let (node, port_cast) = the_running_node_and_its_output_port(
                            graph, &stream_id, &node_name, &port_name,
                        )?;
                        let node_display_name = node.display_name.clone();
                        let source = OutputLinkPortRef::new(node.id.clone(), port_cast.as_str());
                        let source_side = open_the_source_side_for_a_reader_on_the_reserved_slot(
                            graph,
                            &source,
                            &iceoryx2_node,
                        )?;
                        Ok((node_display_name, port_cast, source_side))
                    })?;
                Ok::<_, Error>(OutputPortViewerHeldOpen {
                    hand_off: OutputPortViewerChannelHandOff {
                        stream: stream_id,
                        node: node_display_name,
                        port: port_cast,
                        iceoryx2_domain_root,
                        channel_service_name: source_side.channel_service_name.clone(),
                    },
                    source_side,
                })
            }),
        )
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "a viewer's open of a port failed to join: {join_failure}"
            ))
        })??;
        Self::once_its_source_opened_its_end(viewer_held_open).await
    }

    async fn once_its_source_opened_its_end(
        viewer_held_open: OutputPortViewerHeldOpen,
    ) -> Result<OutputPortViewerHeldOpen> {
        let gave_up_at = tokio::time::Instant::now() + A_HELPER_SOURCE_OPENS_ITS_PORT_WITHIN;
        let refusal = loop {
            match viewer_held_open.source_wiring() {
                ExposedOutputPortSourceWiring::Wired => return Ok(viewer_held_open),
                ExposedOutputPortSourceWiring::RefusedByItsHelper { reason } => {
                    break format!("its helper process could not open the port: {reason}");
                }
                ExposedOutputPortSourceWiring::AwaitingItsHelpersAnswer
                    if tokio::time::Instant::now() >= gave_up_at =>
                {
                    break format!(
                        "its helper process did not open the port within {}s",
                        A_HELPER_SOURCE_OPENS_ITS_PORT_WITHIN.as_secs()
                    );
                }
                ExposedOutputPortSourceWiring::AwaitingItsHelpersAnswer => {
                    tokio::time::sleep(HOW_OFTEN_AN_OPEN_ASKS_FOR_A_HELPERS_ANSWER).await
                }
            }
        };
        let hand_off = viewer_held_open.hand_off.clone();
        tokio::task::spawn_blocking(move || drop(viewer_held_open))
            .await
            .map_err(|join_failure| {
                Error::Runtime(format!(
                    "a refused viewer's release failed to join: {join_failure}"
                ))
            })?;
        Err(Error::Link(format!(
            "port `{}` of node `{}` of the stream `{}` opens no viewer: {refusal}",
            hand_off.port, hand_off.node, hand_off.stream
        )))
    }
}

/// A viewer's subscriber on an output port's channel, in the viewer's own
/// process: each read answers the latest message the port's node wrote and how
/// many it skipped since the read before.
pub struct OutputPortViewerReader {
    subscriber: ChannelDataServiceSubscriber,
    loss_tally: LossTallyOfAReservedSlotReader<UniquePublisherId>,
    _service: Iceoryx2Service,
    _iceoryx2_node: Iceoryx2Node,
}

impl OutputPortViewerReader {
    /// Subscribe to the reserved slot of channel `channel_service_name` in the
    /// iceoryx2 domain rooted at `iceoryx2_domain_root`, from an iceoryx2 node of
    /// this process named `viewer_node_name`.
    pub fn subscribe(
        iceoryx2_domain_root: &Path,
        channel_service_name: &str,
        viewer_node_name: &str,
    ) -> Result<Self> {
        let iceoryx2_node = Iceoryx2Node::new(iceoryx2_domain_root, viewer_node_name)?;
        let service = iceoryx2_node
            .open_existing_channel_service(channel_service_name)?
            .ok_or_else(|| {
                Error::Link(format!(
                    "channel `{channel_service_name}` is not open, so a viewer has nothing to \
                     read: open the viewer first, and keep it open while it reads"
                ))
            })?;
        let subscriber = service
            .create_reserved_slot_subscriber(OUTPUT_PORT_VIEWER_RING_DEPTH)
            .map_err(|refusal| match refusal {
                ChannelReservedSlotSubscribeError::ReservedSlotOccupied => {
                    Error::ReservedPublisherSubscriberSlotOccupied(channel_service_name.to_string())
                }
                ChannelReservedSlotSubscribeError::Transport(detail) => Error::Runtime(format!(
                    "a viewer could not subscribe to the reserved slot of channel \
                     '{channel_service_name}': {detail}"
                )),
            })?;
        Ok(Self {
            subscriber,
            loss_tally: LossTallyOfAReservedSlotReader::default(),
            _service: service,
            _iceoryx2_node: iceoryx2_node,
        })
    }

    /// The latest message the port's node wrote since the read before, or
    /// `None` when it wrote nothing since. Its `lost_since_last_read` counts
    /// every message written in between that the viewer did not read.
    pub fn read_the_latest_message(&mut self) -> Result<Option<MessageReadFromInputPort>> {
        let mut latest: Option<MessageReadFromInputPort> = None;
        loop {
            let sample = self.subscriber.receive().map_err(|receive_failure| {
                Error::Runtime(format!(
                    "a viewer's subscriber failed to receive: {receive_failure:?}"
                ))
            })?;
            let Some(sample) = sample else {
                break;
            };
            let Some(mut message) = self.loss_tally.message_from_a_bag_headed(
                sample.origin(),
                *sample.user_header(),
                data_channel_payload_bytes_of(sample.payload()),
            ) else {
                continue;
            };
            if let Some(passed_over) = latest.take() {
                message.lost_since_last_read = message
                    .lost_since_last_read
                    .saturating_add(passed_over.lost_since_last_read + 1);
            }
            latest = Some(message);
        }
        Ok(latest)
    }
}

#[cfg(test)]
mod tests {
    //! A viewer against a loaded stream read as running without the GPU
    //! context a real start takes: the source node holds the processor
    //! instance a start leaves it with, and nothing compiles.

    use std::time::{Duration, Instant};

    use serde_json::Value;

    use super::*;
    use crate::core::graph::ProcessorInstanceWithItsOutOfProcessLinkWiring;
    use crate::core::processors::{ProcessorInstance, ProcessorSpec};
    use crate::core::runtime::{OptionsForLoadingOneStream, Runner, RuntimeOperations};
    use crate::core::test_support::{
        MockOutputOnlyProcessor, a_temporary_directory_at_owner_only_mode,
        ensure_test_mocks_registered,
    };
    use crate::iceoryx2::{MessageGroupBoundary, OutputWriterInner};

    /// How long a message has to reach a viewer.
    const ARRIVES_WITHIN: Duration = Duration::from_secs(5);

    /// A stream `a` whose node `camera` runs, writing through the output
    /// writer handed back, with no link out of either port.
    struct ARunningCameraWithNoLinks {
        _runner: Arc<Runner>,
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
                        .with_stream_id("a"),
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
                _runner: runner,
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

        fn open_a_viewer(&self, port_name: &str) -> Result<OutputPortViewerHeldOpen> {
            self.async_runtime.block_on(
                self.stream
                    .open_a_viewer_of_an_output_port("camera".into(), port_name.into()),
            )
        }

        fn write(&self, payload: &[u8], timestamp_ns: i64) {
            self.camera_output
                .write_bytes_with_timestamp(
                    "out1",
                    payload,
                    timestamp_ns,
                    MessageGroupBoundary::StartsGroup,
                )
                .expect("the camera writes");
        }
    }

    fn the_links_and_exposures_of(stream: &LoadedStreamInThisRuntime) -> (Value, Value) {
        let graph = stream.to_json().unwrap();
        (graph["links"].clone(), graph["exposed"].clone())
    }

    fn the_next_message_read(
        reader: &mut OutputPortViewerReader,
    ) -> crate::iceoryx2::MessageReadFromInputPort {
        let gave_up_at = Instant::now() + ARRIVES_WITHIN;
        loop {
            if let Some(message) = reader.read_the_latest_message().expect("the viewer reads") {
                return message;
            }
            assert!(Instant::now() < gave_up_at, "no message reached the viewer");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A viewer of a port no link reads, and that is not exposed, reads what
    /// the port's node writes from its own iceoryx2 node, and the graph keeps
    /// no trace of it.
    #[test]
    fn a_viewer_reads_an_unlinked_port_without_changing_the_graph() {
        let camera = ARunningCameraWithNoLinks::load();
        let graph_before = the_links_and_exposures_of(&camera.stream);
        assert!(!camera.camera_output.has_channel_publisher("out1"));

        let viewer = camera
            .open_a_viewer("out1")
            .expect("any port of a running node opens a viewer");
        let hand_off = viewer.hand_off().clone();
        assert_eq!(
            (
                hand_off.stream.as_str(),
                hand_off.node.as_str(),
                hand_off.port.as_str()
            ),
            ("a", "camera", "out1")
        );
        assert_eq!(the_links_and_exposures_of(&camera.stream), graph_before);

        let mut reader = OutputPortViewerReader::subscribe(
            &hand_off.iceoryx2_domain_root,
            &hand_off.channel_service_name,
            "a-viewer",
        )
        .expect("the viewer subscribes to the port's channel");
        camera.write(b"first", 10);
        let message = the_next_message_read(&mut reader);
        assert_eq!(
            (
                &message.payload[..],
                message.timestamp_ns,
                message.lost_since_last_read
            ),
            (&b"first"[..], 10, 0)
        );

        drop(reader);
        drop(viewer);
        assert!(
            !camera.camera_output.has_channel_publisher("out1"),
            "the port's channel goes with its viewer"
        );
        assert_eq!(the_links_and_exposures_of(&camera.stream), graph_before);
    }

    /// A viewer that fell behind reads the latest message and counts every one
    /// written since its read before, and never holds the writer back.
    #[test]
    fn a_viewer_that_fell_behind_reads_the_latest_message_and_counts_what_it_skipped() {
        let camera = ARunningCameraWithNoLinks::load();
        let viewer = camera.open_a_viewer("out1").unwrap();
        let hand_off = viewer.hand_off().clone();
        let mut reader = OutputPortViewerReader::subscribe(
            &hand_off.iceoryx2_domain_root,
            &hand_off.channel_service_name,
            "a-viewer",
        )
        .unwrap();

        camera.write(b"frame 1", 1);
        assert_eq!(the_next_message_read(&mut reader).lost_since_last_read, 0);
        for timestamp_ns in 2..=50 {
            camera.write(format!("frame {timestamp_ns}").as_bytes(), timestamp_ns);
        }
        let message = the_next_message_read(&mut reader);
        assert_eq!(
            (
                &message.payload[..],
                message.timestamp_ns,
                message.lost_since_last_read
            ),
            (&b"frame 50"[..], 50, 48)
        );
        assert!(reader.read_the_latest_message().unwrap().is_none());
    }
}
