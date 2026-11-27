// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use streamlib::sdk::iceoryx2::GpuPoolSlotFrameDescriptorBody;

/// Hands its message input on while its second input is on, else nothing:
/// a message travels as the token of the input holding it, and the slot
/// past its one output says whether it handed one out.
unsafe extern "C" fn hand_the_message_on_while_open(
    input_values: *const f64,
    _config_values: *const f64,
    _state_values: *mut f64,
    output_values: *mut f64,
    _now_ns: i64,
) -> i32 {
    unsafe {
        let is_open = *input_values.add(1) != 0.0;
        *output_values = if is_open { *input_values } else { 0.0 };
        *output_values.add(1) = if is_open { 1.0 } else { 0.0 };
    }
    MODULE_HOST_TABLE_STATUS_OK
}

const READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND: &str =
    "function_node_island_input_link_from_a_class_node";

/// Messages queued at the reader input port a link from outside the
/// island is read at, each read handing out the newest.
fn messages_queued_at_the_reader_input_port() -> (
    Arc<Mutex<Vec<MessageReadFromInputPort>>>,
    NewestMessageReaderOfTheReadersInputPorts,
) {
    let queued: Arc<Mutex<Vec<MessageReadFromInputPort>>> = Arc::new(Mutex::new(Vec::new()));
    let read_by_the_island = Arc::clone(&queued);
    (
        queued,
        Box::new(move |reader_input_port| {
            assert_eq!(
                reader_input_port,
                READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND
            );
            let mut queued = read_by_the_island.lock();
            let newest = queued.pop();
            queued.clear();
            Ok(newest)
        }),
    )
}

fn a_message_of(
    payload: Vec<u8>,
    payload_descriptor: streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor,
    timestamp_ns: i64,
) -> MessageReadFromInputPort {
    MessageReadFromInputPort {
        payload: payload.into(),
        timestamp_ns,
        group_boundary: streamlib::sdk::iceoryx2::MessageGroupBoundary::StartsGroup,
        lost_since_last_read: 2,
        payload_descriptor,
    }
}

fn a_channels_block_of_one_f64_sample(sample: f64, timestamp_ns: i64) -> MessageReadFromInputPort {
    a_message_of(
        sample.to_le_bytes().to_vec(),
        streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::channels_block(
            ChannelsBlockDescriptorBody {
                start_sample: 0,
                sample_rate: 0,
                channel_count: 1,
                samples_per_channel: 1,
                sample_type: ChannelsBlockSampleType::F64 as u32,
            },
        ),
        timestamp_ns,
    )
}

#[test]
fn a_control_value_is_the_first_sample_of_an_f32_or_f64_block_of_any_shape() {
    let samples_of_two_channels_of_three: [f32; 6] = [0.25, 0.5, 0.75, -1.0, -0.5, 0.0];
    let f32_block_of_two_channels_of_three = a_message_of(
        samples_of_two_channels_of_three
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect(),
        streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::channels_block(
            ChannelsBlockDescriptorBody {
                start_sample: 0,
                sample_rate: 48_000,
                channel_count: 2,
                samples_per_channel: 3,
                sample_type: ChannelsBlockSampleType::F32 as u32,
            },
        ),
        10,
    );
    assert_eq!(
        the_control_value_a_message_carries(&f32_block_of_two_channels_of_three, "n", "x").unwrap(),
        0.25
    );
    assert_eq!(
        the_control_value_a_message_carries(&a_channels_block_of_one_f64_sample(7.5, 20), "n", "x")
            .unwrap(),
        7.5
    );
}

#[test]
fn a_control_value_read_off_a_message_that_is_no_channels_block_is_refused_by_name() {
    let records_message = a_message_of(
        vec![0x80],
        streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::records_message(),
        10,
    );
    let refusal = the_control_value_a_message_carries(&records_message, "scale-1", "x")
        .expect_err("a Records message carries no number");
    assert!(
        refusal
            .to_string()
            .contains("function node `scale-1` reads a number")
            && refusal.to_string().contains("is no Channels block"),
        "{refusal}"
    );
}

