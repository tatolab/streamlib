// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An input choosing a pin's type, set through `transact` while its stream plays, re-types the
//! pin live: the node is never rebuilt and the stream keeps playing; a link the new type no
//! longer fits carries nothing and is reported as `LinkPinTypesDiffer`, recovered by fixing the
//! link; setting the input back makes the link flow again without wiring it again, and removing
//! the link instead clears the report.

use std::collections::BTreeMap;

use parking_lot::Mutex;
use streamlib::sdk::context::RuntimeContextLimitedAccess;
use streamlib::sdk::iceoryx2::OutputWriter;
use streamlib::sdk::nodes::{Input, ReactiveNode};

use super::*;

/// How long a message may take to reach the reader, or the reader to stop reading.
const READ_WITHIN: Duration = Duration::from_secs(10);

/// How long a link held unwired is watched for a message it should not carry.
const WATCHED_FOR: Duration = Duration::from_millis(300);

const SOURCE_NODE_UUID: &str = "7e7e7e7e-7e7e-4e7e-8e7e-7e7e7e7e7e7e";
const READER_NODE_UUID: &str = "7a7a7a7a-7a7a-4a7a-8a7a-7a7a7a7a7a7a";

/// How many times the body of each stream's [`a_source_whose_format_chooses_its_type`] ran, and
/// how many messages its [`StreamToolsTestWordCounter`] read, by stream id.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CountedInOneStream {
    bodies_run: usize,
    words_read: usize,
}

static COUNTED_BY_STREAM: Mutex<BTreeMap<String, CountedInOneStream>> = Mutex::new(BTreeMap::new());

fn count_in(stream_id: &str, count: impl FnOnce(&mut CountedInOneStream)) {
    count(
        COUNTED_BY_STREAM
            .lock()
            .entry(stream_id.to_owned())
            .or_default(),
    );
}

fn counted_in(stream_id: &str) -> CountedInOneStream {
    COUNTED_BY_STREAM
        .lock()
        .get(stream_id)
        .copied()
        .unwrap_or_default()
}

/// A source written as one function whose output `word` carries the type its value input
/// `format` chooses, writing a word each frame whatever that type is.
#[streamlib::sdk::node(
    execution = continuous(interval_ms = 5),
    input("format", description = "The type its word is carried as.", value = Option<PinType>, default = None),
    output("word", pin_type = from_input(format)),
)]
fn a_source_whose_format_chooses_its_type(
    format: Input<Option<PinType>>,
    outputs: OutputWriter,
    ctx: &RuntimeContextFullAccess<'_>,
) -> impl FnMut(&RuntimeContextLimitedAccess<'_>) -> Result<()> {
    let _ = format;
    count_in(ctx.stream_id(), |counted| counted.bodies_run += 1);
    move |_ctx| outputs.write_msgpack("word", &"hello")
}

/// A native reader of Strings on its input `word`, counting each message it reads.
#[streamlib::sdk::node(
    execution = reactive,
    input("word", delivery_profile = "newest", pin_type = String, flow = Stream)
)]
pub struct StreamToolsTestWordCounter;

impl ReactiveNode for StreamToolsTestWordCounter::Node {
    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while self.inputs.read_raw("word")?.is_some() {
            count_in(ctx.stream_id(), |counted| counted.words_read += 1);
        }
        Ok(())
    }
}

