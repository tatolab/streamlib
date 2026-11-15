// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How one function node of an island is called: through its compiled evaluate entry, or as
//! Python in this helper's interpreter.

use pyo3::types::{PyInt, PyList};
use streamlib::sdk::error::{Error, Result};
use tatolab_module_sdk::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_OK, ModuleHostTableFunctionNodeEvaluateEntry,
};

use super::*;

/// A function node's evaluate entry, compiled for its signature, and the f64
/// buffers it is called with.
pub(super) struct FunctionNodeCalledThroughItsEvaluateEntry {
    evaluate_entry: ModuleHostTableFunctionNodeEvaluateEntry,
    /// The values a call's message inputs and outputs carry as tokens: a
    /// message input as its index plus one, or 0 for none; and the slot past
    /// the outputs where each output that may hand out nothing says whether it
    /// handed one out.
    values_carried_as_tokens: FunctionNodeValuesCarriedAsTokensThroughAnEvaluateEntry,
    config_values: Vec<f64>,
    /// The index among `config_values` each input past the node's own sets: a
    /// config field it exposes as an input, linked.
    config_value_indices_set_by_the_inputs_past_its_own: Vec<usize>,
    input_values: Vec<f64>,
    state_values: Vec<f64>,
    state_values_of_the_call_under_way: Vec<f64>,
    output_values: Vec<f64>,
}

/// The inputs and outputs of a compiled call that carry a message as a token,
/// and the slot of each output's handed-out flag.
///
/// ABI contract with the evaluate entry the bootstrap compiles: a message input
/// carries `input_index + 1`, or `0.0` when it holds none; a message output
/// carries the token of the input it hands on, or `0.0`; and each output that
/// may hand out nothing has a slot past every output, in output order, holding
/// `1.0` when it handed one out and `0.0` when it did not.
#[derive(Debug, Default)]
pub(super) struct FunctionNodeValuesCarriedAsTokensThroughAnEvaluateEntry {
    inputs_carrying_an_unread_message: Vec<usize>,
    outputs_carrying_an_unread_message: Vec<usize>,
    handed_out_slot_of_each_output: Vec<Option<usize>>,
}

/// A function node whose body runs as Python in this helper's interpreter,
/// through the object the bootstrap built for it.
pub(super) struct FunctionNodeCalledAsPython {
    input_count: usize,
    /// Answers `evaluate(input_values, now_ns)` with its outputs in port order,
    /// keeping its state, and `state_values()` with that state as f64s when
    /// its state record is numbers and bools.
    run_as_python: Py<PyAny>,
    /// The state it last answered, as written on the board.
    state_values: Vec<f64>,
}

/// How one function node is called.
pub(super) enum HowAFunctionNodeIsCalled {
    ThroughItsEvaluateEntry(FunctionNodeCalledThroughItsEvaluateEntry),
    AsPython(FunctionNodeCalledAsPython),
}

