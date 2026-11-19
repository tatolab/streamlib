// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

unsafe extern "C" fn scale_by_config_plus_seconds(
    input_values: *const f64,
    config_values: *const f64,
    _state_values: *mut f64,
    output_values: *mut f64,
    now_ns: i64,
) -> i32 {
    unsafe {
        *output_values = *input_values * *config_values + now_ns as f64 * 1e-9;
    }
    MODULE_HOST_TABLE_STATUS_OK
}

unsafe extern "C" fn refuse_after_writing_state(
    _input_values: *const f64,
    _config_values: *const f64,
    state_values: *mut f64,
    _output_values: *mut f64,
    _now_ns: i64,
) -> i32 {
    unsafe {
        *state_values = 99.0;
    }
    tatolab_module_sdk::module_host_table_abi::MODULE_HOST_TABLE_STATUS_REFUSED
}

fn an_island_of_add_one_then_scale(
    stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
) -> FunctionNodeIslandEvaluatedInThisHelper {
    FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
        vec![
            (
                "add_one".to_string(),
                0,
                compiled_to(add_one_and_count_calls),
                vec![(None, PinValueTheBootstrapHandsOver::Number(2.0))],
                vec![],
                vec![0.0],
                1,
                vec![],
            ),
            (
                "scale".to_string(),
                1,
                compiled_to(scale_by_config_plus_seconds),
                vec![(Some((0, 0)), PinValueTheBootstrapHandsOver::Number(0.0))],
                vec![10.0],
                vec![],
                1,
                vec![],
            ),
        ],
        (1, 0),
        stream_clock_now_ns,
        None,
        &|_| None,
        HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
        raises_noted_in_a_record_of_its_own(),
    )
    .unwrap()
}

#[test]
fn each_node_reads_the_one_before_it_and_its_pin_and_config() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    stream_time_ns.store(1_000_000_000, Ordering::SeqCst);
    let island = an_island_of_add_one_then_scale(stream_clock_now_ns);
    let output = island.evaluate_the_island_output_for_this_read().unwrap();
    assert_eq!(
        output.value,
        FunctionNodeIslandOutputValue::ControlValue(vec![(2.0 + 1.0) * 10.0 + 1.0])
    );
    assert_eq!(output.evaluated_at_ns, 1_000_000_000);
}

#[test]
fn an_input_past_a_nodes_own_sets_the_config_value_it_names_at_each_evaluation() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    stream_time_ns.store(0, Ordering::SeqCst);
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![
                (
                    "add_one".to_string(),
                    0,
                    compiled_to(add_one_and_count_calls),
                    vec![(None, PinValueTheBootstrapHandsOver::Number(2.0))],
                    vec![],
                    vec![0.0],
                    1,
                    vec![],
                ),
                (
                    "scale".to_string(),
                    1,
                    compiled_to(scale_by_config_plus_seconds),
                    vec![
                        (None, PinValueTheBootstrapHandsOver::Number(4.0)),
                        (Some((0, 0)), PinValueTheBootstrapHandsOver::Number(0.0)),
                    ],
                    vec![10.0],
                    vec![],
                    1,
                    vec![0],
                ),
            ],
            (1, 0),
            stream_clock_now_ns,
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            raises_noted_in_a_record_of_its_own(),
        )
        .unwrap();
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::ControlValue(vec![4.0 * (2.0 + 1.0)]),
        "the linked config value, not the one the node was handed, scales"
    );
}

#[test]
fn a_config_value_an_input_sets_that_the_node_does_not_take_is_refused_naming_it() {
    let refusal =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "scale".to_string(),
                0,
                compiled_to(scale_by_config_plus_seconds),
                vec![
                    (None, PinValueTheBootstrapHandsOver::Number(4.0)),
                    (None, PinValueTheBootstrapHandsOver::Number(3.0)),
                ],
                vec![10.0],
                vec![],
                1,
                vec![1],
            )],
            (0, 0),
            a_stream_clock_the_test_sets().1,
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            raises_noted_in_a_record_of_its_own(),
        )
        .err()
        .unwrap();
    assert!(
        refusal.contains("`scale`") && refusal.contains("config value 1"),
        "got {refusal}"
    );
}

