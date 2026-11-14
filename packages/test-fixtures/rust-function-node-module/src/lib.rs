// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The native half of `rust-function-node-fixture`, imported as
//! `rust_function_node_fixture._native`: a stateless, a stateful and a gating
//! Rust function node, each handed to the Python half as its declaration.

use pyo3::prelude::*;
use tatolab_module_sdk::function_node::{
    FunctionNodeRecord, State, Stateful, UnreadMessage, function_node,
};

#[function_node]
#[meta(description = "`x` scaled by `gain`, then moved by `offset`.")]
fn scale_and_offset(
    x: f64,
    #[pin_default(0.5)] gain: f64,
    #[pin_default(0.5)] offset: f64,
) -> f64 {
    x * gain + offset
}

/// How far a `CountsItsOwnCalls` has counted.
#[derive(Clone, Copy, Default, FunctionNodeRecord)]
struct CallsCountedSoFar {
    calls: f64,
}

/// How much a `CountsItsOwnCalls` counts each call by.
#[derive(Clone, Copy, FunctionNodeRecord)]
struct CountsItsOwnCallsConfig {
    step: f64,
}

impl Default for CountsItsOwnCallsConfig {
    fn default() -> Self {
        Self { step: 1.0 }
    }
}

#[function_node]
#[meta(description = "The number of times it was called, counted by the config's `step`.")]
fn counts_its_own_calls(
    config: CountsItsOwnCallsConfig,
    previous: State<CallsCountedSoFar>,
) -> Stateful<f64, CallsCountedSoFar> {
    let calls = previous.calls + config.step;
    (calls, CallsCountedSoFar { calls })
}

#[function_node]
#[meta(description = "`value` handed on unread while `open`, and nothing while closed.")]
fn rust_gate(value: UnreadMessage, #[pin_default(true)] open: bool) -> Option<UnreadMessage> {
    open.then_some(value)
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let python = module.py();
    module.add(
        "ScaleAndOffset",
        scale_and_offset_function_node_declaration().to_python_declaration(python)?,
    )?;
    module.add(
        "CountsItsOwnCalls",
        counts_its_own_calls_function_node_declaration().to_python_declaration(python)?,
    )?;
    module.add(
        "RustGate",
        rust_gate_function_node_declaration().to_python_declaration(python)?,
    )?;
    Ok(())
}
