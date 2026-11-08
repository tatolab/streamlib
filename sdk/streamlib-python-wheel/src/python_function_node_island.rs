// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A function node island evaluated inside this helper's reads: each node
//! called in turn, once per stream time, through its compiled evaluate entry
//! or as Python in this helper's interpreter.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyInt, PyList};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_ITS_PROCESSOR,
    FunctionNodeIslandEvaluatedAtEachRead, FunctionNodeIslandOutputForOneRead,
    FunctionNodeIslandOutputValue, HelperProcessLossCountBoardWriter,
};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_OK, ModuleHostTableFunctionNodeEvaluateEntry,
};

use crate::python_bag_conversion::records_message_payload_of;
use crate::python_helper_stream_clock::helper_stream_clock_now_ns;
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

/// One value a function node takes or gives.
#[derive(Debug)]
enum FunctionNodeValue {
    /// A number or a bool, as the f64 a compiled entry carries.
    Number(f64),
    /// A `dict`, which only a node run as Python takes or gives.
    Record(Py<PyAny>),
}

impl FunctionNodeValue {
    fn as_a_number_for_an_input_of(&self, node_id: &str, input_index: usize) -> Result<f64> {
        match self {
            Self::Number(number) => Ok(*number),
            Self::Record(_) => Err(Error::Runtime(format!(
                "function node `{node_id}` runs compiled and takes a number on input \
                 {input_index}, and was handed a dict"
            ))),
        }
    }

    fn to_python<'py>(&self, python: Python<'py>) -> Bound<'py, PyAny> {
        match self {
            Self::Number(number) => number.into_pyobject(python).unwrap().into_any(),
            Self::Record(record) => record.bind(python).clone(),
        }
    }

    /// `value` as a function node gives it: a `dict` as a record, anything
    /// else as the number it converts to.
    fn from_python(value: &Bound<'_, PyAny>) -> PyResult<Self> {
        if value.is_instance_of::<PyDict>() {
            return Ok(Self::Record(value.clone().unbind()));
        }
        Ok(Self::Number(value.extract::<f64>()?))
    }
}

/// Where one input of a function node in an island takes its value from.
#[derive(Debug)]
enum WhereAFunctionNodeInputTakesItsValue {
    /// An output of a node evaluated earlier in the island.
    FromAnEarlierNodesOutput {
        node_index: usize,
        output_index: usize,
    },
    /// The pin's value, as nothing is linked to it.
    FromItsPin(FunctionNodeValue),
}

/// A function node's evaluate entry, compiled for its signature, and the f64
/// buffers it is called with.
struct FunctionNodeCalledThroughItsEvaluateEntry {
    evaluate_entry: ModuleHostTableFunctionNodeEvaluateEntry,
    config_values: Vec<f64>,
    input_values: Vec<f64>,
    state_values: Vec<f64>,
    state_values_of_the_call_under_way: Vec<f64>,
    output_values: Vec<f64>,
}

/// A function node whose body runs as Python in this helper's interpreter,
/// through the object the bootstrap built for it.
struct FunctionNodeCalledAsPython {
    input_count: usize,
    /// Answers `evaluate(input_values, now_ns)` with its outputs in port order,
    /// keeping its state, and `state_values()` with that state as f64s when
    /// its state record is numbers and bools.
    run_as_python: Py<PyAny>,
    /// The state it last answered, as written on the board.
    state_values: Vec<f64>,
}

/// How one function node is called.
enum HowAFunctionNodeIsCalled {
    ThroughItsEvaluateEntry(FunctionNodeCalledThroughItsEvaluateEntry),
    AsPython(FunctionNodeCalledAsPython),
}

/// One function node of an island, and the values it is called with.
struct FunctionNodeInAnIsland {
    node_id: String,
    breadcrumb_number: u32,
    how_it_is_called: HowAFunctionNodeIsCalled,
    where_each_input_takes_its_value: Vec<WhereAFunctionNodeInputTakesItsValue>,
    output_values: Vec<FunctionNodeValue>,
    /// Whether each call's new state is written on the helper's board, which
    /// carries an entry for the node exactly when its state is carried.
    writes_its_state_on_the_board: bool,
}