#[test]
fn an_island_is_evaluated_once_per_stream_time() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    stream_time_ns.store(5, Ordering::SeqCst);
    let island = an_island_of_add_one_then_scale(stream_clock_now_ns);
    island.evaluate_the_island_output_for_this_read().unwrap();
    island.evaluate_the_island_output_for_this_read().unwrap();
    let calls_at_one_time = island
        .evaluation
        .lock()
        .function_nodes_each_after_what_it_reads[0]
        .how_it_is_called
        .state_values()[0];
    assert_eq!(calls_at_one_time, 1.0);
    stream_time_ns.store(6, Ordering::SeqCst);
    island.evaluate_the_island_output_for_this_read().unwrap();
    assert_eq!(
        island
            .evaluation
            .lock()
            .function_nodes_each_after_what_it_reads[0]
            .how_it_is_called
            .state_values()[0],
        2.0
    );
}

#[test]
fn a_refused_evaluation_hands_out_nothing_keeps_its_state_and_is_noted_as_the_nodes_raise() {
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "refuses".to_string(),
                2,
                compiled_to(refuse_after_writing_state),
                vec![],
                vec![],
                vec![1.0],
                1,
                vec![],
            )],
            (0, 0),
            a_stream_clock_the_test_sets().1,
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            raises_noted_in_a_record_of_its_own(),
        )
        .unwrap();
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Nothing
    );
    let raises = the_raises_noted_by(&island);
    assert!(
        raises["refuses"].contains("refused its evaluation with status"),
        "{raises:?}"
    );
    assert_eq!(
        island
            .evaluation
            .lock()
            .function_nodes_each_after_what_it_reads[0]
            .how_it_is_called
            .state_values(),
        [1.0]
    );
}

/// A board as a parent creates it for a helper calling the stateful node
/// numbered `breadcrumb_number`, and the writer that helper opens on it.
fn a_board_carrying_the_state_of_node(
    breadcrumb_number: u32,
) -> (
    streamlib::sdk::iceoryx2::HelperProcessLossCountBoard,
    Arc<HelperProcessLossCountBoardWriter>,
) {
    let board_name = format!(
        "island{}_state{breadcrumb_number}/loss-counts",
        std::process::id()
    );
    let parent_board = streamlib::sdk::iceoryx2::Iceoryx2Node::for_this_test_process()
        .create_helper_process_loss_count_board(&board_name, Vec::new(), vec![breadcrumb_number])
        .unwrap();
    let board_writer = streamlib::sdk::iceoryx2::Iceoryx2Node::for_this_test_process()
        .open_helper_process_loss_count_board_writer(&board_name, &[], &[breadcrumb_number])
        .unwrap();
    (parent_board, Arc::new(board_writer))
}

#[test]
fn a_stateful_node_resumes_from_the_state_its_stream_carries_and_writes_each_new_one() {
    let (parent_board, board_writer) = a_board_carrying_the_state_of_node(4);
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "counts_calls".to_string(),
                4,
                compiled_to(add_one_and_count_calls),
                vec![(None, PinValueTheBootstrapHandsOver::Number(0.0))],
                vec![],
                vec![0.0],
                1,
                vec![],
            )],
            (0, 0),
            stream_clock_now_ns,
            Some(board_writer),
            &|breadcrumb_number| (breadcrumb_number == 4).then(|| vec![41.0]),
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            raises_noted_in_a_record_of_its_own(),
        )
        .unwrap();
    assert_eq!(
        parent_board
            .function_node_state_entry(4)
            .unwrap()
            .state_values_written(),
        None,
        "nothing is written before the first call"
    );
    stream_time_ns.store(1, Ordering::SeqCst);
    island.evaluate_the_island_output_for_this_read().unwrap();
    assert_eq!(
        parent_board
            .function_node_state_entry(4)
            .unwrap()
            .state_values_written(),
        Some(&[42.0][..]),
        "the first call carries on from the state it resumed from"
    );
}

#[test]
fn a_stored_state_of_another_length_is_not_resumed_from() {
    assert_eq!(
        the_state_a_function_node_starts_from(vec![0.0, 0.0], Some(vec![5.0])),
        vec![0.0, 0.0]
    );
    assert_eq!(
        the_state_a_function_node_starts_from(vec![0.0], Some(vec![5.0])),
        vec![5.0]
    );
    assert_eq!(
        the_state_a_function_node_starts_from(vec![1.0], None),
        vec![1.0]
    );
}

