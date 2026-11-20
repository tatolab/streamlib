// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A value of a module's named type travels through an island as the bytes its class packs: read
//! off a Bytes message a link from outside the island carries, handed to a body run as Python as
//! `bytes`, and handed out as a Bytes message.

use super::*;

const READER_INPUT_PORT: &str = "function_node_island_input_link_from_a_class_node";

/// An object as the bootstrap builds one to run a body as Python: this one hands on the bytes
/// it is handed reversed, and `b"none"` when it is handed `None`.
fn run_as_python_reversing_the_bytes() -> Py<PyAny> {
    Python::initialize();
    Python::attach(|python| {
        let namespace = PyDict::new(python);
        crate::python_class_from_source_for_tests::class_from_source_in_namespace(
            python,
            "class ReversesTheBytesAsPython:\n\
             \x20   def evaluate(self, input_values, now_ns):\n\
             \x20       (reading,) = input_values\n\
             \x20       return [b'none' if reading is None else bytes(reversed(reading))]\n\
             \x20   def state_values(self):\n\
             \x20       return []\n\
             \x20   def resume_from_state_values(self, state_values):\n\
             \x20       pass\n",
            "ReversesTheBytesAsPython",
            &namespace,
        )
        .call0()
        .unwrap()
        .unbind()
    })
}

/// An island of the one node reversing the bytes, its input read off the link from outside the
/// island when `linked`, and holding no value otherwise; and the messages queued at the reader
/// input port that link is read at.
fn an_island_reversing_the_bytes(
    linked: bool,
) -> (
    FunctionNodeIslandEvaluatedInThisHelper,
    Arc<Mutex<Vec<MessageReadFromInputPort>>>,
) {
    let queued: Arc<Mutex<Vec<MessageReadFromInputPort>>> = Arc::default();
    let read_by_the_island = Arc::clone(&queued);
    Python::initialize();
    let nothing = Python::attach(|python| PyNone::get(python).to_owned().unbind());
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "reverse".to_string(),
                0,
                run_as_python_reversing_the_bytes(),
                vec![(None, PinValueTheBootstrapHandsOver::Nothing(nothing))],
                vec![],
                vec![],
                1,
                vec![],
            )],
            (0, 0),
            Box::new(|| 1),
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
                how_each_node_carries_its_values: vec![
                    HowAFunctionNodeCarriesValuesBeyondNumbers {
                        inputs_read_from_links_from_outside_the_island: if linked {
                            vec![(0, READER_INPUT_PORT.to_string(), "named".to_string())]
                        } else {
                            Vec::new()
                        },
                        inputs_that_take_none: vec![0],
                        ..Default::default()
                    },
                ],
                read_the_newest_message_at: Box::new(move |reader_input_port| {
                    assert_eq!(reader_input_port, READER_INPUT_PORT);
                    Ok(read_by_the_island.lock().pop())
                }),
                values_last_read_off_links_from_outside_islands: Arc::default(),
                claim_the_pooled_gpu_frame_a_message_names: Box::new(|_| Ok(None)),
            },
            raises_noted_in_a_record_of_its_own(),
        )
        .unwrap();
    (island, queued)
}

fn a_message_of(
    payload: &[u8],
    payload_descriptor: DataChannelPayloadDescriptor,
) -> MessageReadFromInputPort {
    MessageReadFromInputPort {
        payload: payload.to_vec().into(),
        timestamp_ns: 10,
        group_boundary: streamlib::sdk::iceoryx2::MessageGroupBoundary::StartsGroup,
        lost_since_last_read: 0,
        payload_descriptor,
    }
}

fn a_bytes_message_of(payload: &[u8]) -> FunctionNodeIslandOutputValue {
    FunctionNodeIslandOutputValue::Packed(PackedPinValue {
        payload_descriptor: DataChannelPayloadDescriptor::bytes(),
        payload: payload.to_vec(),
    })
}

/// Mental-revert: without the named value's own reading, an input of value type `named`
/// is refused as one that cannot read a link from outside its island.
#[test]
fn a_named_value_read_off_a_bytes_message_reaches_the_body_as_bytes_and_leaves_as_one() {
    let (island, queued) = an_island_reversing_the_bytes(true);
    queued.lock().push(a_message_of(
        &[1, 2, 3],
        DataChannelPayloadDescriptor::bytes(),
    ));

    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        a_bytes_message_of(&[3, 2, 1])
    );
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        a_bytes_message_of(&[3, 2, 1]),
        "the input keeps the newest value its link carried"
    );
}

#[test]
fn a_named_input_linked_to_nothing_hands_the_body_none() {
    let (island, _queued) = an_island_reversing_the_bytes(false);

    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        a_bytes_message_of(b"none")
    );
}

#[test]
fn a_named_value_read_off_a_message_of_another_kind_is_refused_by_name() {
    let refusal = the_bytes_a_message_of_a_named_type_carries(
        &a_message_of(&[0x80], DataChannelPayloadDescriptor::records_message()),
        "reverse-1",
        "reading",
    )
    .expect_err("a named type travels as a Bytes message");

    let words = refusal.to_string();
    assert!(
        words.contains("`reverse-1`")
            && words.contains("\"reading\"")
            && words.contains("named type"),
        "{words}"
    );
}
