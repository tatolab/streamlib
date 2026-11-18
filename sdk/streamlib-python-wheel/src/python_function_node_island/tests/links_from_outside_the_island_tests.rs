// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

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
            vec![(None, 0.0), (None, if is_open { 1.0 } else { 0.0 })],
            vec![],
            vec![],
            1,
            vec![],
        )],
        (0, 0),
        stream_clock_now_ns,
        None,
        &|_| None,
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
            Ok(Some(Box::new(claim) as PooledGpuFrameClaimAnIslandHolds))
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
                vec![(None, 0.0)],
                vec![],
                vec![0.0],
                1,
                vec![],
            )],
            (0, 0),
            stream_clock_now_ns,
            None,
            &|_| None,
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
        FunctionNodeIslandOutputValue::ControlValue(6.0)
    );
    stream_time_ns.store(300, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::ControlValue(6.0),
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
            vec![(None, 0.0)],
            vec![],
            vec![0.0],
            1,
            vec![],
        )],
        (0, 0),
        stream_clock_now_ns,
        None,
        &|_| None,
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
        FunctionNodeIslandOutputValue::ControlValue(6.0)
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
        FunctionNodeIslandOutputValue::ControlValue(6.0)
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
                vec![(None, 0.0)],
                vec![],
                vec![0.0],
                1,
                vec![],
            )],
            (0, 0),
            stream_clock_now_ns,
            None,
            &|_| None,
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

    assert_eq!(
        island.the_state_each_stateful_function_node_last_returned(),
        vec![(0, vec![0.0])]
    );
}