fn a_gate_reading_a_link_from_outside_its_island(
    is_open: bool,
    stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
    read_the_newest_message_at: NewestMessageReaderOfTheReadersInputPorts,
    claim_the_pooled_gpu_frame_a_message_names: ClaimOfThePooledGpuFrameAMessageNames,
) -> FunctionNodeIslandEvaluatedInThisHelper {
    FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
        vec![(
            "gate".to_string(),
            0,
            compiled_to(hand_the_message_on_while_open),
            vec![
                (None, PinValueTheBootstrapHandsOver::Number(0.0)),
                (
                    None,
                    PinValueTheBootstrapHandsOver::Number(if is_open { 1.0 } else { 0.0 }),
                ),
            ],
            vec![],
            vec![],
            1,
            vec![],
        )],
        (0, 0),
        stream_clock_now_ns,
        None,
        HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
            how_each_node_carries_its_values: vec![HowAFunctionNodeCarriesValuesBeyondNumbers {
                inputs_read_from_links_from_outside_the_island: vec![(
                    0,
                    READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND.to_string(),
                    "unread_message".to_string(),
                )],
                inputs_that_take_none: vec![],
                inputs_carrying_an_unread_message: vec![0],
                outputs_carrying_an_unread_message: vec![0],
                outputs_that_may_hand_out_nothing: vec![0],
                ..Default::default()
            }],
            read_the_newest_message_at,
            values_last_read_off_links_from_outside_islands: Arc::default(),
            claim_the_pooled_gpu_frame_a_message_names,
        },
        raises_noted_in_a_record_of_its_own(),
    )
    .unwrap()
}

#[test]
fn an_open_gate_hands_on_the_newest_message_unread_and_nothing_once_it_is_handed_on() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
    let island = a_gate_reading_a_link_from_outside_its_island(
        true,
        stream_clock_now_ns,
        read_the_newest_message_at,
        Box::new(|_| Ok(None)),
    );
    queued.lock().push(a_message_of(
        vec![1, 2, 3],
        streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::bytes(),
        40,
    ));
    queued.lock().push(a_message_of(
        vec![4, 5],
        streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::bytes(),
        50,
    ));
    stream_time_ns.store(100, Ordering::SeqCst);
    let FunctionNodeIslandOutputValue::MessageHandedOnUnread(handed_on) = island
        .evaluate_the_island_output_for_this_read()
        .unwrap()
        .value
    else {
        panic!("an open gate hands its message on");
    };
    assert_eq!(handed_on.payload_bytes, vec![4, 5]);
    assert_eq!(handed_on.timestamp_ns, 50);
    assert_eq!(handed_on.lost_since_last_read, 2);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Nothing,
        "a second read at the same stream time is not handed the message again"
    );
    stream_time_ns.store(200, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Nothing
    );
}

#[test]
fn a_pooled_gpu_frame_a_message_names_is_claimed_while_the_island_holds_it_and_released_after() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
    let claims_held: Arc<Mutex<Vec<std::sync::Weak<u32>>>> = Arc::new(Mutex::new(Vec::new()));
    let claims_noted_by_the_island = Arc::clone(&claims_held);
    let island = a_gate_reading_a_link_from_outside_its_island(
        false,
        stream_clock_now_ns,
        read_the_newest_message_at,
        Box::new(move |pooled_gpu_frame: GpuPoolSlotFrameDescriptorBody| {
            let claim = Arc::new(pooled_gpu_frame.slot);
            claims_noted_by_the_island
                .lock()
                .push(Arc::downgrade(&claim));
            Ok(Some(Box::new(claim) as PooledGpuFrameClaimAMessageHolds))
        }),
    );
    let pooled_gpu_frame = GpuPoolSlotFrameDescriptorBody {
        pool: [7; 16],
        slot: 3,
        generation: 1,
        timeline_value: 9,
        image_layout: 0,
    };
    queued.lock().push(a_message_of(
        vec![],
        DataChannelPayloadDescriptor::gpu_pool_slot_frame(pooled_gpu_frame),
        10,
    ));
    stream_time_ns.store(100, Ordering::SeqCst);
    island.evaluate_the_island_output_for_this_read().unwrap();
    {
        let claims_held = claims_held.lock();
        assert_eq!(claims_held.len(), 1);
        assert_eq!(claims_held[0].upgrade().as_deref(), Some(&3));
    }
    stream_time_ns.store(200, Ordering::SeqCst);
    island.evaluate_the_island_output_for_this_read().unwrap();
    let claims_held = claims_held.lock();
    assert_eq!(claims_held.len(), 1);
    assert!(claims_held[0].upgrade().is_none());
}