#[test]
fn a_node_reading_one_not_evaluated_before_it_is_refused() {
    let refusal =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "reads_ahead".to_string(),
                3,
                compiled_to(add_one_and_count_calls),
                vec![(Some((0, 0)), PinValueTheBootstrapHandsOver::Number(0.0))],
                vec![],
                vec![0.0],
                1,
                vec![],
            )],
            (0, 0),
            a_stream_clock_the_test_sets().1,
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            raises_noted_in_a_record_of_its_own(),
        )
        .err()
        .unwrap();
    assert!(
        refusal.contains("not an output of a node evaluated before it"),
        "{refusal}"
    );
}

/// An object as the bootstrap builds one to run a body as Python: this one
/// counts its calls in its state and answers a dict naming its input and
/// the count, raising on a negative input.
fn run_as_python_counting_its_calls() -> Py<PyAny> {
    Python::initialize();
    Python::attach(|python| {
        let namespace = PyDict::new(python);
        crate::python_class_from_source_for_tests::class_from_source_in_namespace(
            python,
            "class CountsItsCallsAsPython:\n\
             \x20   def __init__(self):\n\
             \x20       self.calls = 0.0\n\
             \x20   def evaluate(self, input_values, now_ns):\n\
             \x20       (x,) = input_values\n\
             \x20       if x < 0:\n\
             \x20           raise ValueError('a negative input')\n\
             \x20       self.calls += 1.0\n\
             \x20       return [{'x': x, 'calls': self.calls}, self.calls]\n\
             \x20   def state_values(self):\n\
             \x20       return [self.calls]\n\
             \x20   def resume_from_state_values(self, state_values):\n\
             \x20       (self.calls,) = state_values\n",
            "CountsItsCallsAsPython",
            &namespace,
        )
        .call0()
        .unwrap()
        .unbind()
    })
}

fn an_island_of_add_one_then_a_node_run_as_python(
    island_output: (usize, usize),
    stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
    state_resumed_from: &dyn Fn(u32) -> Option<Vec<f64>>,
) -> FunctionNodeIslandEvaluatedInThisHelper {
    FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
        vec![
            (
                "add_one".to_string(),
                0,
                compiled_to(add_one_and_count_calls),
                vec![(None, PinValueTheBootstrapHandsOver::Number(2.0))],
                vec![],
                vec![0.0],
                1,
                vec![],
            ),
            (
                "tally".to_string(),
                1,
                run_as_python_counting_its_calls(),
                vec![(Some((0, 0)), PinValueTheBootstrapHandsOver::Number(0.0))],
                vec![],
                vec![0.0],
                2,
                vec![],
            ),
        ],
        island_output,
        stream_clock_now_ns,
        None,
        state_resumed_from,
        HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
        raises_noted_in_a_record_of_its_own(),
    )
    .unwrap()
}

#[test]
fn a_node_run_as_python_reads_a_compiled_nodes_output_and_gives_a_records_message() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    stream_time_ns.store(7, Ordering::SeqCst);
    let island =
        an_island_of_add_one_then_a_node_run_as_python((1, 0), stream_clock_now_ns, &|_| None);
    assert_eq!(
        island.the_first_function_node_it_runs_as_python(),
        Some("tally")
    );
    let output = island.evaluate_the_island_output_for_this_read().unwrap();
    let FunctionNodeIslandOutputValue::RecordsMessage(records_message_payload) = output.value
    else {
        panic!("a dict output is a Records message: {:?}", output.value);
    };
    let record = rmpv::decode::read_value(&mut records_message_payload.as_slice()).unwrap();
    assert_eq!(
        record,
        rmpv::Value::Map(vec![
            (rmpv::Value::from("x"), rmpv::Value::from(3.0)),
            (rmpv::Value::from("calls"), rmpv::Value::from(1.0)),
        ])
    );
    assert_eq!(output.evaluated_at_ns, 7);
}

#[test]
fn a_number_a_node_run_as_python_gives_is_a_control_value() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let island = an_island_of_add_one_then_a_node_run_as_python(
        (1, 1),
        stream_clock_now_ns,
        &|breadcrumb_number| (breadcrumb_number == 1).then(|| vec![41.0]),
    );
    stream_time_ns.store(1, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::ControlValue(vec![42.0]),
        "the node resumed from the state its stream carries"
    );
    assert_eq!(
        island
            .evaluation
            .lock()
            .function_nodes_each_after_what_it_reads[1]
            .how_it_is_called
            .state_values(),
        [42.0],
        "the state a node run as Python answers is what the board is written"
    );
}

