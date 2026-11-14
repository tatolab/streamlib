// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A viewer against a loaded stream read as running without the GPU
//! context a real start takes: the source node holds the processor
//! instance a start leaves it with, and nothing compiles.
use crate::core::runtime::a_project_on_disk_for_a_test::options_loading_a_stream_of_the_test_project_at;

use std::time::{Duration, Instant};

use serde_json::Value;
use serial_test::serial;

use streamlib_ipc_types::{MessageReadFromAReservedSlot, OutputPortViewerReader};

use super::*;
use crate::core::graph::{GraphNodeWithComponents, ProcessorInstanceWithItsOutOfProcessLinkWiring};
use crate::core::processors::{ProcessorInstance, ProcessorSpec};
use crate::core::runtime::{Runner, RuntimeOperations};
use crate::core::test_support::{
    MockOutputOnlyProcessor, a_temporary_directory_at_owner_only_mode, ensure_test_mocks_registered,
};
use crate::iceoryx2::{MessageGroupBoundary, OutputWriterInner};
use streamlib_processor_schema::ProcessorClassImportPath;

/// How long a message has to reach a viewer.
const ARRIVES_WITHIN: Duration = Duration::from_secs(5);

/// A stream `a` whose node `camera` runs, writing through the output
/// writer handed back, with no link out of either port.
struct ARunningCameraWithNoLinks {
    _runner: Arc<Runner>,
    stream: Arc<LoadedStreamInThisRuntime>,
    camera_id: crate::core::ProcessorUniqueId,
    camera_node_uuid: String,
    camera_output: Arc<OutputWriterInner>,
    async_runtime: tokio::runtime::Runtime,
    _project_directory: tempfile::TempDir,
}

impl ARunningCameraWithNoLinks {
    fn load() -> Self {
        Self::load_running::<MockOutputOnlyProcessor::Processor>(
            MockOutputOnlyProcessor::processor_class_import_path(),
        )
    }