/// A project whose stream `main` holds `source`, its `format` choosing a String, linked into the
/// `word` of a [`StreamToolsTestWordCounter`] `reader`.
fn a_project_whose_source_chooses_a_string_for_its_reader() -> ProjectWithGraphDocuments {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        NODE_REGISTRY.register::<ASourceWhoseFormatChoosesItsType::Node>();
        NODE_REGISTRY.register::<StreamToolsTestWordCounter::Node>();
    });
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": SOURCE_NODE_UUID, "name": "source",
              "type": ASourceWhoseFormatChoosesItsType::Node::node_class_import_path().as_str(),
              "input_values": { "format": PinType::String {} } },
            { "id": READER_NODE_UUID, "name": "reader",
              "type": StreamToolsTestWordCounter::node_class_import_path().as_str() },
        ],
        "links": [
            { "source": { "node": "source", "port": "word" },
              "target": { "node": "reader", "port": "word" } },
        ],
    }))
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// Set the `format` of `source` to `format` through `transact`, asserting no node is
    /// rebuilt for it.
    async fn set_the_format_of_the_source(
        &self,
        project: &ProjectWithGraphDocuments,
        format: PinType,
    ) {
        let answer = self
            .the_answer_or_the_refusal_of(
                "transact",
                json!({ "author": an_agent_named("alice"), "operations": [
                    { "operation": "set_node_input_value",
                      "stream": project.stream_asset_of("main"),
                      "node": SOURCE_NODE_UUID, "input": "format", "value": format },
                ] }),
            )
            .await
            .expect("an input choosing a pin type is never refused for its links");
        assert!(
            answer.get("nodes_rebuilt").is_none(),
            "no node is rebuilt: {answer}"
        );
    }

    /// Wait until the reader of `stream_id` has read more than `read_before`.
    async fn the_reader_reads_past(&self, stream_id: &str, read_before: usize) {
        let deadline = std::time::Instant::now() + READ_WITHIN;
        while counted_in(stream_id).words_read <= read_before {
            assert!(
                std::time::Instant::now() < deadline,
                "the reader reads again within {READ_WITHIN:?}; it read {:?}",
                counted_in(stream_id)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Wait until the reader of `stream_id` reads nothing more for [`WATCHED_FOR`].
    async fn the_reader_stops_reading(&self, stream_id: &str) {
        let deadline = std::time::Instant::now() + READ_WITHIN;
        loop {
            let read = counted_in(stream_id).words_read;
            tokio::time::sleep(WATCHED_FOR).await;
            if counted_in(stream_id).words_read == read {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the reader stops reading within {READ_WITHIN:?}; it read {:?}",
                counted_in(stream_id)
            );
        }
    }

    /// The diagnostics of `stream_id`'s condition, as `list_streams` serves them.
    async fn the_diagnostics_of(&self, stream_id: &str) -> Vec<Value> {
        let listing = the_listing_of(&self.operations_on_the_loaded_streams(), stream_id).await;
        assert_eq!(
            listing["state"], "running",
            "the stream still plays: {listing}"
        );
        listing["condition"]["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The state the live graph of `stream_id` serves its one link in.
    fn the_state_of_the_link_of(&self, stream_id: &str) -> Value {
        let live_graph = self.the_live_graph_of(stream_id);
        let links = live_graph["links"].as_array().expect("links").clone();
        assert_eq!(links.len(), 1, "{live_graph}");
        links[0]["state"].clone()
    }
}

/// Mental-revert: a format change refused for the link it no longer fits leaves the pin as it
/// was; one taken without judging the link again keeps carrying words into an input that does
/// not read their type; one that rebuilds the source runs its body twice; and a link held
/// unwired that is not wired again on the change back stays silent.
#[tokio::test(flavor = "multi_thread")]
async fn a_format_change_retypes_the_pin_live_and_a_link_it_no_longer_fits_says_so_until_changed_back()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_source_chooses_a_string_for_its_reader();
    let (stream_id, _served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;
    engine.the_reader_reads_past(&stream_id, 0).await;

    engine
        .set_the_format_of_the_source(&project, PinType::Audio {})
        .await;

    engine.the_reader_stops_reading(&stream_id).await;
    let diagnostics = engine.the_diagnostics_of(&stream_id).await;
    let differing: Vec<&Value> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic["kind"] == "link_pin_types_differ")
        .collect();
    assert_eq!(differing.len(), 1, "{diagnostics:?}");
    assert_eq!(differing[0]["recovery"], "fix_the_link", "{diagnostics:?}");
    assert_eq!(differing[0]["place"]["on"], "link", "{diagnostics:?}");
    let words = differing[0]["words"].as_str().expect("words");
    assert!(
        words.contains("Audio") && words.contains("String"),
        "both pin types are named: {words}"
    );
    assert_eq!(engine.the_state_of_the_link_of(&stream_id), "error");
    assert_the_graph_document_equals_the_running_graph(
        &project.the_graph_document_of("main"),
        &engine.the_live_graph_of(&stream_id),
    );

    let read_while_held = counted_in(&stream_id).words_read;
    engine
        .set_the_format_of_the_source(&project, PinType::String {})
        .await;

    engine
        .the_reader_reads_past(&stream_id, read_while_held)
        .await;
    assert!(
        engine
            .the_diagnostics_of(&stream_id)
            .await
            .iter()
            .all(|diagnostic| diagnostic["kind"] != "link_pin_types_differ"),
        "the link fits again"
    );
    assert_eq!(engine.the_state_of_the_link_of(&stream_id), "wired");
    assert_eq!(
        counted_in(&stream_id).bodies_run,
        1,
        "the source is never rebuilt"
    );
    assert_the_graph_document_equals_the_running_graph(
        &project.the_graph_document_of("main"),
        &engine.the_live_graph_of(&stream_id),
    );
}

/// Mental-revert: a link held unwired whose removal closes a channel it no longer holds fails
/// the edit, or leaves the report standing on a link that is gone.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_whose_pin_types_differ_is_removed_clearing_the_report() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_source_chooses_a_string_for_its_reader();
    let (stream_id, _served) = engine
        .the_stream_main_of_the_project_started_and_served(&project)
        .await;
    engine.the_reader_reads_past(&stream_id, 0).await;
    engine
        .set_the_format_of_the_source(&project, PinType::Audio {})
        .await;
    engine.the_reader_stops_reading(&stream_id).await;

    engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                { "operation": "disconnect", "stream": project.stream_asset_of("main"),
                  "source": { "node": SOURCE_NODE_UUID, "port": "word" },
                  "target": { "node": READER_NODE_UUID, "port": "word" } },
            ] }),
        )
        .await
        .expect("the link is removed");

    assert!(
        engine
            .the_diagnostics_of(&stream_id)
            .await
            .iter()
            .all(|diagnostic| diagnostic["kind"] != "link_pin_types_differ"),
        "the link is gone"
    );
    assert!(
        engine.the_live_graph_of(&stream_id)["links"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert_eq!(counted_in(&stream_id).bodies_run, 1);
    assert_the_graph_document_equals_the_running_graph(
        &project.the_graph_document_of("main"),
        &engine.the_live_graph_of(&stream_id),
    );
}