/// The values every node of an island last took, and the stream time they were
/// evaluated at.
struct FunctionNodeIslandEvaluation {
    function_nodes_each_after_what_it_reads: Vec<FunctionNodeInAnIsland>,
    last_evaluated_at_ns: Option<i64>,
    /// The board each call's breadcrumb is noted on, so the parent can name
    /// the node a call died or stalled in.
    call_breadcrumb_board_writer: Option<Arc<HelperProcessLossCountBoardWriter>>,
}

/// A function node island this helper evaluates at each read of the port it
/// feeds.
pub(crate) struct FunctionNodeIslandEvaluatedInThisHelper {
    island_output_node_index: usize,
    island_output_index: usize,
    first_function_node_it_runs_as_python: Option<String>,
    evaluation: Mutex<FunctionNodeIslandEvaluation>,
    stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
}

impl FunctionNodeIslandEvaluation {
    fn evaluate_every_node_at(&mut self, now_ns: i64) -> Result<()> {
        for node_index in 0..self.function_nodes_each_after_what_it_reads.len() {
            let (evaluated_earlier, from_this_node_on) = self
                .function_nodes_each_after_what_it_reads
                .split_at_mut(node_index);
            let function_node = &mut from_this_node_on[0];
            let input_values: Vec<&FunctionNodeValue> = function_node
                .where_each_input_takes_its_value
                .iter()
                .map(|where_it_takes_its_value| match where_it_takes_its_value {
                    WhereAFunctionNodeInputTakesItsValue::FromAnEarlierNodesOutput {
                        node_index,
                        output_index,
                    } => &evaluated_earlier[*node_index].output_values[*output_index],
                    WhereAFunctionNodeInputTakesItsValue::FromItsPin(pin_value) => pin_value,
                })
                .collect();
            call_noting_its_breadcrumb_on(
                self.call_breadcrumb_board_writer.as_deref(),
                &function_node.node_id,
                function_node.breadcrumb_number,
                &mut function_node.how_it_is_called,
                &mut function_node.output_values,
                &input_values,
                now_ns,
            )?;
            if let Some(board_writer) = self.call_breadcrumb_board_writer.as_deref()
                && function_node.writes_its_state_on_the_board
            {
                board_writer.write_the_state_of_a_function_node(
                    function_node.breadcrumb_number,
                    function_node.how_it_is_called.state_values(),
                )?;
            }
        }
        self.last_evaluated_at_ns = Some(now_ns);
        Ok(())
    }
}

/// Call a function node at `now_ns` with `input_values`, its breadcrumb noted
/// on `call_breadcrumb_board_writer` for the length of the call.
fn call_noting_its_breadcrumb_on(
    call_breadcrumb_board_writer: Option<&HelperProcessLossCountBoardWriter>,
    node_id: &str,
    breadcrumb_number: u32,
    how_it_is_called: &mut HowAFunctionNodeIsCalled,
    output_values: &mut [FunctionNodeValue],
    input_values: &[&FunctionNodeValue],
    now_ns: i64,
) -> Result<()> {
    let call_under_way = call_breadcrumb_board_writer.map(|board_writer| {
        board_writer.note_a_function_node_call_started(breadcrumb_number, monotonic_clock_now_ns())
    });
    let called = how_it_is_called.call_at(node_id, input_values, output_values, now_ns);
    if let (Some(board_writer), Some(call_under_way)) =
        (call_breadcrumb_board_writer, call_under_way)
    {
        board_writer.note_the_function_node_call_returned(call_under_way);
    }
    called
}

