// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A function node island evaluated inside this helper's reads: each node's
//! compiled evaluate entry called in turn, once per stream time.

use std::sync::Arc;

use parking_lot::Mutex;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_ITS_PROCESSOR,
    FunctionNodeIslandEvaluatedAtEachRead, FunctionNodeIslandOutputForOneRead,
    HelperProcessLossCountBoardWriter,
};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_OK, ModuleHostTableFunctionNodeEvaluateEntry,
};

use crate::python_helper_stream_clock::helper_stream_clock_now_ns;
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

/// Where one input of a function node in an island takes its value from.
#[derive(Debug, Clone, Copy, PartialEq)]
enum WhereAFunctionNodeInputTakesItsValue {
    /// An output of a node evaluated earlier in the island.
    FromAnEarlierNodesOutput {
        node_index: usize,
        output_index: usize,
    },
    /// The pin's value, as nothing is linked to it.
    FromItsPin(f64),
}

/// One function node of an island, compiled, and the values it is called with.
struct FunctionNodeCompiledIntoAnIsland {
    node_id: String,
    breadcrumb_number: u32,
    evaluate_entry: ModuleHostTableFunctionNodeEvaluateEntry,
    where_each_input_takes_its_value: Vec<WhereAFunctionNodeInputTakesItsValue>,
    config_values: Vec<f64>,
    input_values: Vec<f64>,
    state_values: Vec<f64>,
    state_values_of_the_call_under_way: Vec<f64>,
    output_values: Vec<f64>,
}

/// The values every node of an island last took, and the stream time they were
/// evaluated at.
struct FunctionNodeIslandEvaluation {
    function_nodes_each_after_what_it_reads: Vec<FunctionNodeCompiledIntoAnIsland>,
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
            for (input_value, where_it_takes_its_value) in function_node
                .input_values
                .iter_mut()
                .zip(&function_node.where_each_input_takes_its_value)
            {
                *input_value = match *where_it_takes_its_value {
                    WhereAFunctionNodeInputTakesItsValue::FromAnEarlierNodesOutput {
                        node_index,
                        output_index,
                    } => evaluated_earlier[node_index].output_values[output_index],
                    WhereAFunctionNodeInputTakesItsValue::FromItsPin(pin_value) => pin_value,
                };
            }
            call_noting_its_breadcrumb_on(
                self.call_breadcrumb_board_writer.as_deref(),
                function_node,
                now_ns,
            )?;
        }
        self.last_evaluated_at_ns = Some(now_ns);
        Ok(())
    }
}

/// Call `function_node` at `now_ns` with its breadcrumb noted on
/// `call_breadcrumb_board_writer` for the length of the call.
fn call_noting_its_breadcrumb_on(
    call_breadcrumb_board_writer: Option<&HelperProcessLossCountBoardWriter>,
    function_node: &mut FunctionNodeCompiledIntoAnIsland,
    now_ns: i64,
) -> Result<()> {
    let call_under_way = call_breadcrumb_board_writer.map(|board_writer| {
        board_writer.note_a_function_node_call_started(
            function_node.breadcrumb_number,
            monotonic_clock_now_ns(),
        )
    });
    let called = function_node.call_its_evaluate_entry_at(now_ns);
    if let (Some(board_writer), Some(call_under_way)) =
        (call_breadcrumb_board_writer, call_under_way)
    {
        board_writer.note_the_function_node_call_returned(call_under_way);
    }
    called
}

impl FunctionNodeCompiledIntoAnIsland {
    /// Call the node's evaluate entry, keeping the state it wrote only when it
    /// answers OK.
    fn call_its_evaluate_entry_at(&mut self, now_ns: i64) -> Result<()> {
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
                "function node `{}` refused its evaluation with status {status}",
                self.node_id
            )));
        }
        std::mem::swap(
            &mut self.state_values,
            &mut self.state_values_of_the_call_under_way,
        );
        Ok(())
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
        Ok(FunctionNodeIslandOutputForOneRead {
            value: evaluation.function_nodes_each_after_what_it_reads
                [self.island_output_node_index]
                .output_values[self.island_output_index],
            evaluated_at_ns: now_ns,
        })
    }
}