#[test]
fn a_copy_of_an_unread_message_a_state_record_keeps_is_the_same_message() {
    Python::attach(|python| {
        let unread_message = Py::new(
            python,
            UnreadMessageAFunctionNodeHandsOn {
                message: Arc::new(MessageAFunctionNodeIslandHolds {
                    message: MessageAFunctionNodeIslandHandsOnUnread::read_off_its_channel(
                        &a_message_of(vec![1], DataChannelPayloadDescriptor::bytes(), 10),
                    ),
                    _claim_of_its_pooled_gpu_frame: None,
                }),
            },
        )
        .unwrap();
        let copy_module = python.import("copy").unwrap();
        for copy_function in ["copy", "deepcopy"] {
            let copied = copy_module
                .call_method1(copy_function, (unread_message.clone_ref(python),))
                .unwrap();
            assert!(copied.is(unread_message.bind(python)), "{copy_function}");
        }
    });
}

#[test]
fn a_closed_gate_hands_out_nothing_rather_than_the_last_message() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
    let island = a_gate_reading_a_link_from_outside_its_island(
        false,
        stream_clock_now_ns,
        read_the_newest_message_at,
        Box::new(|_| Ok(None)),
    );
    assert_eq!(
        island.the_reader_input_ports_its_links_from_outside_it_arrive_at(),
        [READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND.to_string()],
        "the island names the reader input port its link arrives at, so a message there \
         is judged by what the island hands out for it"
    );
    queued.lock().push(a_message_of(
        vec![1],
        streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::bytes(),
        10,
    ));
    stream_time_ns.store(100, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Nothing
    );
}

#[test]
fn a_number_read_off_a_link_from_outside_the_island_keeps_the_newest_control_value() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "add_one".to_string(),
                0,
                compiled_to(add_one_and_count_calls),
                vec![(None, PinValueTheBootstrapHandsOver::Number(0.0))],
                vec![],
                vec![0.0],
                1,
                vec![],
            )],
            (0, 0),
            stream_clock_now_ns,
            None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
                how_each_node_carries_its_values: vec![
                    HowAFunctionNodeCarriesValuesBeyondNumbers {
                        inputs_read_from_links_from_outside_the_island: vec![(
                            0,
                            READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND.to_string(),
                            "float".to_string(),
                        )],
                        ..Default::default()
                    },
                ],
                read_the_newest_message_at,
                values_last_read_off_links_from_outside_islands: Arc::default(),
                claim_the_pooled_gpu_frame_a_message_names: Box::new(|_| Ok(None)),
            },
            raises_noted_in_a_record_of_its_own(),
        )
        .unwrap();
    stream_time_ns.store(100, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Nothing,
        "a node whose link has carried no value yet hands out nothing"
    );
    queued
        .lock()
        .push(a_channels_block_of_one_f64_sample(3.0, 10));
    queued
        .lock()
        .push(a_channels_block_of_one_f64_sample(5.0, 20));
    stream_time_ns.store(200, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::ControlValue(vec![6.0])
    );
    stream_time_ns.store(300, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::ControlValue(vec![6.0]),
        "a number keeps the newest value its link carried"
    );
}

fn an_add_one_island_reading_a_link_from_outside_its_island(
    stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
    read_the_newest_message_at: NewestMessageReaderOfTheReadersInputPorts,
    values_last_read_off_links_from_outside_islands: ValuesLastReadOffLinksFromOutsideIslands,
) -> FunctionNodeIslandEvaluatedInThisHelper {
    FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
        vec![(
            "add_one".to_string(),
            0,
            compiled_to(add_one_and_count_calls),
            vec![(None, PinValueTheBootstrapHandsOver::Number(0.0))],
            vec![],
            vec![0.0],
            1,
            vec![],
        )],
        (0, 0),
        stream_clock_now_ns,
        None,
        HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
            how_each_node_carries_its_values: vec![HowAFunctionNodeCarriesValuesBeyondNumbers {
                inputs_read_from_links_from_outside_the_island: vec![(
                    0,
                    READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND.to_string(),
                    "float".to_string(),
                )],
                ..Default::default()
            }],
            read_the_newest_message_at,
            values_last_read_off_links_from_outside_islands,
            claim_the_pooled_gpu_frame_a_message_names: Box::new(|_| Ok(None)),
        },
        raises_noted_in_a_record_of_its_own(),
    )
    .unwrap()
}

