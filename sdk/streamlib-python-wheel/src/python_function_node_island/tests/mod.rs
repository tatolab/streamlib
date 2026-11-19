// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::function_node_island_evaluators::PinValueTheBootstrapHandsOver;
use super::*;
use std::sync::atomic::{AtomicI64, Ordering};
use streamlib::sdk::iceoryx2::{ChannelsBlockDescriptorBody, ChannelsBlockSampleType};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_OK, ModuleHostTableFunctionNodeEvaluateEntry,
};

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

/// Where an island under test notes the raises of its function nodes: a record
/// of its own, under the input port `island`.
fn raises_noted_in_a_record_of_its_own() -> WhereAnIslandNotesTheRaisesOfItsFunctionNodes {
    WhereAnIslandNotesTheRaisesOfItsFunctionNodes {
        function_node_raises_in_this_helper: Arc::default(),
        island_input_port: "island".to_string(),
    }
}

/// What each function node of `island` raised at its last call, by node id.
fn the_raises_noted_by(
    island: &FunctionNodeIslandEvaluatedInThisHelper,
) -> std::collections::BTreeMap<String, String> {
    island
        .evaluation
        .lock()
        .where_it_notes_the_raises_of_its_function_nodes
        .function_node_raises_in_this_helper
        .why_each_function_node_is_raising()
}

mod island_evaluation_tests;
mod links_from_outside_the_island_tests;
