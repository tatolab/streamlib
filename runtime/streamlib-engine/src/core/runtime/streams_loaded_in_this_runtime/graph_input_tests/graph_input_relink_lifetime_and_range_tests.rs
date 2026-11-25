// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A stream's graph inputs as a node reading one is linked again, as a reconcile changes the
//! lifetime of one, and as an Array value is checked against a range.

use parking_lot::Mutex;

use super::*;
use crate::core::graph::LinkUniqueId;
use crate::core::runtime::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::{
    KeptNodesLeftToRebuild, LinksAReconcileMakes,
};

/// Every record a [`GraphInputTestRecordsRecorder`] has read.
static RECORDS_READ: Mutex<Vec<Value>> = Mutex::new(Vec::new());

/// A reader of Records on its input `cue`, which records each one it reads.
#[crate::node(
    execution = reactive,
    input("cue", delivery_profile = "newest", pin_type = String, flow = Stream)
)]
pub(crate) struct GraphInputTestRecordsRecorder;

impl crate::core::ReactiveNode for GraphInputTestRecordsRecorder::Node {
    fn process(
        &mut self,
        _ctx: &crate::core::context::RuntimeContextLimitedAccess<'_>,
    ) -> crate::core::error::Result<()> {
        while let Some(message) = self.inputs.read_raw("cue")? {
            let record: Value =
                rmp_serde::from_slice(message.payload.bytes()).expect("a Records message decodes");
            RECORDS_READ.lock().push(record);
        }
        Ok(())
    }
}

fn register_the_recorder_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| {
        crate::core::nodes::NODE_REGISTRY.register::<GraphInputTestRecordsRecorder::Node>()
    });
}

