// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `TensorToTexture` on a playing stream writes each Image frame in the `format` and from the
//! `attribute` it holds, and one set through `transact` while it plays reaches its next frame
//! with no rebuild.

use std::collections::BTreeMap;

use parking_lot::Mutex;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::iceoryx2::{
    ArrayFrameCpuAttributeToPack, DataChannelPayloadDescriptor, ImageFrameDescriptorBody,
    InputMailboxes, MessageGroupBoundary, OutputWriter, array_frame_payload_of_cpu_attributes,
};
use streamlib::sdk::rhi::TextureFormat;
use streamlib_media_builtins::TensorToTexture;

use super::*;

/// How long a value set may take to reach the frames the reader reads.
const READ_WITHIN: Duration = Duration::from_secs(10);

const SOURCE_NODE_UUID: &str = "8a8a8a8a-8a8a-4a8a-8a8a-8a8a8a8a8a8a";
const TO_TEXTURE_NODE_UUID: &str = "8b8b8b8b-8b8b-4b8b-8b8b-8b8b8b8b8b8b";
const READER_NODE_UUID: &str = "8c8c8c8c-8c8c-4c8c-8c8c-8c8c8c8c8c8c";

/// The format and width of the Image frame each stream's reader read last, by stream id.
static READ_BY_STREAM: Mutex<BTreeMap<String, (TextureFormat, u32)>> = Mutex::new(BTreeMap::new());

/// A source writing one Array frame each tick of two CPU `uint8` `[h, w, 4]` attributes: `image`
/// two pixels wide and `picture` four.
#[streamlib::sdk::node(
    execution = continuous(interval_ms = 5),
    output("array", pin_type = Tensor, residency = "cpu"),
)]
fn a_source_of_two_images_in_one_array_frame(
    outputs: OutputWriter,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    let image = [0u8; 2 * 2 * 4];
    let picture = [0u8; 4 * 4 * 4];
    let mut timestamp_ns = 0;
    move |_ctx| {
        let (body, payload) = array_frame_payload_of_cpu_attributes(&[
            ArrayFrameCpuAttributeToPack {
                name: "image",
                dtype: "uint8",
                shape: &[2, 2, 4],
                data: &image,
            },
            ArrayFrameCpuAttributeToPack {
                name: "picture",
                dtype: "uint8",
                shape: &[4, 4, 4],
                data: &picture,
            },
        ])
        .expect("two attributes pack");
        timestamp_ns += 5_000_000;
        outputs.write_bytes_described_by(
            "array",
            &payload,
            timestamp_ns,
            MessageGroupBoundary::StartsGroup,
            DataChannelPayloadDescriptor::array_frame(body),
        )
    }
}

/// A reader recording the format and width of each Image frame on `image`.
#[streamlib::sdk::node(
    execution = reactive,
    input("image", delivery_profile = "newest", pin_type = Texture),
)]
fn a_reader_of_image_frames(
    inputs: InputMailboxes,
    ctx: &RuntimeContextFullAccess<'_>,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    let stream_id = ctx.stream_id().to_owned();
    move |_ctx| {
        while let Some(message) = inputs.read_raw("image")? {
            let image =
                ImageFrameDescriptorBody::from_descriptor_body(&message.payload_descriptor.body);
            let format = TextureFormat::from_repr_value(image.format).expect("a known format");
            READ_BY_STREAM
                .lock()
                .insert(stream_id.clone(), (format, image.width));
        }
        Ok(())
    }
}

fn read_in(stream_id: &str) -> Option<(TextureFormat, u32)> {
    READ_BY_STREAM.lock().get(stream_id).copied()
}

/// A project whose stream `main` links the source into a `TensorToTexture` holding its defaults,
/// linked into the reader.
fn a_project_whose_tensor_to_texture_feeds_a_reader() -> ProjectWithGraphDocuments {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        streamlib_media_builtins::register_media_builtin_node_types();
        NODE_REGISTRY.register::<ASourceOfTwoImagesInOneArrayFrame::Node>();
        NODE_REGISTRY.register::<AReaderOfImageFrames::Node>();
    });
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": SOURCE_NODE_UUID, "name": "source",
              "type": ASourceOfTwoImagesInOneArrayFrame::Node::node_class_import_path().as_str() },
            { "id": TO_TEXTURE_NODE_UUID, "name": "to_texture",
              "type": TensorToTexture::node_class_import_path().as_str() },
            { "id": READER_NODE_UUID, "name": "reader",
              "type": AReaderOfImageFrames::Node::node_class_import_path().as_str() },
        ],
        "links": [
            { "source": { "node": "source", "port": "array" },
              "target": { "node": "to_texture", "port": "array" } },
            { "source": { "node": "to_texture", "port": "image" },
              "target": { "node": "reader", "port": "image" } },
        ],
    }))
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// Set `input` of the `TensorToTexture` to `value` through `transact`, asserting no node is
    /// rebuilt for it.
    async fn set_on_the_tensor_to_texture(
        &self,
        project: &ProjectWithGraphDocuments,
        input: &str,
        value: Value,
    ) {
        let answer = self
            .the_answer_or_the_refusal_of(
                "transact",
                json!({ "author": an_agent_named("alice"), "operations": [
                    { "operation": "set_node_input_value",
                      "stream": project.stream_asset_of("main"),
                      "node": TO_TEXTURE_NODE_UUID, "input": input, "value": value },
                ] }),
            )
            .await
            .unwrap_or_else(|refusal| panic!("`{input}` is set: {refusal:?}"));
        assert!(
            answer.get("nodes_rebuilt").is_none(),
            "no node is rebuilt: {answer}"
        );
    }
}

/// Wait until the reader of `stream_id` reads `wanted`.
async fn the_reader_reads(stream_id: &str, wanted: (TextureFormat, u32)) {
    let deadline = std::time::Instant::now() + READ_WITHIN;
    while read_in(stream_id) != Some(wanted) {
        assert!(
            std::time::Instant::now() < deadline,
            "the reader reads {wanted:?} within {READ_WITHIN:?}; it last read {:?}",
            read_in(stream_id)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Mental-revert: a body building its image format from the default rather than from `format`
/// keeps writing `rgba8_unorm`, and a frame reading the default `attribute` keeps writing the
/// two-pixel `image`.
#[tokio::test(flavor = "multi_thread")]
async fn a_format_and_an_attribute_set_while_playing_reach_the_frames_it_writes() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_tensor_to_texture_feeds_a_reader();
    let (stream_id, _served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;
    the_reader_reads(&stream_id, (TextureFormat::Rgba8Unorm, 2)).await;

    engine
        .set_on_the_tensor_to_texture(&project, "format", json!("bgra8_unorm"))
        .await;
    the_reader_reads(&stream_id, (TextureFormat::Bgra8Unorm, 2)).await;

    engine
        .set_on_the_tensor_to_texture(&project, "attribute", json!("picture"))
        .await;
    the_reader_reads(&stream_id, (TextureFormat::Bgra8Unorm, 4)).await;

    let listing = the_listing_of(&engine.operations_on_the_loaded_streams(), &stream_id).await;
    assert_eq!(listing["state"], "running", "{listing}");
}