#[test]
fn an_island_replacing_another_starts_from_the_control_value_its_link_last_carried() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
    let values_last_read: ValuesLastReadOffLinksFromOutsideIslands = Arc::default();
    let island = an_add_one_island_reading_a_link_from_outside_its_island(
        stream_clock_now_ns,
        read_the_newest_message_at,
        Arc::clone(&values_last_read),
    );
    queued
        .lock()
        .push(a_channels_block_of_one_f64_sample(5.0, 10));
    stream_time_ns.store(100, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::ControlValue(vec![6.0])
    );

    let (replacement_stream_time_ns, replacement_stream_clock_now_ns) =
        a_stream_clock_the_test_sets();
    let (_, replacement_reads_the_newest_message_at) = messages_queued_at_the_reader_input_port();
    let replacement = an_add_one_island_reading_a_link_from_outside_its_island(
        replacement_stream_clock_now_ns,
        replacement_reads_the_newest_message_at,
        values_last_read,
    );
    replacement_stream_time_ns.store(200, Ordering::SeqCst);
    assert_eq!(
        replacement
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::ControlValue(vec![6.0])
    );
}

/// Counts its calls in its state and hands out a token naming an input it
/// does not have.
unsafe extern "C" fn count_calls_and_hand_out_a_token_naming_no_message_input(
    _input_values: *const f64,
    _config_values: *const f64,
    state_values: *mut f64,
    output_values: *mut f64,
    _now_ns: i64,
) -> i32 {
    unsafe {
        *state_values += 1.0;
        *output_values = 7.0;
    }
    MODULE_HOST_TABLE_STATUS_OK
}

#[test]
fn a_call_handing_out_a_token_naming_no_message_input_keeps_the_state_it_started_from() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "hand_on".to_string(),
                0,
                compiled_to(count_calls_and_hand_out_a_token_naming_no_message_input),
                vec![(None, PinValueTheBootstrapHandsOver::Number(0.0))],
                vec![],
                vec![0.0],
                1,
                vec![],
            )],
            (0, 0),
            stream_clock_now_ns,
            None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
                how_each_node_carries_its_values: vec![
                    HowAFunctionNodeCarriesValuesBeyondNumbers {
                        inputs_that_take_none: vec![0],
                        inputs_carrying_an_unread_message: vec![0],
                        outputs_carrying_an_unread_message: vec![0],
                        ..Default::default()
                    },
                ],
                read_the_newest_message_at: Box::new(|_| Ok(None)),
                values_last_read_off_links_from_outside_islands: Arc::default(),
                claim_the_pooled_gpu_frame_a_message_names: Box::new(|_| Ok(None)),
            },
            raises_noted_in_a_record_of_its_own(),
        )
        .unwrap();
    stream_time_ns.store(100, Ordering::SeqCst);

    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Nothing,
        "a call handing out a token naming no message input hands out nothing"
    );
    let raises = the_raises_noted_by(&island);
    assert!(
        raises["hand_on"].contains("names none of its message inputs"),
        "{raises:?}"
    );
}

/// An object as the bootstrap builds one to run a body as Python: this one
/// halves the red, green and blue of the Color it is handed as its parts, and
/// answers the dimmed Color as its parts.
fn run_as_python_dimming_a_colour() -> Py<PyAny> {
    Python::initialize();
    Python::attach(|python| {
        let namespace = PyDict::new(python);
        crate::python_class_from_source_for_tests::class_from_source_in_namespace(
            python,
            "class DimsAColourAsPython:\n\
             \x20   def evaluate(self, input_values, now_ns):\n\
             \x20       ((red, green, blue, alpha),) = input_values\n\
             \x20       return [(red / 2, green / 2, blue / 2, alpha)]\n\
             \x20   def state_values(self):\n\
             \x20       return []\n\
             \x20   def resume_from_state_values(self, state_values):\n\
             \x20       pass\n",
            "DimsAColourAsPython",
            &namespace,
        )
        .call0()
        .unwrap()
        .unbind()
    })
}