/// Wait until a recorder has read `record`, failing when none does within the wait.
fn a_recorder_reads(record: &Value) {
    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    while !RECORDS_READ.lock().contains(record) {
        assert!(Instant::now() < gave_up_at, "no recorder read {record}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The id of the one link of `stream`.
fn the_one_link_of(stream: &LoadedStreamInThisRuntime) -> LinkUniqueId {
    let rendered = stream.to_json().unwrap();
    let links = rendered["links"].as_array().unwrap();
    assert_eq!(links.len(), 1, "{rendered}");
    LinkUniqueId::from(links[0]["id"].as_str().expect("a running link has an id"))
}

/// A node linked to a `GraphInput` again, after the link it read through was released, reads the
/// value the Input carries though it never changed.
///
/// Mental-revert: this locks keep-last on the `GraphInput` `value` output; an output that keeps
/// no last message for a reader linked later leaves the node linked again reading nothing until
/// the value changes.
#[test]
#[serial]
fn a_reader_linked_again_reads_the_value_a_graph_input_carries() {
    register_the_recorder_once();
    let project_directory = a_project_on_disk();
    let cue = Uuid::new_v4();
    let recorder = Uuid::new_v4();
    let carried = json!({ "track": format!("intro-{}", Uuid::new_v4()) });
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    stream
        .load_graph_snapshot_into_this_stream(
            &GraphSnapshot::from_graph_document(json!({
                "nodes": [
                    a_graph_input_in_a_document(
                        cue, "cue",
                        json!({ "port": "cue", "default": carried }),
                    ),
                    { "id": recorder, "name": "recorder",
                      "type": GraphInputTestRecordsRecorder::node_class_import_path().as_str() },
                ],
                "links": [{
                    "source": { "node": "cue", "port": VALUE },
                    "target": { "node": "recorder", "port": "cue" },
                }],
            }))
            .unwrap(),
            None,
        )
        .expect("the graph loads");
    play(&stream);
    a_recorder_reads(&carried);

    stream
        .disconnect(&the_one_link_of(&stream))
        .expect("the link is released");
    RECORDS_READ.lock().retain(|record| record != &carried);
    stream
        .connect(
            OutputLinkPortRef::new(the_run_id_of(&stream, cue), VALUE),
            InputLinkPortRef::new(the_run_id_of(&stream, recorder), "cue"),
        )
        .expect("the reader is linked again");

    a_recorder_reads(&carried);
    stream.stop().expect("the stream ends");
}

/// A `GraphInput` whose `lifetime` a reconcile of its stream's document changes from `reset` to
/// `kept` holds its live value as kept: the project's store keeps it, and it survives a Stop.
///
/// Mental-revert: a config compiled right away that holds no live value under the lifetime it
/// changed to keeps the value `reset`, forgotten at the Stop.
#[test]
#[serial]
fn a_graph_input_whose_lifetime_a_reconcile_makes_kept_keeps_its_live_value_across_a_stop() {
    let project_directory = a_project_on_disk();
    let fader = Uuid::new_v4();
    let the_fader_of_lifetime = |lifetime: &str| {
        json!({ "nodes": [a_graph_input_in_a_document(
            fader,
            "fader",
            json!({ "port": "fader", "pin_type": {"type": "tensor"}, "default": 0.0, "lifetime": lifetime }),
        )] })
    };
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    stream
        .load_graph_snapshot_into_this_stream(
            &GraphSnapshot::from_graph_document(the_fader_of_lifetime("reset")).unwrap(),
            None,
        )
        .expect("the graph loads");
    play(&stream);
    let project_store = the_project_store_of(&stream, project_directory.path());
    set_live(&stream, fader, json!(0.3)).expect("the value is set live");

    stream
        .reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
            the_fader_of_lifetime("kept"),
            LinksAReconcileMakes::EachAsASavedGraphsLink,
            KeptNodesLeftToRebuild::HandedTheirValuesLive,
        )
        .expect("the reconcile changes the lifetime");

    assert_eq!(
        the_value_the_project_store_keeps_live(&project_store, fader),
        Some(json!(0.3))
    );
    stream
        .stop_playback_keeping_the_graph()
        .expect("the stream stops");
    assert_eq!(the_live_value_of(&stream, fader), Some(json!(0.3)));
    stream.stop().expect("the stream ends");
}

/// A `range` bounds every number of an Array value: one holding a number outside it is refused
/// naming the first such number, and one whose every number is inside it is taken.
///
/// Mental-revert: checking the range only of a value that is itself a number takes `[2]`.
#[test]
#[serial]
fn a_graph_input_refuses_an_array_value_holding_a_number_outside_its_range() {
    let project_directory = a_project_on_disk();
    let fader = Uuid::new_v4();
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            fader,
            "fader",
            json!({ "port": "fader", "pin_type": {"type": "tensor"}, "range": [0, 1], "default": 0 }),
        )],
    );
    play(&stream);

    for (value, first_outside) in [(json!([2]), "holds 2,"), (json!([0.5, 3, -1]), "holds 3,")] {
        match set_live(&stream, fader, value.clone()) {
            Err(LiveInputValueSetRefusal::TheInputDoesNotTakeIt { refusal, .. }) => {
                assert!(
                    refusal.words.contains(first_outside) && refusal.words.contains("from 0 to 1"),
                    "{}",
                    refusal.words
                );
            }
            other => panic!("{value} is refused: {other:?}"),
        }
        assert_eq!(the_live_value_of(&stream, fader), None);
    }
    set_live(&stream, fader, json!([0.25, 1])).expect("an Array value inside the range is taken");
    stream.stop().expect("the stream ends");
}

/// A live value the project's store holds for a `GraphInput` wins over the value it kept under
/// its old key before its value was an input: the input reads the live value, and the old key
/// is forgotten.
///
/// Mental-revert: a carry that copies the old value over the live one leaves the input reading
/// the older value.
#[test]
#[serial]
fn a_live_value_held_already_wins_over_the_value_a_graph_input_kept_under_its_old_key() {
    let project_directory = a_project_on_disk();
    let lights = Uuid::new_v4();
    let old_key = format!("{lights}/value");
    let node_store = ProjectStoreTableName::named("node_store");
    let (_runner, stream) = a_runner_holding_the_stream_of_the_project_at(project_directory.path());
    let project_store = the_project_store_of(&stream, project_directory.path());
    project_store
        .write(node_store, &old_key, &json!({ "level": 0.1 }))
        .unwrap();
    project_store
        .write(
            LIVE_INPUT_VALUES_TABLE,
            &key_of_the_node(lights, VALUE),
            &json!({ "level": 0.9 }),
        )
        .unwrap();

    load_the_graph_inputs(
        &stream,
        vec![a_graph_input_in_a_document(
            lights,
            "lights",
            json!({ "port": "lights", "default": { "level": 0.0 } }),
        )],
    );

    assert_eq!(
        the_live_value_of(&stream, lights),
        Some(json!({ "level": 0.9 }))
    );
    assert_eq!(project_store.read(node_store, &old_key).unwrap(), None);
}