/// The state a function node starts its first call from: the state its stream
/// carries for it when that is one of the same length, its state record's
/// defaults otherwise.
fn the_state_a_function_node_starts_from(
    initial_state_values: Vec<f64>,
    state_resumed_from: Option<Vec<f64>>,
) -> Vec<f64> {
    match state_resumed_from {
        Some(state_resumed_from) if state_resumed_from.len() == initial_state_values.len() => {
            state_resumed_from
        }
        _ => initial_state_values,
    }
}

impl HowAFunctionNodeIsCalled {
    /// How the bootstrap hands a node over: its compiled entry's address, or
    /// the object that runs its body as Python. A node run as Python is told
    /// the state its stream carries for it, when it carries one.
    fn handed_over_by_the_bootstrap(
        node_id: &str,
        called_through: Py<PyAny>,
        config_values: Vec<f64>,
        input_count: usize,
        state_values: Vec<f64>,
        resumes_from_the_state_its_stream_carries: bool,
        output_count: usize,
    ) -> std::result::Result<Self, String> {
        Python::attach(|python| {
            let called_through = called_through.bind(python);
            if called_through.is_instance_of::<PyInt>() {
                let evaluate_entry_address: usize = called_through
                    .extract()
                    .map_err(|refusal| format!("function node `{node_id}`: {refusal}"))?;
                if evaluate_entry_address == 0 {
                    return Err(format!(
                        "function node `{node_id}` was handed over with no evaluate entry"
                    ));
                }
                // SAFETY: the bootstrap hands over the address of an entry it
                // compiled to this signature and keeps alive for as long as
                // this helper reads, and the address was refused above when
                // null.
                let evaluate_entry = unsafe {
                    std::mem::transmute::<usize, ModuleHostTableFunctionNodeEvaluateEntry>(
                        evaluate_entry_address,
                    )
                };
                return Ok(Self::ThroughItsEvaluateEntry(
                    FunctionNodeCalledThroughItsEvaluateEntry {
                        evaluate_entry,
                        config_values,
                        input_values: vec![0.0; input_count],
                        state_values_of_the_call_under_way: vec![0.0; state_values.len()],
                        state_values,
                        output_values: vec![0.0; output_count],
                    },
                ));
            }
            if resumes_from_the_state_its_stream_carries {
                called_through
                    .call_method1("resume_from_state_values", (state_values.clone(),))
                    .map_err(|refusal| {
                        format!(
                            "function node `{node_id}` did not resume from the state its \
                             stream carries: {refusal}"
                        )
                    })?;
            }
            Ok(Self::AsPython(FunctionNodeCalledAsPython {
                input_count,
                run_as_python: called_through.clone().unbind(),
                state_values,
            }))
        })
    }

    fn input_count(&self) -> usize {
        match self {
            Self::ThroughItsEvaluateEntry(compiled) => compiled.input_values.len(),
            Self::AsPython(run_as_python) => run_as_python.input_count,
        }
    }

    fn runs_as_python(&self) -> bool {
        matches!(self, Self::AsPython(_))
    }

    fn state_values(&self) -> &[f64] {
        match self {
            Self::ThroughItsEvaluateEntry(compiled) => &compiled.state_values,
            Self::AsPython(run_as_python) => &run_as_python.state_values,
        }
    }

    /// Call the node with `input_values`, writing `output_values` and keeping
    /// the new state only when the call succeeds.
    fn call_at(
        &mut self,
        node_id: &str,
        input_values: &[&FunctionNodeValue],
        output_values: &mut [FunctionNodeValue],
        now_ns: i64,
    ) -> Result<()> {
        match self {
            Self::ThroughItsEvaluateEntry(compiled) => {
                for (input_index, (input_slot, input_value)) in compiled
                    .input_values
                    .iter_mut()
                    .zip(input_values)
                    .enumerate()
                {
                    *input_slot = input_value.as_a_number_for_an_input_of(node_id, input_index)?;
                }
                compiled.call_its_evaluate_entry_at(node_id, now_ns)?;
                for (output_value, compiled_output) in
                    output_values.iter_mut().zip(&compiled.output_values)
                {
                    *output_value = FunctionNodeValue::Number(*compiled_output);
                }
                Ok(())
            }
            Self::AsPython(run_as_python) => {
                run_as_python.call_at(node_id, input_values, output_values, now_ns)
            }
        }
    }
}