fn an_island_dimming_a_colour(
    pin_value: PinValueTheBootstrapHandsOver,
    inputs_read_from_links_from_outside_the_island: Vec<(usize, String, String)>,
    read_the_newest_message_at: NewestMessageReaderOfTheReadersInputPorts,
    stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
) -> FunctionNodeIslandEvaluatedInThisHelper {
    FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
        vec![(
            "dim".to_string(),
            0,
            run_as_python_dimming_a_colour(),
            vec![(None, pin_value)],
            vec![],
            vec![],
            1,
            vec![],
        )],
        (0, 0),
        stream_clock_now_ns,
        None,
        HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
            how_each_node_carries_its_values: vec![HowAFunctionNodeCarriesValuesBeyondNumbers {
                inputs_read_from_links_from_outside_the_island,
                ..Default::default()
            }],
            read_the_newest_message_at,
            values_last_read_off_links_from_outside_islands: Arc::default(),
            claim_the_pooled_gpu_frame_a_message_names: Box::new(|_| Ok(None)),
        },
        raises_noted_in_a_record_of_its_own(),
    )
    .unwrap()
}

#[test]
fn a_colour_pin_reaches_a_node_run_as_python_as_its_parts_and_its_colour_is_a_control_value() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let (_queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
    let island = an_island_dimming_a_colour(
        PinValueTheBootstrapHandsOver::Parts(vec![1.0, 0.5, 0.25, 0.75]),
        Vec::new(),
        read_the_newest_message_at,
        stream_clock_now_ns,
    );
    stream_time_ns.store(1, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::ControlValue(vec![0.5, 0.25, 0.125, 0.75])
    );
}

#[test]
fn a_colour_read_off_a_link_from_outside_the_island_takes_a_vector_with_an_alpha_of_1() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
    let island = an_island_dimming_a_colour(
        PinValueTheBootstrapHandsOver::Parts(vec![0.0, 0.0, 0.0, 1.0]),
        vec![(
            0,
            READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND.to_string(),
            "color".to_string(),
        )],
        read_the_newest_message_at,
        stream_clock_now_ns,
    );
    queued.lock().push(a_message_of(
        [0.5f64, 0.25]
            .iter()
            .flat_map(|part| part.to_le_bytes())
            .collect(),
        streamlib::sdk::iceoryx2::DataChannelPayloadDescriptor::channels_block(
            ChannelsBlockDescriptorBody {
                start_sample: 0,
                sample_rate: 0,
                channel_count: 2,
                samples_per_channel: 1,
                sample_type: ChannelsBlockSampleType::F64 as u32,
            },
        ),
        10,
    ));
    stream_time_ns.store(1, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::ControlValue(vec![0.25, 0.125, 0.0, 1.0])
    );
}

#[test]
fn a_transform_read_off_a_control_message_of_another_size_is_refused_naming_both() {
    let refusal = the_parts_a_message_carries(
        &a_channels_block_of_one_f64_sample(1.0, 10),
        Some(&PinType::Transform {}),
        "place-1",
        "x",
    )
    .expect_err("a Transform reads exactly 16 channels");
    assert!(
        refusal
            .to_string()
            .contains("function node `place-1` reads a Transform")
            && refusal.to_string().contains("carries 1"),
        "{refusal}"
    );
}

#[test]
fn each_value_type_of_parts_names_its_pin_type() {
    for (value_type, channel_count) in [
        ("color", 4),
        ("vector2", 2),
        ("vector3", 3),
        ("vector4", 4),
        ("rectangle", 4),
        ("transform", 16),
    ] {
        let pin_type = the_pin_type_a_value_type_carries_in_parts(value_type).unwrap();
        assert_eq!(
            streamlib::sdk::pin_value_packing::channel_count_of(&pin_type),
            Some(channel_count),
            "{value_type}"
        );
    }
    assert_eq!(the_pin_type_a_value_type_carries_in_parts("float"), None);
}

/// An object as the bootstrap builds one to run a body as Python: this one
/// doubles each level of the List of floats it is handed as the record
/// `{"value": [...]}` an island carries a List as, and answers the doubled
/// List as that record.
fn run_as_python_doubling_each_level() -> Py<PyAny> {
    Python::initialize();
    Python::attach(|python| {
        let namespace = PyDict::new(python);
        crate::python_class_from_source_for_tests::class_from_source_in_namespace(
            python,
            "class DoublesEachLevelAsPython:\n\
             \x20   def evaluate(self, input_values, now_ns):\n\
             \x20       ((levels),) = input_values\n\
             \x20       return [{'value': [level * 2 for level in levels['value']]}]\n\
             \x20   def state_values(self):\n\
             \x20       return []\n\
             \x20   def resume_from_state_values(self, state_values):\n\
             \x20       pass\n",
            "DoublesEachLevelAsPython",
            &namespace,
        )
        .call0()
        .unwrap()
        .unbind()
    })
}