    /// The stream with a node `camera` of the type `import_path`, `P`'s
    /// processor, running instead of the Records mock.
    fn load_running<P>(import_path: ProcessorClassImportPath) -> Self
    where
        P: crate::core::GeneratedProcessor + 'static,
        P::Config: Default,
    {
        ensure_test_mocks_registered();
        let project_directory = a_temporary_directory_at_owner_only_mode().unwrap();
        let runner = Runner::new().expect("the engine builds");
        let stream = runner
            .load_an_empty_stream(options_loading_a_stream_of_the_test_project_at(
                project_directory.path(),
                "a",
            ))
            .expect("the stream loads");
        let camera_id = stream
            .add_processor(
                ProcessorSpec::new(import_path, Value::Null)
                    .with_display_name("camera".to_string()),
            )
            .expect("the node is added");
        let mut instance = ProcessorInstance::new(Box::new(
            P::from_config(Default::default()).expect("the mock constructs"),
        ));
        instance
            .install_iceoryx2_resources()
            .expect("the mock accepts its iceoryx2 resources");
        let camera_output = instance
            .iceoryx2_output_writer_inner()
            .expect("an output-only mock holds an output writer");
        let camera_node_uuid = stream.compiler.scope(|graph, _tx| {
            let camera = graph
                .traversal_mut()
                .v(&camera_id)
                .first_mut()
                .expect("the node is in the stream");
            ProcessorInstanceWithItsOutOfProcessLinkWiring::from(instance).attach_to(camera);
            camera.node_uuid.to_string()
        });
        Self {
            _runner: runner,
            stream,
            camera_id,
            camera_node_uuid,
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
        self.async_runtime
            .block_on(self.stream.open_a_viewer_of_an_output_port(
                self.camera_node_uuid.clone(),
                port_name.into(),
                None,
            ))
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

fn the_links_of(stream: &LoadedStreamInThisRuntime) -> Value {
    stream.to_json().unwrap()["links"].clone()
}

fn the_next_message_read(reader: &mut OutputPortViewerReader) -> MessageReadFromAReservedSlot {
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
#[serial]
fn a_viewer_reads_an_unlinked_port_without_changing_the_graph() {
    let camera = ARunningCameraWithNoLinks::load();
    let graph_before = the_links_of(&camera.stream);
    assert!(!camera.camera_output.has_channel_publisher("out1"));

    let viewer = camera
        .open_a_viewer("out1")
        .expect("any port of a running node opens a viewer");
    let hand_off = viewer.hand_off().clone();
    assert_eq!(
        (
            hand_off.stream.as_str(),
            &hand_off.node_id,
            hand_off.node_name.as_str(),
            hand_off.port.as_str()
        ),
        ("a", &camera.camera_node_uuid, "camera", "out1")
    );
    assert_eq!(the_links_of(&camera.stream), graph_before);

    let mut reader = OutputPortViewerReader::subscribe(
        &hand_off.iceoryx2_domain_root,
        &hand_off.channel_service_name,
        &hand_off.wake_service_name,
        "a-viewer",
        hand_off.viewer_reads,
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
    assert_eq!(the_links_of(&camera.stream), graph_before);
}

/// A viewer's wake hears nothing from a port its node writes nothing to,
/// and a write wakes a thread already waiting on it, after which the
/// viewer reads the message written.
///
/// Mental-revert: without the wake the waiting thread sleeps its whole
/// wait and answers that nothing was written.
#[test]
#[serial]
fn a_viewers_wake_is_woken_by_a_write_and_hears_nothing_from_a_silent_port() {
    let camera = ARunningCameraWithNoLinks::load();
    let viewer = camera.open_a_viewer("out1").expect("the viewer opens");
    let hand_off = viewer.hand_off().clone();
    let mut reader = OutputPortViewerReader::subscribe(
        &hand_off.iceoryx2_domain_root,
        &hand_off.channel_service_name,
        &hand_off.wake_service_name,
        "a-waiting-viewer",
        hand_off.viewer_reads,
    )
    .expect("the viewer subscribes to the port's channel and its wake");
    let wake = reader.wake();
    assert!(
        !wake
            .wait_for_a_write(Duration::from_millis(50))
            .expect("the wake waits"),
        "a silent port wakes no one"
    );

    let (woken, waited_for) = std::thread::scope(|scope| {
        let waiting = scope.spawn(|| {
            let waited_from = Instant::now();
            let woken = wake
                .wait_for_a_write(ARRIVES_WITHIN)
                .expect("the wake waits");
            (woken, waited_from.elapsed())
        });
        std::thread::sleep(Duration::from_millis(100));
        camera.write(b"woken", 20);
        waiting.join().unwrap()
    });
    assert!(woken, "the write wakes the waiting thread");
    assert!(
        waited_for < Duration::from_secs(2),
        "the wake answers on the write, not when its wait runs out: {waited_for:?}"
    );
    let message = reader
        .read_the_latest_message()
        .expect("the viewer reads")
        .expect("the message written is waiting");
    assert_eq!(
        (&message.payload[..], message.timestamp_ns),
        (&b"woken"[..], 20)
    );
}

/// A viewer names its node by id, so a name or an id the stream does not
/// hold is refused, listing each node's id beside its name, and a port the
/// node lacks is refused listing the ports it has.
#[test]
#[serial]
fn a_viewer_of_a_node_id_or_port_the_stream_does_not_hold_is_refused() {
    let camera = ARunningCameraWithNoLinks::load();

    let by_name = camera
        .async_runtime
        .block_on(camera.stream.open_a_viewer_of_an_output_port(
            "camera".to_string(),
            "out1".into(),
            None,
        ))
        .expect_err("a node's name is not its id")
        .to_string();
    assert!(
        by_name.contains("has the id `camera`")
            && by_name.contains(&format!("`{}` (camera)", camera.camera_node_uuid)),
        "{by_name}"
    );
    let by_its_run_id = camera
        .async_runtime
        .block_on(camera.stream.open_a_viewer_of_an_output_port(
            camera.camera_id.to_string(),
            "out1".into(),
            None,
        ))
        .expect_err("a node's id for one run is not the UUID the graph lists");
    assert!(
        by_its_run_id.to_string().contains("has the id"),
        "{by_its_run_id}"
    );

    let no_such_port = camera
        .open_a_viewer("out9")
        .expect_err("the camera has no port out9")
        .to_string();
    assert!(
        no_such_port.contains("out9") && no_such_port.contains("out1"),
        "{no_such_port}"
    );
}

/// A viewer of a node that does not run is refused naming why: the stream
/// preparing, its playback paused or stopped, or the node's own failure.
#[test]
#[serial]
fn a_viewer_of_a_node_that_does_not_run_is_refused_naming_why() {
    let camera = ARunningCameraWithNoLinks::load();
    let microphone_id = camera
        .stream
        .add_processor(
            ProcessorSpec::new(
                MockOutputOnlyProcessor::processor_class_import_path(),
                Value::Null,
            )
            .with_display_name("microphone".to_string()),
        )
        .expect("the node is added");
    let microphone_node_uuid = camera.stream.compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .v(&microphone_id)
            .first_mut()
            .expect("the node is in the stream")
            .node_uuid
            .to_string()
    });
    let why_the_viewer_is_refused = || match camera
        .async_runtime
        .block_on(camera.stream.open_a_viewer_of_an_output_port(
            microphone_node_uuid.clone(),
            "out1".into(),
            None,
        ))
        .expect_err("a node that does not run has nothing to view")
    {
        Error::NodeOfAStreamIsNotRunning {
            not_running_because,
            runtime_words,
        } => {
            assert!(runtime_words.contains("microphone"), "{runtime_words}");
            not_running_because
        }
        other => panic!("refused for another reason: {other}"),
    };

    assert_eq!(
        why_the_viewer_is_refused(),
        WhyANodeOfAStreamIsNotRunning::Preparing
    );
    *camera.stream.status.lock() = crate::core::runtime::RuntimeStatus::Paused;
    assert_eq!(
        why_the_viewer_is_refused(),
        WhyANodeOfAStreamIsNotRunning::PlaybackPaused
    );
    camera.stream.hold_stopped_without_a_start();
    assert_eq!(
        why_the_viewer_is_refused(),
        WhyANodeOfAStreamIsNotRunning::PlaybackStopped
    );
    camera.stream.compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .v(&microphone_id)
            .first_mut()
            .expect("the node is in the stream")
            .insert(crate::core::graph::StateComponent::new(
                crate::core::graph::ProcessorState::Error,
            ));
    });
    assert_eq!(
        why_the_viewer_is_refused(),
        WhyANodeOfAStreamIsNotRunning::NodeFailed
    );
}

/// A viewer that fell behind reads the latest message and counts every one
/// written since its read before, and never holds the writer back.
#[test]
#[serial]
fn a_viewer_that_fell_behind_reads_the_latest_message_and_counts_what_it_skipped() {
    let camera = ARunningCameraWithNoLinks::load();
    let viewer = camera.open_a_viewer("out1").unwrap();
    let hand_off = viewer.hand_off().clone();
    let mut reader = OutputPortViewerReader::subscribe(
        &hand_off.iceoryx2_domain_root,
        &hand_off.channel_service_name,
        &hand_off.wake_service_name,
        "a-viewer",
        hand_off.viewer_reads,
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

/// A viewer of a Records port reads the latest message, and a viewer of a
/// Channels port reads every block the node wrote since its read before, in
/// order, so a view can play the sound with no gap.
///
/// Mental-revert: on a one-message ring the Channels viewer reads only the
/// last block, the rest overwritten.
#[test]
#[serial]
fn a_viewer_of_a_channels_port_reads_every_block_and_any_other_port_the_latest() {
    use streamlib_ipc_types::{
        ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
    };

    let records_camera = ARunningCameraWithNoLinks::load();
    let records_viewer = records_camera.open_a_viewer("out1").unwrap();
    assert_eq!(
        records_viewer.hand_off().viewer_reads,
        OutputPortViewerReads::LatestMessage
    );
    drop(records_viewer);
    drop(records_camera);

    let tone = ARunningCameraWithNoLinks::load_running::<
        crate::core::test_support::MockChannelsSourceProcessor::Processor,
    >(
        crate::core::test_support::MockChannelsSourceProcessor::processor_class_import_path(),
    );
    let viewer = tone.open_a_viewer("audio").unwrap();
    let hand_off = viewer.hand_off().clone();
    assert_eq!(hand_off.viewer_reads, OutputPortViewerReads::EveryMessage);
    let mut reader = OutputPortViewerReader::subscribe(
        &hand_off.iceoryx2_domain_root,
        &hand_off.channel_service_name,
        &hand_off.wake_service_name,
        "a-listening-viewer",
        hand_off.viewer_reads,
    )
    .unwrap();

    for block_index in 0..8u64 {
        let block = ChannelsBlockDescriptorBody {
            start_sample: block_index * 4,
            sample_rate: 48_000,
            channel_count: 1,
            samples_per_channel: 4,
            sample_type: ChannelsBlockSampleType::F32 as u32,
        };
        let samples: Vec<u8> = (0..4)
            .flat_map(|sample| ((block_index * 4 + sample) as f32).to_le_bytes())
            .collect();
        tone.camera_output
            .write_bytes_described_by(
                "audio",
                &samples,
                block_index as i64,
                MessageGroupBoundary::StartsGroup,
                DataChannelPayloadDescriptor::channels_block(block),
            )
            .expect("the tone writes its block");
    }
    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    let mut blocks_read = Vec::new();
    while blocks_read.len() < 8 {
        blocks_read.extend(reader.read_every_message().expect("the viewer reads"));
        assert!(
            Instant::now() < gave_up_at,
            "not every block reached the viewer"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        blocks_read
            .iter()
            .map(|block| (block.timestamp_ns, block.lost_since_last_read))
            .collect::<Vec<_>>(),
        (0..8)
            .map(|timestamp_ns| (timestamp_ns, 0))
            .collect::<Vec<_>>()
    );
}

/// A viewer reads a Channels block and a Records message whole: each one's
/// descriptor kind and body, its payload as written, and a keyframe-less
/// group flag, with no GPU.
#[test]
#[serial]
fn a_viewer_reads_a_channels_block_and_a_records_message_with_their_descriptors() {
    use streamlib_ipc_types::{
        ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
        DataChannelPayloadDescriptorKind, MessageGroupBoundary as ReadGroupBoundary,
    };

    let camera = ARunningCameraWithNoLinks::load();
    let viewer = camera.open_a_viewer("out1").unwrap();
    let hand_off = viewer.hand_off().clone();
    let mut reader = OutputPortViewerReader::subscribe(
        &hand_off.iceoryx2_domain_root,
        &hand_off.channel_service_name,
        &hand_off.wake_service_name,
        "a-viewer",
        hand_off.viewer_reads,
    )
    .unwrap();

    let block = ChannelsBlockDescriptorBody {
        start_sample: 480,
        sample_rate: 48_000,
        channel_count: 2,
        samples_per_channel: 3,
        sample_type: ChannelsBlockSampleType::F32 as u32,
    };
    let planar_samples: Vec<u8> = [0.0f32, 0.5, -0.5, 1.0, -1.0, 0.25]
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    camera
        .camera_output
        .write_bytes_described_by(
            "out1",
            &planar_samples,
            10,
            MessageGroupBoundary::StartsGroup,
            DataChannelPayloadDescriptor::channels_block(block),
        )
        .expect("the camera writes the block");
    let message = the_next_message_read(&mut reader);
    assert_eq!(
        message.payload_descriptor.known_kind().ok(),
        Some(DataChannelPayloadDescriptorKind::ChannelsBlock)
    );
    assert_eq!(
        ChannelsBlockDescriptorBody::from_descriptor_body(&message.payload_descriptor.body),
        block
    );
    assert_eq!(block.planar_payload_byte_len(), Some(message.payload.len()));
    assert_eq!(
        (message.payload, message.timestamp_ns),
        (planar_samples, 10)
    );

    #[derive(serde::Serialize)]
    struct DetectedFace {
        label: &'static str,
        score: f64,
    }
    let record = rmp_serde::to_vec_named(&DetectedFace {
        label: "face",
        score: 0.875,
    })
    .unwrap();
    camera
        .camera_output
        .write_bytes_described_by(
            "out1",
            &record,
            20,
            MessageGroupBoundary::ContinuesGroup,
            DataChannelPayloadDescriptor::records_message(),
        )
        .expect("the camera writes the record");
    let message = the_next_message_read(&mut reader);
    assert_eq!(
        message.payload_descriptor.known_kind().ok(),
        Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    );
    assert_eq!(message.group_boundary, ReadGroupBoundary::ContinuesGroup);
    let read_back: serde_json::Value = rmp_serde::from_slice(&message.payload).unwrap();
    assert_eq!(
        read_back,
        serde_json::json!({"label": "face", "score": 0.875})
    );
}

/// A viewer holds the frame an Image message names through the
/// surface-sharing socket its hand-off names, claimed in its pool's claim
/// table until it drops, and imports the registration the engine names
/// for its display — here the slot itself, so it reads the very memory
/// the engine wrote. Needs a Vulkan device that exports OPAQUE_FD memory;
/// skips where none comes up.
#[cfg(target_os = "linux")]
#[test]
#[serial]
fn a_viewer_imports_the_registration_the_engine_names_for_an_image_message() {
    use streamlib_consumer_rhi::ConsumerVulkanDevice;
    use streamlib_gpu_frame_import_client::GpuPoolFrameImporterOfAViewer;
    use streamlib_ipc_types::{
        DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind,
        GpuPoolSlotFrameDescriptorBody, ImageFrameDescriptorBody, ImageFrameH273Colour,
    };

    use crate::core::context::GpuPoolClaimTablesOfOnePool;
    use crate::core::rhi::{PixelBuffer, PixelFormat};
    use crate::vulkan::rhi::{HostVulkanBuffer, HostVulkanDevice};

    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 16;
    const BYTE_SIZE: u64 = (WIDTH * 4 * HEIGHT) as u64;
    const STAMP: u8 = 0x5A;
    const POOL_SLOT_KEY: &str = "a-viewed-pool-slot";

    let host_device = match HostVulkanDevice::new() {
        Ok(host_device) => host_device,
        Err(unavailable) => {
            tracing::warn!("skipping: no host Vulkan device ({unavailable})");
            return;
        }
    };
    let exported = match HostVulkanBuffer::new_opaque_fd_export(&host_device, BYTE_SIZE) {
        Ok(exported) => exported,
        Err(unavailable) => {
            tracing::warn!("skipping: no OPAQUE_FD buffer export on this device ({unavailable})");
            return;
        }
    };
    // SAFETY: the export is a live mapping of BYTE_SIZE bytes.
    unsafe { std::ptr::write_bytes(exported.mapped_ptr(), STAMP, BYTE_SIZE as usize) };
    let pool_slot = PixelBuffer::from_host_vulkan_buffer(
        Arc::new(exported),
        WIDTH,
        HEIGHT,
        4,
        PixelFormat::Bgra32,
    );

    /// The engine's answer for a slot already in memory the viewer reads.
    struct TheFramesOwnPoolSlot;
    impl crate::linux::surface_share::ViewedFrameSource for TheFramesOwnPoolSlot {
        fn surface_share_id_a_viewer_reads_the_frame_through(
            &self,
            frame_id: &str,
            _read_by: streamlib_surface_client::ViewedFrameReadBy,
            _layout_the_producer_left_the_frame_in: Option<streamlib_consumer_rhi::VulkanLayout>,
        ) -> crate::core::error::Result<String> {
            Ok(crate::core::rhi::pool_slot_key_of_surface_id(frame_id).to_string())
        }
    }

    let camera = ARunningCameraWithNoLinks::load();
    let engine_resources = Arc::clone(camera.stream.engine_resources_shared_by_every_stream());
    engine_resources.serve_viewed_frames_from(Arc::new(TheFramesOwnPoolSlot));
    let engine_surface_store = engine_resources
        .a_connected_surface_store_owned_by("engine:a-viewed-pool".to_string())
        .expect("the engine's surface store connects");
    engine_surface_store
        .register_buffer(POOL_SLOT_KEY, &pool_slot)
        .expect("the engine registers the pool slot");
    let mut claim_tables_of_the_pool = GpuPoolClaimTablesOfOnePool::named("a-viewed-pool");
    let row_lease = claim_tables_of_the_pool
        .lease_a_row_for_a_slot(
            engine_resources
                .surface_check_out_leases
                .gpu_pool_claim_tables(),
            POOL_SLOT_KEY,
        )
        .expect("the pool leases its slot a row");
    row_lease.row().publish_generation(1);
    engine_resources
        .surface_check_out_leases
        .publish_frame_generation(POOL_SLOT_KEY, 1)
        .expect("the lease table takes the frame");

    let viewer = camera.open_a_viewer("out1").unwrap();
    let hand_off = viewer.hand_off().clone();
    let surface_share_address = hand_off
        .surface_share_address
        .clone()
        .expect("Linux hands every viewer the surface-sharing socket");
    assert_eq!(
        std::path::Path::new(&surface_share_address),
        camera._runner.surface_socket_path()
    );
    let mut reader = OutputPortViewerReader::subscribe(
        &hand_off.iceoryx2_domain_root,
        &hand_off.channel_service_name,
        &hand_off.wake_service_name,
        "a-viewer",
        hand_off.viewer_reads,
    )
    .unwrap();
    camera
        .camera_output
        .write_bytes_described_by(
            "out1",
            &[],
            1,
            MessageGroupBoundary::StartsGroup,
            DataChannelPayloadDescriptor::image_frame(ImageFrameDescriptorBody {
                slot: GpuPoolSlotFrameDescriptorBody {
                    pool: row_lease.table().pool_uuid(),
                    slot: row_lease.row_index(),
                    generation: 1,
                    timeline_value: 0,
                    image_layout: 0,
                },
                width: WIDTH,
                height: HEIGHT,
                format: 0,
                colour: ImageFrameH273Colour::UNSPECIFIED,
            }),
        )
        .expect("the camera writes the frame");

    let message = the_next_message_read(&mut reader);
    assert_eq!(
        message.payload_descriptor.kind,
        DataChannelPayloadDescriptorKind::ImageFrame as u32
    );
    let frame = ImageFrameDescriptorBody::from_descriptor_body(&message.payload_descriptor.body);
    assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));

    let importer = GpuPoolFrameImporterOfAViewer::new(
        surface_share_address,
        streamlib_surface_client::ViewedFrameReadBy::CpuRead,
    );
    let held = importer
        .hold_the_frame(frame.slot)
        .expect("the viewer holds the frame the message names");
    assert!(row_lease.row().is_claimed());
    let pool_slot = held.pool_slot();
    assert_eq!((pool_slot.width(), pool_slot.height()), (WIDTH, HEIGHT));
    assert!(
        pool_slot.dma_buf_planes().is_none(),
        "an OPAQUE_FD export crosses as OPAQUE_FD memory"
    );
    let viewer_device = Arc::new(
        ConsumerVulkanDevice::new_on_the_device_the_runtime_named(
            pool_slot
                .opaque_fd_exporting_device_uuid()
                .expect("OPAQUE_FD memory names its exporting device"),
        )
        .expect("the viewer opens the exporting device"),
    );
    let pixels = pool_slot
        .bytes_for_a_cpu_read(&viewer_device)
        .expect("the viewer reads the slot's memory");
    assert_eq!(pixels.len(), BYTE_SIZE as usize);
    assert!(pixels.iter().all(|byte| *byte == STAMP));
    drop(held);
    assert!(!row_lease.row().is_claimed());
}