impl FunctionNodeCalledThroughItsEvaluateEntry {
    /// Call the node's evaluate entry, keeping the state it wrote only when it
    /// answers OK.
    fn call_its_evaluate_entry_at(&mut self, node_id: &str, now_ns: i64) -> Result<()> {
        self.state_values_of_the_call_under_way
            .copy_from_slice(&self.state_values);
        // SAFETY: the entry was compiled for this node's signature, so it reads
        // exactly `input_values.len()` inputs and `config_values.len()` config
        // values and writes at most `state_values.len()` state values and
        // `output_values.len()` outputs, each buffer that long and live across
        // the call.
        let status = unsafe {
            (self.evaluate_entry)(
                self.input_values.as_ptr(),
                self.config_values.as_ptr(),
                self.state_values_of_the_call_under_way.as_mut_ptr(),
                self.output_values.as_mut_ptr(),
                now_ns,
            )
        };
        if status != MODULE_HOST_TABLE_STATUS_OK {
            return Err(Error::Runtime(format!(
                "function node `{node_id}` refused its evaluation with status {status}"
            )));
        }
        std::mem::swap(
            &mut self.state_values,
            &mut self.state_values_of_the_call_under_way,
        );
        Ok(())
    }
}

impl FunctionNodeCalledAsPython {
    /// Call the node's body as Python, holding the interpreter's lock for the
    /// length of the call.
    fn call_at(
        &mut self,
        node_id: &str,
        input_values: &[&FunctionNodeValue],
        output_values: &mut [FunctionNodeValue],
        now_ns: i64,
    ) -> Result<()> {
        Python::attach(|python| {
            let raised = |refusal: PyErr| {
                Error::Runtime(format!(
                    "function node `{node_id}`, run as Python, raised: {refusal}"
                ))
            };
            let input_list = PyList::new(
                python,
                input_values
                    .iter()
                    .map(|input_value| input_value.to_python(python)),
            )
            .map_err(raised)?;
            let returned = self
                .run_as_python
                .bind(python)
                .call_method1("evaluate", (input_list, now_ns))
                .map_err(raised)?;
            let returned: Vec<Bound<'_, PyAny>> = returned.extract().map_err(raised)?;
            if returned.len() != output_values.len() {
                return Err(Error::Runtime(format!(
                    "function node `{node_id}`, run as Python, answered {} outputs, and it has {}",
                    returned.len(),
                    output_values.len()
                )));
            }
            let mut outputs_given = Vec::with_capacity(returned.len());
            for output in &returned {
                outputs_given.push(FunctionNodeValue::from_python(output).map_err(raised)?);
            }
            if !self.state_values.is_empty() {
                self.state_values = self
                    .run_as_python
                    .bind(python)
                    .call_method0("state_values")
                    .and_then(|state_values| state_values.extract())
                    .map_err(raised)?;
            }
            for (output_value, output_given) in output_values.iter_mut().zip(outputs_given) {
                *output_value = output_given;
            }
            Ok(())
        })
    }
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
        let value = match &island_output_node.output_values[self.island_output_index] {
            FunctionNodeValue::Number(number) => {
                FunctionNodeIslandOutputValue::ControlValue(*number)
            }
            FunctionNodeValue::Record(record) => Python::attach(|python| {
                records_message_payload_of(
                    &format!(
                        "function node `{}`'s output {}",
                        island_output_node.node_id, self.island_output_index
                    ),
                    record.bind(python),
                    &[],
                )
                .map(FunctionNodeIslandOutputValue::RecordsMessage)
                .map_err(|refusal| Error::Runtime(refusal.to_string()))
            })?,
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
}

/// One function node as the bootstrap hands it over: its id, its breadcrumb
/// number, its compiled evaluate entry's address or the object that runs it as
/// Python, where each input takes its value, its config values, its initial
/// state values and how many outputs it writes.
type FunctionNodeHandedOverByTheBootstrap = (
    String,
    u32,
    Py<PyAny>,
    Vec<(Option<(usize, usize)>, f64)>,
    Vec<f64>,
    Vec<f64>,
    usize,
);

impl FunctionNodeIslandEvaluatedInThisHelper {
    fn of_the_function_nodes_the_bootstrap_compiled(
        function_nodes: Vec<FunctionNodeHandedOverByTheBootstrap>,
        (island_output_node_index, island_output_index): (usize, usize),
        stream_clock_now_ns: Box<dyn Fn() -> i64 + Send + Sync>,
        call_breadcrumb_board_writer: Option<Arc<HelperProcessLossCountBoardWriter>>,
        state_resumed_from: &dyn Fn(u32) -> Option<Vec<f64>>,
    ) -> std::result::Result<Self, String> {
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
            ),
        ) in function_nodes.into_iter().enumerate()
        {
            let mut where_each_input_takes_its_value_read = Vec::new();
            for (source, pin_value) in where_each_input_takes_its_value {
                where_each_input_takes_its_value_read.push(match source {
                    None => WhereAFunctionNodeInputTakesItsValue::FromItsPin(
                        FunctionNodeValue::Number(pin_value),
                    ),
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
                        WhereAFunctionNodeInputTakesItsValue::FromAnEarlierNodesOutput {
                            node_index: source_node_index,
                            output_index,
                        }
                    }
                });
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
                where_each_input_takes_its_value_read.len(),
                the_state_a_function_node_starts_from(initial_state_values, state_resumed_from),
                resumes_from_the_state_its_stream_carries,
                output_count,
            )?;
            function_nodes_each_after_what_it_reads.push(FunctionNodeInAnIsland {
                node_id,
                breadcrumb_number,
                how_it_is_called,
                where_each_input_takes_its_value: where_each_input_takes_its_value_read,
                output_values: (0..output_count)
                    .map(|_| FunctionNodeValue::Number(0.0))
                    .collect(),
                writes_its_state_on_the_board,
            });
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
        Ok(Self {
            island_output_node_index,
            island_output_index,
            first_function_node_it_runs_as_python,
            evaluation: Mutex::new(FunctionNodeIslandEvaluation {
                function_nodes_each_after_what_it_reads,
                last_evaluated_at_ns: None,
                call_breadcrumb_board_writer,
            }),
            stream_clock_now_ns,
        })
    }
}