/// One function node as the bootstrap hands it over: its id, its breadcrumb
/// number, its evaluate entry's address, where each input takes its value, its
/// config values, its initial state values and how many outputs it writes.
type FunctionNodeHandedOverByTheBootstrap = (
    String,
    u32,
    usize,
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
    ) -> std::result::Result<Self, String> {
        let mut function_nodes_each_after_what_it_reads = Vec::with_capacity(function_nodes.len());
        for (
            node_index,
            (
                node_id,
                breadcrumb_number,
                evaluate_entry_address,
                where_each_input_takes_its_value,
                config_values,
                initial_state_values,
                output_count,
            ),
        ) in function_nodes.into_iter().enumerate()
        {
            if evaluate_entry_address == 0 {
                return Err(format!(
                    "function node `{node_id}` was handed over with no evaluate entry"
                ));
            }
            let mut where_each_input_takes_its_value_read = Vec::new();
            for (source, pin_value) in where_each_input_takes_its_value {
                where_each_input_takes_its_value_read.push(match source {
                    None => WhereAFunctionNodeInputTakesItsValue::FromItsPin(pin_value),
                    Some((source_node_index, output_index)) => {
                        let reads_an_output_evaluated_earlier = source_node_index < node_index
                            && output_index
                                < function_nodes_each_after_what_it_reads
                                    .get(source_node_index)
                                    .map_or(0, |earlier: &FunctionNodeCompiledIntoAnIsland| {
                                        earlier.output_values.len()
                                    });
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
            // SAFETY: the bootstrap hands over the address of an entry it
            // compiled to this signature and keeps alive for as long as this
            // helper reads, and the address was refused above when null.
            let evaluate_entry = unsafe {
                std::mem::transmute::<usize, ModuleHostTableFunctionNodeEvaluateEntry>(
                    evaluate_entry_address,
                )
            };
            function_nodes_each_after_what_it_reads.push(FunctionNodeCompiledIntoAnIsland {
                node_id,
                breadcrumb_number,
                evaluate_entry,
                input_values: vec![0.0; where_each_input_takes_its_value_read.len()],
                where_each_input_takes_its_value: where_each_input_takes_its_value_read,
                config_values,
                state_values_of_the_call_under_way: vec![0.0; initial_state_values.len()],
                state_values: initial_state_values,
                output_values: vec![0.0; output_count],
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
        Ok(Self {
            island_output_node_index,
            island_output_index,
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
    function_node: Mutex<FunctionNodeCompiledIntoAnIsland>,
    call_breadcrumb_board_writer: Option<Arc<HelperProcessLossCountBoardWriter>>,
}

#[pymethods]
impl FunctionNodeEvaluatedAsItsHelpersProcessor {
    #[new]
    #[pyo3(signature = (
        *,
        link_data_access,
        node_id,
        evaluate_entry_address,
        input_count,
        config_values,
        initial_state_values,
        output_count,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        link_data_access: &PythonProcessorLinkDataAccess,
        node_id: String,
        evaluate_entry_address: usize,
        input_count: usize,
        config_values: Vec<f64>,
        initial_state_values: Vec<f64>,
        output_count: usize,
    ) -> PyResult<Self> {
        if evaluate_entry_address == 0 {
            return Err(PyValueError::new_err(format!(
                "function node `{node_id}` was handed over with no evaluate entry"
            )));
        }
        // SAFETY: the bootstrap hands over the address of an entry it compiled
        // to this signature and keeps alive for as long as this helper runs,
        // and the address was refused above when null.
        let evaluate_entry = unsafe {
            std::mem::transmute::<usize, ModuleHostTableFunctionNodeEvaluateEntry>(
                evaluate_entry_address,
            )
        };
        Ok(Self {
            function_node: Mutex::new(FunctionNodeCompiledIntoAnIsland {
                node_id,
                breadcrumb_number:
                    BREADCRUMB_NUMBER_OF_THE_FUNCTION_NODE_A_HELPER_RUNS_AS_ITS_PROCESSOR,
                evaluate_entry,
                where_each_input_takes_its_value: Vec::new(),
                config_values,
                input_values: vec![0.0; input_count],
                state_values_of_the_call_under_way: vec![0.0; initial_state_values.len()],
                state_values: initial_state_values,
                output_values: vec![0.0; output_count],
            }),
            call_breadcrumb_board_writer: link_data_access
                .shared_loss_count_board_writer_of_this_helper(),
        })
    }

    /// Call the node with `input_values` at the stream's time now, answering
    /// its outputs; refused when it refuses, keeping the state it had.
    fn evaluate(&self, python: Python<'_>, input_values: Vec<f64>) -> PyResult<Vec<f64>> {
        let now_ns = helper_stream_clock_now_ns();
        python.detach(|| {
            let mut function_node = self.function_node.lock();
            if input_values.len() != function_node.input_values.len() {
                return Err(PyValueError::new_err(format!(
                    "function node `{}` takes {} inputs, and was called with {}",
                    function_node.node_id,
                    function_node.input_values.len(),
                    input_values.len()
                )));
            }
            function_node.input_values.copy_from_slice(&input_values);
            call_noting_its_breadcrumb_on(
                self.call_breadcrumb_board_writer.as_deref(),
                &mut function_node,
                now_ns,
            )
            .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
            Ok(function_node.output_values.clone())
        })
    }
}

/// Feed `input_port` from the island of `function_nodes`, each after what it
/// reads, evaluated at each read of the port.
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
    let island =
        FunctionNodeIslandEvaluatedInThisHelper::of_the_function_nodes_the_bootstrap_compiled(
            function_nodes,
            island_output,
            Box::new(helper_stream_clock_now_ns),
            link_data_access.shared_loss_count_board_writer_of_this_helper(),
        )
        .map_err(PyValueError::new_err)?;
    input_mailboxes.feed_an_input_port_from_a_function_node_island(input_port, Arc::new(island));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

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
                    add_one_and_count_calls as *const () as usize,
                    vec![(None, 2.0)],
                    vec![],
                    vec![0.0],
                    1,
                ),
                (
                    "scale".to_string(),
                    1,
                    scale_by_config_plus_seconds as *const () as usize,
                    vec![(Some((0, 0)), 0.0)],
                    vec![10.0],
                    vec![],
                    1,
                ),
            ],
            (1, 0),
            stream_clock_now_ns,
            None,
        )
        .unwrap()
    }

    #[test]
    fn each_node_reads_the_one_before_it_and_its_pin_and_config() {
        let (stream_time_ns, stream_clock_now_ns) = a_stream_clock_the_test_sets();
        stream_time_ns.store(1_000_000_000, Ordering::SeqCst);
        let island = an_island_of_add_one_then_scale(stream_clock_now_ns);
        let output = island.evaluate_the_island_output_for_this_read().unwrap();
        assert_eq!(output.value, (2.0 + 1.0) * 10.0 + 1.0);
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
            .state_values[0];
        assert_eq!(calls_at_one_time, 1.0);
        stream_time_ns.store(6, Ordering::SeqCst);
        island.evaluate_the_island_output_for_this_read().unwrap();
        assert_eq!(
            island
                .evaluation
                .lock()
                .function_nodes_each_after_what_it_reads[0]
                .state_values[0],
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
                    refuse_after_writing_state as *const () as usize,
                    vec![],
                    vec![],
                    vec![1.0],
                    1,
                )],
                (0, 0),
                a_stream_clock_the_test_sets().1,
                None,
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
                .state_values,
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
                    add_one_and_count_calls as *const () as usize,
                    vec![(Some((0, 0)), 0.0)],
                    vec![],
                    vec![0.0],
                    1,
                )],
                (0, 0),
                a_stream_clock_the_test_sets().1,
                None,
            )
            .err()
            .unwrap();
        assert!(
            refusal.contains("not an output of a node evaluated before it"),
            "{refusal}"
        );
    }
}