#[test]
fn an_island_of_compiled_nodes_runs_no_node_as_python() {
    let island = an_island_of_add_one_then_scale(a_stream_clock_the_test_sets().1);
    assert_eq!(island.the_first_function_node_it_runs_as_python(), None);
}

#[test]
fn a_node_run_as_python_that_raises_hands_out_nothing_and_is_noted_with_what_it_raised() {
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "tally".to_string(),
                5,
                run_as_python_counting_its_calls(),
                vec![(None, PinValueTheBootstrapHandsOver::Number(-1.0))],
                vec![],
                vec![0.0],
                2,
                vec![],
            )],
            (0, 1),
            a_stream_clock_the_test_sets().1,
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            raises_noted_in_a_record_of_its_own(),
        )
        .unwrap();
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Nothing
    );
    let raises = the_raises_noted_by(&island);
    assert!(
        raises["tally"].starts_with("raised ValueError: a negative input"),
        "{raises:?}"
    );
}

/// A node run as Python that raises on its first call and answers its call
/// count after.
fn run_as_python_raising_on_its_first_call() -> Py<PyAny> {
    Python::initialize();
    Python::attach(|python| {
        let namespace = PyDict::new(python);
        crate::python_class_from_source_for_tests::class_from_source_in_namespace(
            python,
            "class RaisesOnItsFirstCall:\n\
             \x20   def __init__(self):\n\
             \x20       self.calls = 0\n\
             \x20   def evaluate(self, input_values, now_ns):\n\
             \x20       self.calls += 1\n\
             \x20       if self.calls == 1:\n\
             \x20           raise RuntimeError('broke on its first call')\n\
             \x20       return [float(self.calls)]\n",
            "RaisesOnItsFirstCall",
            &namespace,
        )
        .call0()
        .unwrap()
        .unbind()
    })
}

#[test]
fn a_raise_is_noted_until_the_node_runs_cleanly_and_the_island_answers_again() {
    let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![(
                "beat".to_string(),
                0,
                run_as_python_raising_on_its_first_call(),
                vec![],
                vec![],
                vec![],
                1,
                vec![],
            )],
            (0, 0),
            stream_clock_now_ns,
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            raises_noted_in_a_record_of_its_own(),
        )
        .unwrap();
    stream_time_ns.store(1, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Nothing
    );
    assert!(
        the_raises_noted_by(&island)["beat"]
            .starts_with("raised RuntimeError: broke on its first call"),
        "{:?}",
        the_raises_noted_by(&island)
    );
    stream_time_ns.store(2, Ordering::SeqCst);
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::ControlValue(vec![2.0])
    );
    assert!(the_raises_noted_by(&island).is_empty());
}

#[test]
fn a_compiled_node_handed_a_dict_hands_out_nothing_and_is_noted_with_why() {
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            vec![
                (
                    "tally".to_string(),
                    0,
                    run_as_python_counting_its_calls(),
                    vec![(None, PinValueTheBootstrapHandsOver::Number(1.0))],
                    vec![],
                    vec![],
                    2,
                    vec![],
                ),
                (
                    "add_one".to_string(),
                    1,
                    compiled_to(add_one_and_count_calls),
                    vec![(Some((0, 0)), PinValueTheBootstrapHandsOver::Number(0.0))],
                    vec![],
                    vec![0.0],
                    1,
                    vec![],
                ),
            ],
            (1, 0),
            a_stream_clock_the_test_sets().1,
            None,
            &|_| None,
            HowAFunctionNodeIslandCarriesValuesBeyondNumbers::none(),
            raises_noted_in_a_record_of_its_own(),
        )
        .unwrap();
    assert_eq!(
        island
            .evaluate_the_island_output_for_this_read()
            .unwrap()
            .value,
        FunctionNodeIslandOutputValue::Nothing
    );
    let raises = the_raises_noted_by(&island);
    assert_eq!(raises.keys().collect::<Vec<_>>(), ["add_one"], "{raises:?}");
    assert!(
        raises["add_one"].contains("`add_one` runs compiled and takes a number on input 0"),
        "{raises:?}"
    );
}