/// The function node a helper runs as its own processor, once it is promoted
/// out of its readers' islands: called with the values its process reads.
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
        initial_state_values,
        output_count,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        link_data_access: &PythonProcessorLinkDataAccess,
        node_id: String,
        called_through: Py<PyAny>,
        input_count: usize,
        config_values: Vec<f64>,
        initial_state_values: Vec<f64>,
        output_count: usize,
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
            input_count,
            the_state_a_function_node_starts_from(initial_state_values, state_resumed_from),
            resumes_from_the_state_its_stream_carries,
            output_count,
        )
        .map_err(PyValueError::new_err)?;
        Ok(Self {
            function_node: Mutex::new(FunctionNodeInAnIsland {
                node_id,
                breadcrumb_number,
                how_it_is_called,
                where_each_input_takes_its_value: Vec::new(),
                output_values: (0..output_count)
                    .map(|_| FunctionNodeValue::Number(0.0))
                    .collect(),
                writes_its_state_on_the_board,
            }),
            call_breadcrumb_board_writer,
        })
    }

    /// Call the node with `input_values` at the stream's time now, answering
    /// its outputs, a `float` for a number or bool and a `dict` as given;
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
        Ok(function_node
            .output_values
            .iter()
            .map(|output_value| output_value.to_python(python))
            .collect())
    }
}

