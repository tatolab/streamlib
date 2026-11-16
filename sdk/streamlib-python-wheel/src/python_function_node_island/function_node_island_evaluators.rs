// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::PyValueError;
use streamlib::sdk::error::Result;
use streamlib::sdk::iceoryx2::{
    BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_ITS_PROCESSOR,
    FunctionNodeIslandEvaluatedAtEachRead, FunctionNodeIslandOutputForOneRead,
    FunctionNodeIslandOutputValue, HelperProcessLossCountBoardWriter,
};

use crate::python_helper_stream_clock::helper_stream_clock_now_ns;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

use super::*;

/// A function node island this helper evaluates at each read of the port it
/// feeds.
pub(crate) struct FunctionNodeIslandEvaluatedInThisHelper {
    island_output_node_index: usize,
    island_output_index: usize,
    first_function_node_it_runs_as_python: Option<String>,
    reader_input_ports_its_links_from_outside_it_arrive_at: Vec<String>,
    pub(super) evaluation: Mutex<FunctionNodeIslandEvaluation>,
    stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl FunctionNodeIslandEvaluatedAtEachRead for FunctionNodeIslandEvaluatedInThisHelper {
    fn evaluate_the_island_output_for_this_read(
        &self,
    ) -> Result<FunctionNodeIslandOutputForOneRead> {
        let mut evaluation = self.evaluation.lock();
        let now_ns = (self.stream_clock_now_ns)();
        if evaluation.last_evaluated_at_ns != Some(now_ns) {
            evaluation.evaluate_every_node_at(now_ns)?;
        }
        let island_output_node =
            &evaluation.function_nodes_each_after_what_it_reads[self.island_output_node_index];
        let value = island_output_node.output_values[self.island_output_index]
            .as_the_island_output_of(&island_output_node.node_id, self.island_output_index)?;
        let value = match value {
            FunctionNodeIslandOutputValue::MessageHandedOnUnread(_)
                if evaluation.message_handed_on_since_the_last_evaluation =>
            {
                FunctionNodeIslandOutputValue::Nothing
            }
            FunctionNodeIslandOutputValue::MessageHandedOnUnread(message) => {
                evaluation.message_handed_on_since_the_last_evaluation = true;
                FunctionNodeIslandOutputValue::MessageHandedOnUnread(message)
            }
            value => value,
        };
        Ok(FunctionNodeIslandOutputForOneRead {
            value,
            evaluated_at_ns: now_ns,
        })
    }

    fn the_first_function_node_it_runs_as_python(&self) -> Option<&str> {
        self.first_function_node_it_runs_as_python.as_deref()
    }

    fn the_state_each_stateful_function_node_last_returned(&self) -> Vec<(u32, Vec<f64>)> {
        self.evaluation
            .lock()
            .function_nodes_each_after_what_it_reads
            .iter()
            .filter(|function_node| !function_node.how_it_is_called.state_values().is_empty())
            .map(|function_node| {
                (
                    function_node.breadcrumb_number,
                    function_node.how_it_is_called.state_values().to_vec(),
                )
            })
            .collect()
    }

    fn the_reader_input_ports_its_links_from_outside_it_arrive_at(&self) -> &[String] {
        &self.reader_input_ports_its_links_from_outside_it_arrive_at
    }
}

/// One function node as the bootstrap hands it over: its id, its breadcrumb
/// number, its compiled evaluate entry's address or the object that runs it as
/// Python, where each input takes its value, its config values, its initial
/// state values, how many outputs it writes, and the index among its config
/// values each input past its own sets.
pub(super) type FunctionNodeHandedOverByTheBootstrap = (
    String,
    u32,
    Py<PyAny>,
    Vec<(Option<(usize, usize)>, f64)>,
    Vec<f64>,
    Vec<f64>,
    usize,
    Vec<usize>,
);

impl FunctionNodeIslandEvaluatedInThisHelper {
    pub(super) fn of_the_function_nodes_the_bootstrap_compiled(
        function_nodes: Vec<FunctionNodeHandedOverByTheBootstrap>,
        (island_output_node_index, island_output_index): (usize, usize),
        stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
        call_breadcrumb_board_writer: Option<Arc<HelperProcessLossCountBoardWriter>>,
        state_resumed_from: &dyn Fn(u32) -> Option<Vec<f64>>,
        how_it_carries_values_beyond_numbers: HowAFunctionNodeIslandCarriesValuesBeyondNumbers,
    ) -> std::result::Result<Self, String> {
        let HowAFunctionNodeIslandCarriesValuesBeyondNumbers {
            mut how_each_node_carries_its_values,
            read_the_newest_message_at,
            values_last_read_off_links_from_outside_islands,
            claim_the_pooled_gpu_frame_a_message_names,
        } = how_it_carries_values_beyond_numbers;
        how_each_node_carries_its_values.resize_with(function_nodes.len(), Default::default);
        let mut function_nodes_each_after_what_it_reads: Vec<FunctionNodeInAnIsland> =
            Vec::with_capacity(function_nodes.len());
        for (
            node_index,
            (
                node_id,
                breadcrumb_number,
                called_through,
                where_each_input_takes_its_value,
                config_values,
                initial_state_values,
                output_count,
                config_value_indices_set_by_the_inputs_past_its_own,
            ),
        ) in function_nodes.into_iter().enumerate()
        {
            let how_it_carries_its_values = &how_each_node_carries_its_values[node_index];
            let mut where_each_input_takes_its_value_read = Vec::new();
            for (input_index, (source, pin_value)) in
                where_each_input_takes_its_value.into_iter().enumerate()
            {
                where_each_input_takes_its_value_read.push(match source {
                    None if how_it_carries_its_values
                        .inputs_carrying_an_unread_message
                        .contains(&input_index) =>
                    {
                        WhereAFunctionNodeInputTakesItsValue::ItsPin(FunctionNodeValue::Nothing)
                    }
                    None => WhereAFunctionNodeInputTakesItsValue::ItsPin(
                        FunctionNodeValue::Number(pin_value),
                    ),
                    Some((source_node_index, output_index)) if source_node_index > node_index => {
                        WhereAFunctionNodeInputTakesItsValue::ALaterNodesOutputAtTheLastEvaluation {
                            node_index: source_node_index,
                            output_index,
                        }
                    }
                    Some((source_node_index, output_index)) => {
                        let reads_an_output_evaluated_earlier = source_node_index < node_index
                            && output_index
                                < function_nodes_each_after_what_it_reads
                                    .get(source_node_index)
                                    .map_or(0, |earlier| earlier.output_values.len());
                        if !reads_an_output_evaluated_earlier {
                            return Err(format!(
                                "function node `{node_id}` reads output {output_index} of node \
                                 {source_node_index}, which is not an output of a node \
                                 evaluated before it"
                            ));
                        }
                        WhereAFunctionNodeInputTakesItsValue::AnEarlierNodesOutput {
                            node_index: source_node_index,
                            output_index,
                        }
                    }
                });
            }
            for (input_index, reader_input_port, value_type) in
                &how_it_carries_its_values.inputs_read_from_links_from_outside_the_island
            {
                let read_as =
                    HowAnInputReadsALinkFromOutsideItsIsland::of_the_value_type(value_type)
                        .map_err(|refusal| format!("function node `{node_id}`: {refusal}"))?;
                let where_it_takes_its_value = where_each_input_takes_its_value_read
                    .get_mut(*input_index)
                    .ok_or_else(|| {
                        format!(
                            "function node `{node_id}` reads input {input_index} off a link from \
                             outside its island, and it takes fewer inputs"
                        )
                    })?;
                let value_read_last = match read_as {
                    HowAnInputReadsALinkFromOutsideItsIsland::MessageHandedOnUnread => {
                        FunctionNodeValue::Nothing
                    }
                    _ => values_last_read_off_links_from_outside_islands
                        .lock()
                        .get(reader_input_port)
                        .map_or(FunctionNodeValue::Nothing, FunctionNodeValue::duplicated),
                };
                *where_it_takes_its_value =
                    WhereAFunctionNodeInputTakesItsValue::ALinkFromOutsideTheIsland {
                        reader_input_port: reader_input_port.clone(),
                        read_as,
                        value_read_last,
                    };
            }
            let writes_its_state_on_the_board = !initial_state_values.is_empty()
                && call_breadcrumb_board_writer
                    .as_deref()
                    .is_some_and(|board_writer| {
                        board_writer.carries_a_state_entry_for(breadcrumb_number)
                    });
            let state_resumed_from = state_resumed_from(breadcrumb_number)
                .filter(|resumed| resumed.len() == initial_state_values.len());
            let resumes_from_the_state_its_stream_carries =
                state_resumed_from.is_some() && !initial_state_values.is_empty();
            let how_it_is_called = HowAFunctionNodeIsCalled::handed_over_by_the_bootstrap(
                &node_id,
                called_through,
                config_values,
                config_value_indices_set_by_the_inputs_past_its_own,
                where_each_input_takes_its_value_read.len(),
                the_state_a_function_node_starts_from(initial_state_values, state_resumed_from),
                resumes_from_the_state_its_stream_carries,
                output_count,
                how_it_carries_its_values,
            )?;
            function_nodes_each_after_what_it_reads.push(FunctionNodeInAnIsland {
                node_id,
                breadcrumb_number,
                how_it_is_called,
                where_each_input_takes_its_value: where_each_input_takes_its_value_read,
                inputs_that_take_none: how_it_carries_its_values.inputs_that_take_none.clone(),
                output_values: (0..output_count)
                    .map(|_| FunctionNodeValue::Number(0.0))
                    .collect(),
                writes_its_state_on_the_board,
            });
        }
        for function_node in &function_nodes_each_after_what_it_reads {
            for where_it_takes_its_value in &function_node.where_each_input_takes_its_value {
                if let WhereAFunctionNodeInputTakesItsValue::ALaterNodesOutputAtTheLastEvaluation {
                    node_index: later_node_index,
                    output_index,
                } = where_it_takes_its_value
                {
                    let reads_an_output_the_island_holds = function_nodes_each_after_what_it_reads
                        .get(*later_node_index)
                        .is_some_and(|later| *output_index < later.output_values.len());
                    if !reads_an_output_the_island_holds {
                        return Err(format!(
                            "function node `{}` reads output {output_index} of node \
                             {later_node_index}, which the island does not hold",
                            function_node.node_id
                        ));
                    }
                }
            }
        }
        let reads_an_output_of_the_island = function_nodes_each_after_what_it_reads
            .get(island_output_node_index)
            .is_some_and(|node| island_output_index < node.output_values.len());
        if !reads_an_output_of_the_island {
            return Err(format!(
                "the island's output names output {island_output_index} of node \
                 {island_output_node_index}, which the island does not hold"
            ));
        }
        let first_function_node_it_runs_as_python = function_nodes_each_after_what_it_reads
            .iter()
            .find(|function_node| function_node.how_it_is_called.runs_as_python())
            .map(|function_node| function_node.node_id.clone());
        let mut reader_input_ports_its_links_from_outside_it_arrive_at = Vec::new();
        for function_node in &function_nodes_each_after_what_it_reads {
            for where_it_takes_its_value in &function_node.where_each_input_takes_its_value {
                if let WhereAFunctionNodeInputTakesItsValue::ALinkFromOutsideTheIsland {
                    reader_input_port,
                    ..
                } = where_it_takes_its_value
                    && !reader_input_ports_its_links_from_outside_it_arrive_at
                        .contains(reader_input_port)
                {
                    reader_input_ports_its_links_from_outside_it_arrive_at
                        .push(reader_input_port.clone());
                }
            }
        }
        Ok(Self {
            island_output_node_index,
            island_output_index,
            first_function_node_it_runs_as_python,
            reader_input_ports_its_links_from_outside_it_arrive_at,
            evaluation: Mutex::new(FunctionNodeIslandEvaluation {
                function_nodes_each_after_what_it_reads,
                last_evaluated_at_ns: None,
                message_handed_on_since_the_last_evaluation: false,
                read_the_newest_message_at,
                values_last_read_off_links_from_outside_islands,
                claim_the_pooled_gpu_frame_a_message_names,
                call_breadcrumb_board_writer,
            }),
            stream_clock_now_ns,
        })
    }
}

#[pyclass(name = "FunctionNodeEvaluatedAsItsHelpersProcessor", frozen)]
pub(crate) struct FunctionNodeEvaluatedAsItsHelpersProcessor {
    function_node: Mutex<FunctionNodeInAnIsland>,
    call_breadcrumb_board_writer: Option<Arc<HelperProcessLossCountBoardWriter>>,
}

#[pymethods]
impl FunctionNodeEvaluatedAsItsHelpersProcessor {
    #[new]
    #[pyo3(signature = (
        *,
        link_data_access,
        node_id,
        called_through,
        input_count,
        config_values,
        config_value_indices_set_by_the_inputs_past_its_own,
        initial_state_values,
        output_count,
        outputs_that_may_hand_out_nothing = Vec::new(),
        inputs_carrying_an_unread_message = Vec::new(),
        outputs_carrying_an_unread_message = Vec::new(),
    ))]
    #[allow(
        clippy::too_many_arguments,
        reason = "pyo3 maps each Python keyword argument to its own parameter"
    )]
    fn new(
        link_data_access: &PythonProcessorLinkDataAccess,
        node_id: String,
        called_through: Py<PyAny>,
        input_count: usize,
        config_values: Vec<f64>,
        config_value_indices_set_by_the_inputs_past_its_own: Vec<usize>,
        initial_state_values: Vec<f64>,
        output_count: usize,
        outputs_that_may_hand_out_nothing: Vec<usize>,
        inputs_carrying_an_unread_message: Vec<usize>,
        outputs_carrying_an_unread_message: Vec<usize>,
    ) -> PyResult<Self> {
        let breadcrumb_number =
            BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_ITS_PROCESSOR;
        let call_breadcrumb_board_writer =
            link_data_access.shared_loss_count_board_writer_of_this_helper();
        let writes_its_state_on_the_board = !initial_state_values.is_empty()
            && call_breadcrumb_board_writer
                .as_deref()
                .is_some_and(|board_writer| {
                    board_writer.carries_a_state_entry_for(breadcrumb_number)
                });
        let state_resumed_from = link_data_access
            .the_state_a_function_node_resumes_from(breadcrumb_number)
            .filter(|resumed| resumed.len() == initial_state_values.len());
        let resumes_from_the_state_its_stream_carries =
            state_resumed_from.is_some() && !initial_state_values.is_empty();
        let how_it_is_called = HowAFunctionNodeIsCalled::handed_over_by_the_bootstrap(
            &node_id,
            called_through,
            config_values,
            config_value_indices_set_by_the_inputs_past_its_own,
            input_count,
            the_state_a_function_node_starts_from(initial_state_values, state_resumed_from),
            resumes_from_the_state_its_stream_carries,
            output_count,
            &HowAFunctionNodeCarriesValuesBeyondNumbers {
                outputs_that_may_hand_out_nothing,
                inputs_carrying_an_unread_message,
                outputs_carrying_an_unread_message,
                ..HowAFunctionNodeCarriesValuesBeyondNumbers::default()
            },
        )
        .map_err(PyValueError::new_err)?;
        Ok(Self {
            function_node: Mutex::new(FunctionNodeInAnIsland {
                node_id,
                breadcrumb_number,
                how_it_is_called,
                where_each_input_takes_its_value: Vec::new(),
                inputs_that_take_none: Vec::new(),
                output_values: (0..output_count)
                    .map(|_| FunctionNodeValue::Number(0.0))
                    .collect(),
                writes_its_state_on_the_board,
            }),
            call_breadcrumb_board_writer,
        })
    }

    /// Call the node with `input_values` at the stream's time now, answering
    /// its outputs, a `float` for a number or bool, a `dict` as given, the
    /// `UnreadMessage` it hands on, and `None` for one that hands out nothing;
    /// refused when it refuses, keeping the state it had.
    fn evaluate<'py>(
        &self,
        python: Python<'py>,
        input_values: Vec<Bound<'py, PyAny>>,
    ) -> PyResult<Vec<Bound<'py, PyAny>>> {
        let now_ns = helper_stream_clock_now_ns();
        let input_values = input_values
            .iter()
            .map(FunctionNodeValue::from_python)
            .collect::<PyResult<Vec<_>>>()?;
        let function_node = &self.function_node;
        let call_breadcrumb_board_writer = self.call_breadcrumb_board_writer.as_deref();
        python.detach(|| {
            let mut function_node = function_node.lock();
            let function_node = &mut *function_node;
            if input_values.len() != function_node.how_it_is_called.input_count() {
                return Err(PyValueError::new_err(format!(
                    "function node `{}` takes {} inputs, and was called with {}",
                    function_node.node_id,
                    function_node.how_it_is_called.input_count(),
                    input_values.len()
                )));
            }
            let input_value_references: Vec<&FunctionNodeValue> = input_values.iter().collect();
            call_noting_its_breadcrumb_on(
                call_breadcrumb_board_writer,
                &function_node.node_id,
                function_node.breadcrumb_number,
                &mut function_node.how_it_is_called,
                &mut function_node.output_values,
                &input_value_references,
                now_ns,
            )
            .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
            if let Some(board_writer) = call_breadcrumb_board_writer
                && function_node.writes_its_state_on_the_board
            {
                board_writer
                    .write_the_state_of_a_function_node(
                        function_node.breadcrumb_number,
                        function_node.how_it_is_called.state_values(),
                    )
                    .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
            }
            Ok(())
        })?;
        let function_node = self.function_node.lock();
        function_node
            .output_values
            .iter()
            .map(|output_value| output_value.to_python(python))
            .collect()
    }
}