const A_LIST_OF_FLOATS: &str = r#"{"type":"list_of","element_type":{"type":"float"}}"#;

fn a_list_of_floats() -> PinType {
    serde_json::from_str(A_LIST_OF_FLOATS).unwrap()
}

fn an_island_doubling_each_level(
    pin_value: PinValueTheBootstrapHandsOver,
    inputs_read_from_links_from_outside_the_island: Vec<(usize, String, String)>,
    read_the_newest_message_at: NewestMessageReaderOfTheReadersInputPorts,
    stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
) -> FunctionNodeIslandEvaluatedInThisHelper {
    FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
        vec![(
            "double".to_string(),
            0,
            run_as_python_doubling_each_level(),
            vec![(None, pin_value)],
            vec![],
            vec![],
            1,
            vec![],
        )],
        (0, 0),
        stream_clock_now_ns,
        None,
        HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
            how_each_node_carries_its_values: vec![HowAFunctionNodeCarriesValuesBeyondNumbers {
                inputs_read_from_links_from_outside_the_island,
                inputs_carrying_a_stored_value: vec![(0, A_LIST_OF_FLOATS.to_string())],
                outputs_carrying_a_stored_value: vec![(0, A_LIST_OF_FLOATS.to_string())],
                ..Default::default()
            }],
            read_the_newest_message_at,
            values_last_read_off_links_from_outside_islands: Arc::default(),
            claim_the_pooled_gpu_frame_a_message_names: Box::new(|_| Ok(None)),
        },
        raises_noted_in_a_record_of_its_own(),
    )
    .unwrap()
}

fn a_record_of_levels(levels: &[f64]) -> PinValueTheBootstrapHandsOver {
    Python::initialize();
    Python::attach(|python| {
        let record = PyDict::new(python);
        record.set_item("value", levels.to_vec()).unwrap();
        PinValueTheBootstrapHandsOver::Record(record.unbind())
    })
}

#[test]
fn a_list_of_floats_set_as_a_pin_reaches_the_body_and_leaves_the_island_as_an_array_frame() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let (_queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
    let island = an_island_doubling_each_level(
        a_record_of_levels(&[0.25, 1.5]),
        Vec::new(),
        read_the_newest_message_at,
        stream_clock_now_ns,
    );
    stream_time_ns.store(1, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Packed(
            pack_pin_value(&a_list_of_floats(), &serde_json::json!([0.5, 3.0]), 0).unwrap()
        )
    );
}

#[test]
fn a_list_of_floats_read_off_a_link_from_outside_the_island_travels_unchanged() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let (queued, read_the_newest_message_at) = messages_queued_at_the_reader_input_port();
    let island = an_island_doubling_each_level(
        a_record_of_levels(&[]),
        vec![(
            0,
            READER_INPUT_PORT_OF_THE_LINK_FROM_OUTSIDE_THE_ISLAND.to_string(),
            "list_of".to_string(),
        )],
        read_the_newest_message_at,
        stream_clock_now_ns,
    );
    let PackedPinValue {
        payload_descriptor,
        payload,
    } = pack_pin_value(&a_list_of_floats(), &serde_json::json!([1.0, 2.0, 4.0]), 0).unwrap();
    queued
        .lock()
        .push(a_message_of(payload, payload_descriptor, 10));
    stream_time_ns.store(1, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Packed(
            pack_pin_value(&a_list_of_floats(), &serde_json::json!([2.0, 4.0, 8.0]), 0).unwrap()
        )
    );
}

#[test]
fn a_list_of_strings_read_where_a_list_of_floats_is_taken_is_refused_naming_both() {
    let PackedPinValue {
        payload_descriptor,
        payload,
    } = pack_pin_value(
        &serde_json::from_str(r#"{"type":"list_of","element_type":{"type":"string"}}"#).unwrap(),
        &serde_json::json!(["loud"]),
        0,
    )
    .unwrap();
    let refusal = the_record_of_a_stored_value_a_message_carries(
        &a_message_of(payload, payload_descriptor, 10),
        &a_list_of_floats(),
        "double-1",
        "levels",
    )
    .expect_err("a List of floats travels as an Array frame");
    assert!(
        refusal.to_string().contains("List of Float")
            && refusal.to_string().contains("Array frame"),
        "{refusal}"
    );
}