/// Feed `input_port` from the island of `function_nodes`, each after what it
/// reads, evaluated at each read of the port; an island already feeding the
/// port is replaced, and each stateful node it shares resumes from its state.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, input_port, function_nodes, island_output))]
pub(crate) fn feed_an_input_port_from_a_function_node_island(
    link_data_access: &PythonProcessorLinkDataAccess,
    input_port: &str,
    function_nodes: Vec<FunctionNodeHandedOverByTheBootstrap>,
    island_output: (usize, usize),
) -> PyResult<()> {
    let input_mailboxes = link_data_access
        .input_mailboxes_of_this_helper()
        .ok_or_else(|| {
            PyValueError::new_err(
                "a function node island feeds only a helper process's own data plane",
            )
        })?;
    let states_the_island_it_replaces_last_returned: HashMap<u32, Vec<f64>> = input_mailboxes
        .the_state_each_stateful_function_node_of_the_island_feeding_last_returned(input_port)
        .into_iter()
        .collect();
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            function_nodes,
            island_output,
            Box::new(helper_stream_clock_now_ns),
            link_data_access.shared_loss_count_board_writer_of_this_helper(),
            &|breadcrumb_number| {
                states_the_island_it_replaces_last_returned
                    .get(&breadcrumb_number)
                    .cloned()
                    .or_else(|| {
                        link_data_access.the_state_a_function_node_resumes_from(breadcrumb_number)
                    })
            },
        )
        .map_err(PyValueError::new_err)?;
    if let Some(function_node_id) = island.the_first_function_node_it_runs_as_python()
        && link_data_access.input_port_is_lent_to_native_code(input_port)
    {
        return Err(PyValueError::new_err(format!(
            "function node `{function_node_id}` cannot feed input port {input_port:?}: it runs \
             as Python, since its body does not compile, and the port is lent to native code, \
             whose read holds no Python interpreter lock to call it with. To fix: make \
             `{function_node_id}` compile, or read the port from Python"
        )));
    }
    input_mailboxes.feed_an_input_port_from_a_function_node_island(input_port, Arc::new(island));
    Ok(())
}