impl HowAFunctionNodeIsCalled {
    /// How the bootstrap hands a node over: its compiled entry's address, or
    /// the object that runs its body as Python. A node run as Python is told
    /// the state its stream carries for it, when it carries one. Of a compiled
    /// node's `input_count` inputs, the last set the config values
    /// `config_value_indices_set_by_the_inputs_past_its_own` names.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handed_over_by_the_bootstrap(
        node_id: &str,
        called_through: Py<PyAny>,
        config_values: Vec<f64>,
        config_value_indices_set_by_the_inputs_past_its_own: Vec<usize>,
        input_count: usize,
        state_values: Vec<f64>,
        resumes_from_the_state_its_stream_carries: bool,
        output_count: usize,
        how_it_carries_its_values: &HowAFunctionNodeCarriesValuesBeyondNumbers,
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
                if let Some(config_value_index) =
                    config_value_indices_set_by_the_inputs_past_its_own
                        .iter()
                        .find(|config_value_index| **config_value_index >= config_values.len())
                {
                    return Err(format!(
                        "function node `{node_id}` sets config value {config_value_index} from \
                         an input, and it takes {} config values",
                        config_values.len()
                    ));
                }
                let own_input_count = input_count
                    .checked_sub(config_value_indices_set_by_the_inputs_past_its_own.len())
                    .ok_or_else(|| {
                        format!(
                            "function node `{node_id}` sets {} config values from inputs, and \
                             it takes {input_count} inputs",
                            config_value_indices_set_by_the_inputs_past_its_own.len()
                        )
                    })?;
                let values_carried_as_tokens =
                    FunctionNodeValuesCarriedAsTokensThroughAnEvaluateEntry::of(
                        node_id,
                        how_it_carries_its_values,
                        own_input_count,
                        output_count,
                    )?;
                let output_slot_count = output_count
                    + how_it_carries_its_values
                        .outputs_that_may_hand_out_nothing
                        .len();
                return Ok(Self::ThroughItsEvaluateEntry(
                    FunctionNodeCalledThroughItsEvaluateEntry {
                        evaluate_entry,
                        values_carried_as_tokens,
                        config_values,
                        config_value_indices_set_by_the_inputs_past_its_own,
                        input_values: vec![0.0; own_input_count],
                        state_values_of_the_call_under_way: vec![0.0; state_values.len()],
                        state_values,
                        output_values: vec![0.0; output_slot_count],
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

    pub(super) fn input_count(&self) -> usize {
        match self {
            Self::ThroughItsEvaluateEntry(compiled) => {
                compiled.input_values.len()
                    + compiled
                        .config_value_indices_set_by_the_inputs_past_its_own
                        .len()
            }
            Self::AsPython(run_as_python) => run_as_python.input_count,
        }
    }

    pub(super) fn runs_as_python(&self) -> bool {
        matches!(self, Self::AsPython(_))
    }

    pub(super) fn state_values(&self) -> &[f64] {
        match self {
            Self::ThroughItsEvaluateEntry(compiled) => &compiled.state_values,
            Self::AsPython(run_as_python) => &run_as_python.state_values,
        }
    }

    /// Call the node with `input_values`, writing `output_values` and keeping
    /// the new state only when the call succeeds.
    pub(super) fn call_at(
        &mut self,
        node_id: &str,
        input_values: &[&FunctionNodeValue],
        output_values: &mut [FunctionNodeValue],
        now_ns: i64,
    ) -> Result<()> {
        match self {
            Self::ThroughItsEvaluateEntry(compiled) => {
                let own_input_count = compiled.input_values.len();
                for (input_index, input_value) in input_values.iter().enumerate() {
                    let input_number = if compiled
                        .values_carried_as_tokens
                        .inputs_carrying_an_unread_message
                        .contains(&input_index)
                    {
                        if input_value.is_nothing() {
                            0.0
                        } else {
                            (input_index + 1) as f64
                        }
                    } else {
                        input_value.as_a_number_for_an_input_of(node_id, input_index)?
                    };
                    match input_index.checked_sub(own_input_count) {
                        None => compiled.input_values[input_index] = input_number,
                        Some(past_its_own) => {
                            if let Some(config_value_index) = compiled
                                .config_value_indices_set_by_the_inputs_past_its_own
                                .get(past_its_own)
                            {
                                compiled.config_values[*config_value_index] = input_number;
                            }
                        }
                    }
                }
                compiled.call_its_evaluate_entry_at(node_id, now_ns)?;
                let values_the_outputs_took = (0..output_values.len())
                    .map(|output_index| {
                        compiled.the_value_an_output_took(node_id, output_index, input_values)
                    })
                    .collect::<Result<Vec<FunctionNodeValue>>>()?;
                compiled.keep_the_state_of_the_call_just_made();
                for (output_value, value_the_output_took) in
                    output_values.iter_mut().zip(values_the_outputs_took)
                {
                    *output_value = value_the_output_took;
                }
                Ok(())
            }
            Self::AsPython(run_as_python) => {
                run_as_python.call_at(node_id, input_values, output_values, now_ns)
            }
        }
    }
}

impl FunctionNodeValuesCarriedAsTokensThroughAnEvaluateEntry {
    fn of(
        node_id: &str,
        how_it_carries_its_values: &HowAFunctionNodeCarriesValuesBeyondNumbers,
        own_input_count: usize,
        output_count: usize,
    ) -> std::result::Result<Self, String> {
        let refusal = |what: &str, index: usize, count: usize| {
            format!(
                "function node `{node_id}` names {what} {index} as carrying a message or \
                 nothing, and it has {count}"
            )
        };
        for input_index in &how_it_carries_its_values.inputs_carrying_an_unread_message {
            if *input_index >= own_input_count {
                return Err(refusal("input", *input_index, own_input_count));
            }
        }
        for output_index in how_it_carries_its_values
            .outputs_carrying_an_unread_message
            .iter()
            .chain(&how_it_carries_its_values.outputs_that_may_hand_out_nothing)
        {
            if *output_index >= output_count {
                return Err(refusal("output", *output_index, output_count));
            }
        }
        let mut handed_out_slot_of_each_output = vec![None; output_count];
        for (handed_out_slot_index, output_index) in how_it_carries_its_values
            .outputs_that_may_hand_out_nothing
            .iter()
            .enumerate()
        {
            handed_out_slot_of_each_output[*output_index] =
                Some(output_count + handed_out_slot_index);
        }
        Ok(Self {
            inputs_carrying_an_unread_message: how_it_carries_its_values
                .inputs_carrying_an_unread_message
                .clone(),
            outputs_carrying_an_unread_message: how_it_carries_its_values
                .outputs_carrying_an_unread_message
                .clone(),
            handed_out_slot_of_each_output,
        })
    }
}

impl FunctionNodeCalledThroughItsEvaluateEntry {
    /// The value output `output_index` took at the call just made: nothing
    /// when its handed-out slot says so, the input its token names for a
    /// message output, and the number it wrote otherwise.
    fn the_value_an_output_took(
        &self,
        node_id: &str,
        output_index: usize,
        input_values: &[&FunctionNodeValue],
    ) -> Result<FunctionNodeValue> {
        let tokens = &self.values_carried_as_tokens;
        let handed_one_out = tokens
            .handed_out_slot_of_each_output
            .get(output_index)
            .copied()
            .flatten()
            .is_none_or(|handed_out_slot| self.output_values[handed_out_slot] != 0.0);
        if !handed_one_out {
            return Ok(FunctionNodeValue::Nothing);
        }
        let written = self.output_values[output_index];
        if !tokens
            .outputs_carrying_an_unread_message
            .contains(&output_index)
        {
            return Ok(FunctionNodeValue::Number(written));
        }
        if written == 0.0 {
            return Ok(FunctionNodeValue::Nothing);
        }
        let named_input_index = (written - 1.0) as usize;
        let names_a_message_input = written.fract() == 0.0
            && written >= 1.0
            && tokens
                .inputs_carrying_an_unread_message
                .contains(&named_input_index);
        match input_values.get(named_input_index) {
            Some(input_value) if names_a_message_input => Ok(input_value.duplicated()),
            _ => Err(Error::Runtime(format!(
                "function node `{node_id}` handed out {written} on output {output_index}, which \
                 carries a message it was handed, and names none of its message inputs"
            ))),
        }
    }

    /// Call the node's evaluate entry, writing the state it answers aside
    /// until [`Self::keep_the_state_of_the_call_just_made`].
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
        Ok(())
    }

    /// Keep the state the last call wrote, once every output it handed out is
    /// one the island takes.
    fn keep_the_state_of_the_call_just_made(&mut self) {
        std::mem::swap(
            &mut self.state_values,
            &mut self.state_values_of_the_call_under_way,
        );
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
            let input_objects = input_values
                .iter()
                .map(|input_value| input_value.to_python(python))
                .collect::<PyResult<Vec<_>>>()
                .map_err(raised)?;
            let input_list = PyList::new(python, input_objects).map_err(raised)?;
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