/// Stop feeding `input_port` from the island feeding it; whether one did.
#[pyfunction]
#[pyo3(signature = (*, link_data_access, input_port))]
pub(crate) fn stop_feeding_an_input_port_from_a_function_node_island(
    link_data_access: &PythonProcessorLinkDataAccess,
    input_port: &str,
) -> PyResult<bool> {
    let input_mailboxes = link_data_access
        .input_mailboxes_of_this_helper()
        .ok_or_else(|| {
            PyValueError::new_err(
                "a function node island feeds only a helper process's own data plane",
            )
        })?;
    Ok(input_mailboxes.stop_feeding_an_input_port_from_a_function_node_island(input_port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// The handover of a compiled entry: its address, as the bootstrap passes
    /// it, a Python int.
    fn compiled_to(evaluate_entry: ModuleHostTableFunctionNodeEvaluateEntry) -> Py<PyAny> {
        Python::initialize();
        Python::attach(|python| {
            (evaluate_entry as *const () as usize)
                .into_pyobject(python)
                .unwrap()
                .into_any()
                .unbind()
        })
    }

    fn a_stream_clock_the_test_sets() -> (Arc<AtomicI64>, Box<dyn Fn() -> i64 + Send + Sync>) {
        let stream_time_ns = Arc::new(AtomicI64::new(0));
        let read_by_the_island = Arc::clone(&stream_time_ns);
        (
            stream_time_ns,
            Box::new(move || read_by_the_island.load(Ordering::SeqCst)),
        )
    }

    unsafe extern "C" fn add_one_and_count_calls(
        input_values: *const f64,
        _config_values: *const f64,
        state_values: *mut f64,
        output_values: *mut f64,
        _now_ns: i64,
    ) -> i32 {
        unsafe {
            *state_values += 1.0;
            *output_values = *input_values + 1.0;
        }
        MODULE_HOST_TABLE_STATUS_OK
    }

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
                    vec![(None, 2.0)],
                    vec![],
                    vec![0.0],
                    1,
                ),
                (
                    "scale".to_string(),
                    1,
                    compiled_to(scale_by_config_plus_seconds),
                    vec![(Some((0, 0)), 0.0)],
                    vec![10.0],
                    vec![],
                    1,
                ),
            ],
            (1, 0),
            stream_clock_now_ns,
            None,
            &|_| None,
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
            FunctionNodeIslandOutputValue::ControlValue((2.0 + 1.0) * 10.0 + 1.0)
        );
        assert_eq!(output.evaluated_at_ns, 1_000_000_000);
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
    fn a_refused_evaluation_keeps_the_state_it_was_called_with_and_names_the_node() {
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
                )],
                (0, 0),
                a_stream_clock_the_test_sets().1,
                None,
                &|_| None,
            )
            .unwrap();
        let refusal = island
            .evaluate_the_island_output_for_this_read()
            .unwrap_err();
        assert!(refusal.to_string().contains("`refuses`"), "{refusal}");
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
            .create_helper_process_loss_count_board(
                &board_name,
                Vec::new(),
                vec![breadcrumb_number],
            )
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
                    vec![(None, 0.0)],
                    vec![],
                    vec![0.0],
                    1,
                )],
                (0, 0),
                stream_clock_now_ns,
                Some(board_writer),
                &|breadcrumb_number| (breadcrumb_number == 4).then(|| vec![41.0]),
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
                    vec![(Some((0, 0)), 0.0)],
                    vec![],
                    vec![0.0],
                    1,
                )],
                (0, 0),
                a_stream_clock_the_test_sets().1,
                None,
                &|_| None,
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
                    vec![(None, 2.0)],
                    vec![],
                    vec![0.0],
                    1,
                ),
                (
                    "tally".to_string(),
                    1,
                    run_as_python_counting_its_calls(),
                    vec![(Some((0, 0)), 0.0)],
                    vec![],
                    vec![0.0],
                    2,
                ),
            ],
            island_output,
            stream_clock_now_ns,
            None,
            state_resumed_from,
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
            FunctionNodeIslandOutputValue::ControlValue(42.0),
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
    fn a_node_run_as_python_that_raises_is_refused_naming_it_and_what_it_raised() {
        let island =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![(
                    "tally".to_string(),
                    5,
                    run_as_python_counting_its_calls(),
                    vec![(None, -1.0)],
                    vec![],
                    vec![0.0],
                    2,
                )],
                (0, 1),
                a_stream_clock_the_test_sets().1,
                None,
                &|_| None,
            )
            .unwrap();
        let refusal = island
            .evaluate_the_island_output_for_this_read()
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("`tally`, run as Python, raised"),
            "{refusal}"
        );
        assert!(refusal.contains("a negative input"), "{refusal}");
    }

    #[test]
    fn a_compiled_node_handed_a_dict_is_refused_naming_it() {
        let island =
            FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
                vec![
                    (
                        "tally".to_string(),
                        0,
                        run_as_python_counting_its_calls(),
                        vec![(None, 1.0)],
                        vec![],
                        vec![],
                        2,
                    ),
                    (
                        "add_one".to_string(),
                        1,
                        compiled_to(add_one_and_count_calls),
                        vec![(Some((0, 0)), 0.0)],
                        vec![],
                        vec![0.0],
                        1,
                    ),
                ],
                (1, 0),
                a_stream_clock_the_test_sets().1,
                None,
                &|_| None,
            )
            .unwrap();
        let refusal = island
            .evaluate_the_island_output_for_this_read()
            .unwrap_err()
            .to_string();
        assert!(
            refusal.contains("`add_one` runs compiled and takes a number on input 0"),
            "{refusal}"
        );
    }
}
