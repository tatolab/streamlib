// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Rust function nodes: a `#[function_node]` function exports the evaluate entry
//! the read seam calls, beside the declaration its module's Python half hands
//! `node.rust_function(...)`.
//!
//! The grammar is `@node.function`'s. Each plain parameter is an input pin, and
//! `#[pin_default(...)]` its value when nothing is wired. A parameter typed
//! [`Time`] is handed the reader's stream time, one typed [`State<Record>`] the
//! state the last call returned in its [`Stateful<Output, Record>`], and one
//! named `config` the node's config record. The return is the output `result`,
//! or one output per field of a returned record. A value is an `f64`, an `i64`
//! or a `bool`; a record is a struct of them deriving `FunctionNodeRecord`,
//! whose `Default` is its first state or its config's defaults.
//!
//! A pin typed [`UnreadMessage`] takes a message of any family, which the body
//! can only hand on, to an `UnreadMessage` output; `Option<UnreadMessage>` takes
//! `None` when it holds none. An output typed `Option<T>` hands out nothing on
//! `None`, and a record of outputs types each such field `Option<T>`.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_OK, MODULE_HOST_TABLE_STATUS_REFUSED,
    ModuleHostTableFunctionNodeEvaluateEntry, ModuleHostTableStatus,
};

#[cfg(feature = "function-node")]
pub use tatolab_module_sdk_macros::{FunctionNodeRecord, function_node};

/// The reader's stream time at the read, in integer nanoseconds on the stream's monotonic clock.
pub type Time = i64;

/// The state a stateful function node's last call returned, or its record's `Default` on the first call.
pub type State<StateRecord> = StateRecord;

/// What a stateful function node returns: its output, then the state its next call takes.
pub type Stateful<FunctionNodeOutputValue, StateRecord> = (FunctionNodeOutputValue, StateRecord);

/// The value a pin, a config field or a state field carries, by its wire name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionNodeValueType {
    /// An `f64`; wire name `float`.
    Float,
    /// An `i64`, carried exactly up to 2^53; wire name `int`.
    Int,
    /// A `bool`, carried as 0 or 1; wire name `bool`.
    Bool,
    /// A message handed on unread, carried as the index of the input holding
    /// it plus one, or 0 for none; wire name `unread_message`.
    UnreadMessage,
}

impl FunctionNodeValueType {
    /// The name a function node's declaration spells this value type with.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Float => "float",
            Self::Int => "int",
            Self::Bool => "bool",
            Self::UnreadMessage => "unread_message",
        }
    }
}

/// A value an evaluate entry carries as one f64.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a number or a bool a function node carries as one f64",
    label = "a config field, a state field and a pin default are `f64`, `i64` or `bool`"
)]
pub trait FunctionNodeValue: Copy {
    /// Its value type.
    const VALUE_TYPE: FunctionNodeValueType;
    /// The f64 the evaluate entry carries it as.
    fn to_function_node_f64(self) -> f64;
    /// The value an f64 the evaluate entry carries stands for.
    fn from_function_node_f64(value: f64) -> Self;
}

impl FunctionNodeValue for f64 {
    const VALUE_TYPE: FunctionNodeValueType = FunctionNodeValueType::Float;
    fn to_function_node_f64(self) -> f64 {
        self
    }
    fn from_function_node_f64(value: f64) -> Self {
        value
    }
}

impl FunctionNodeValue for i64 {
    const VALUE_TYPE: FunctionNodeValueType = FunctionNodeValueType::Int;
    fn to_function_node_f64(self) -> f64 {
        self as f64
    }
    fn from_function_node_f64(value: f64) -> Self {
        value as i64
    }
}

impl FunctionNodeValue for bool {
    const VALUE_TYPE: FunctionNodeValueType = FunctionNodeValueType::Bool;
    fn to_function_node_f64(self) -> f64 {
        if self { 1.0 } else { 0.0 }
    }
    fn from_function_node_f64(value: f64) -> Self {
        value != 0.0
    }
}

/// One named value of a record or of a node's outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionNodeNamedValue {
    /// The field or port name.
    pub name: &'static str,
    /// What it carries.
    pub value_type: FunctionNodeValueType,
}

/// A struct of values a function node takes as its state or its config, or returns as its outputs.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a function node's state or config record",
    label = "derive `FunctionNodeRecord` on a struct of `f64`, `i64` and `bool` fields; one with an \
             `Option` or `UnreadMessage` field is a record of outputs only"
)]
pub trait FunctionNodeRecord: Default + Copy {
    /// Its fields, in the order the evaluate entry carries their values.
    const FIELDS: &'static [FunctionNodeNamedValue];
    /// Write each field's f64 into `values`, in [`Self::FIELDS`] order.
    fn write_function_node_values(&self, values: &mut [f64]);
    /// The record `values` carries, in [`Self::FIELDS`] order.
    fn from_function_node_values(values: &[f64]) -> Self;
}

/// A message of any family a function node was handed, which its body can only hand on unread.
///
/// ABI contract with the read seam: it travels through the evaluate entry as
/// the index of the input holding it plus one, and `0.0` stands for none.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UnreadMessage {
    token_naming_the_input_holding_it: f64,
}

/// A value an input pin takes: a number, a bool, or a message it hands on unread.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a function node input pin",
    label = "an input pin is `f64`, `i64`, `bool`, `UnreadMessage` or `Option<UnreadMessage>`"
)]
pub trait FunctionNodeInputPinValue: Sized {
    /// Its value type.
    const VALUE_TYPE: FunctionNodeValueType;
    /// Whether the pin takes `None` when it holds nothing, rather than leaving its node uncalled.
    const TAKES_NONE: bool;
    /// The value the f64 the evaluate entry is handed for this pin stands for.
    fn from_function_node_input_f64(value: f64) -> Self;
}

macro_rules! a_value_is_an_input_pin {
    ($($value:ty),*) => {$(
        impl FunctionNodeInputPinValue for $value {
            const VALUE_TYPE: FunctionNodeValueType = <$value as FunctionNodeValue>::VALUE_TYPE;
            const TAKES_NONE: bool = false;
            fn from_function_node_input_f64(value: f64) -> Self {
                <$value as FunctionNodeValue>::from_function_node_f64(value)
            }
        }
    )*};
}

a_value_is_an_input_pin!(f64, i64, bool);

impl FunctionNodeInputPinValue for UnreadMessage {
    const VALUE_TYPE: FunctionNodeValueType = FunctionNodeValueType::UnreadMessage;
    const TAKES_NONE: bool = false;
    fn from_function_node_input_f64(value: f64) -> Self {
        Self {
            token_naming_the_input_holding_it: value,
        }
    }
}

impl FunctionNodeInputPinValue for Option<UnreadMessage> {
    const VALUE_TYPE: FunctionNodeValueType = FunctionNodeValueType::UnreadMessage;
    const TAKES_NONE: bool = true;
    fn from_function_node_input_f64(value: f64) -> Self {
        (value != 0.0).then_some(UnreadMessage {
            token_naming_the_input_holding_it: value,
        })
    }
}

/// A value an output port hands out: a number, a bool or a message handed on, or an `Option` of one that may hand out nothing.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a function node output",
    label = "an output is `f64`, `i64`, `bool`, `UnreadMessage`, or an `Option` of one"
)]
pub trait FunctionNodeOutputPortValue: Copy {
    /// Its value type.
    const VALUE_TYPE: FunctionNodeValueType;
    /// Whether it may hand out nothing, which gives its port a handed-out flag slot.
    const MAY_HAND_OUT_NOTHING: bool;
    /// The f64 its output slot carries, or `None` when it hands out nothing.
    fn to_function_node_output_f64(self) -> Option<f64>;
}

macro_rules! a_value_and_its_option_are_outputs {
    ($($value:ty),*) => {$(
        impl FunctionNodeOutputPortValue for $value {
            const VALUE_TYPE: FunctionNodeValueType = <$value as FunctionNodeValue>::VALUE_TYPE;
            const MAY_HAND_OUT_NOTHING: bool = false;
            fn to_function_node_output_f64(self) -> Option<f64> {
                Some(self.to_function_node_f64())
            }
        }

        impl FunctionNodeOutputPortValue for Option<$value> {
            const VALUE_TYPE: FunctionNodeValueType = <$value as FunctionNodeValue>::VALUE_TYPE;
            const MAY_HAND_OUT_NOTHING: bool = true;
            fn to_function_node_output_f64(self) -> Option<f64> {
                self.map(FunctionNodeValue::to_function_node_f64)
            }
        }
    )*};
}

a_value_and_its_option_are_outputs!(f64, i64, bool);

impl FunctionNodeOutputPortValue for UnreadMessage {
    const VALUE_TYPE: FunctionNodeValueType = FunctionNodeValueType::UnreadMessage;
    const MAY_HAND_OUT_NOTHING: bool = false;
    fn to_function_node_output_f64(self) -> Option<f64> {
        Some(self.token_naming_the_input_holding_it)
    }
}

impl FunctionNodeOutputPortValue for Option<UnreadMessage> {
    const VALUE_TYPE: FunctionNodeValueType = FunctionNodeValueType::UnreadMessage;
    const MAY_HAND_OUT_NOTHING: bool = true;
    fn to_function_node_output_f64(self) -> Option<f64> {
        self.map(|unread_message| unread_message.token_naming_the_input_holding_it)
    }
}

/// One output port of a function node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionNodeOutputPort {
    /// The port name: `result`, or its record field's.
    pub name: &'static str,
    /// What it carries.
    pub value_type: FunctionNodeValueType,
    /// Whether it may hand out nothing, which gives it a handed-out flag slot past every output.
    pub may_hand_out_nothing: bool,
}

impl FunctionNodeOutputPort {
    /// The port `name` that hands out `OutputValue`.
    pub const fn handing_out<OutputValue: FunctionNodeOutputPortValue>(name: &'static str) -> Self {
        Self {
            name,
            value_type: OutputValue::VALUE_TYPE,
            may_hand_out_nothing: OutputValue::MAY_HAND_OUT_NOTHING,
        }
    }
}

/// What a function node returns as its outputs: one value as `result`, or a record's fields.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a function node's output",
    label = "return `f64`, `i64`, `bool`, `UnreadMessage`, an `Option` of one, or a record deriving \
             `FunctionNodeRecord`; a record of outputs is never `None` as a whole, so type each \
             field that may hand out nothing `Option<T>`"
)]
pub trait FunctionNodeOutput {
    /// Its output ports, in the order the evaluate entry writes their values.
    const OUTPUT_PORTS: &'static [FunctionNodeOutputPort];
    /// Write each output's slot, and the handed-out flag slot of each output that may hand out nothing.
    fn write_function_node_output_values(&self, output_values: &mut [f64]);
}

macro_rules! one_value_is_the_result_output {
    ($($value:ty),*) => {$(
        impl FunctionNodeOutput for $value {
            const OUTPUT_PORTS: &'static [FunctionNodeOutputPort] =
                &[FunctionNodeOutputPort::handing_out::<$value>("result")];
            fn write_function_node_output_values(&self, output_values: &mut [f64]) {
                write_one_function_node_output_value(Self::OUTPUT_PORTS, 0, *self, output_values);
            }
        }
    )*};
}

one_value_is_the_result_output!(
    f64,
    i64,
    bool,
    UnreadMessage,
    Option<f64>,
    Option<i64>,
    Option<bool>,
    Option<UnreadMessage>
);

/// The slots an evaluate entry writes for `output_ports`: one per output, then
/// one handed-out flag per output that may hand out nothing, in output order.
///
/// ABI contract with the read seam: a flag holds `1.0` when its output handed
/// one out and `0.0` when it did not.
pub const fn function_node_output_slot_count(output_ports: &[FunctionNodeOutputPort]) -> usize {
    let mut slot_count = output_ports.len();
    let mut output_index = 0;
    while output_index < output_ports.len() {
        if output_ports[output_index].may_hand_out_nothing {
            slot_count += 1;
        }
        output_index += 1;
    }
    slot_count
}

/// The slot of output `output_index`'s handed-out flag; none when it always hands one out.
pub fn function_node_handed_out_flag_slot(
    output_ports: &[FunctionNodeOutputPort],
    output_index: usize,
) -> Option<usize> {
    if !output_ports.get(output_index)?.may_hand_out_nothing {
        return None;
    }
    let flags_before_it = output_ports[..output_index]
        .iter()
        .filter(|output_port| output_port.may_hand_out_nothing)
        .count();
    Some(output_ports.len() + flags_before_it)
}

/// Write output `output_index`'s `value` into its slot, and its handed-out flag when it has one.
pub fn write_one_function_node_output_value<OutputValue: FunctionNodeOutputPortValue>(
    output_ports: &[FunctionNodeOutputPort],
    output_index: usize,
    value: OutputValue,
    output_values: &mut [f64],
) {
    let handed_out = value.to_function_node_output_f64();
    output_values[output_index] = handed_out.unwrap_or(0.0);
    if let Some(handed_out_flag_slot) =
        function_node_handed_out_flag_slot(output_ports, output_index)
    {
        output_values[handed_out_flag_slot] = if handed_out.is_some() { 1.0 } else { 0.0 };
    }
}

/// Refuse at compile time an `UnreadMessage` output with no one `UnreadMessage` input to follow.
///
/// Such an output takes the family linked into the input it hands on, so a
/// function handing one out takes exactly one.
pub const fn refuse_an_unread_message_output_following_no_one_input(
    input_value_types: &[FunctionNodeValueType],
    output_ports: &[FunctionNodeOutputPort],
) {
    let mut unread_message_inputs = 0;
    let mut input_index = 0;
    while input_index < input_value_types.len() {
        if matches!(
            input_value_types[input_index],
            FunctionNodeValueType::UnreadMessage
        ) {
            unread_message_inputs += 1;
        }
        input_index += 1;
    }
    let mut output_index = 0;
    while output_index < output_ports.len() {
        if matches!(
            output_ports[output_index].value_type,
            FunctionNodeValueType::UnreadMessage
        ) && unread_message_inputs != 1
        {
            panic!(
                "a function node's `UnreadMessage` output takes the family of the one \
                 `UnreadMessage` input it hands on; take exactly one `UnreadMessage` input"
            );
        }
        output_index += 1;
    }
}

/// An input pin of a function node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FunctionNodeInputPin {
    /// The port name, the parameter's own.
    pub port: &'static str,
    /// What it carries.
    pub value_type: FunctionNodeValueType,
    /// Its value when nothing is wired to it; `None` when it must be wired.
    pub pin_default: Option<f64>,
    /// Whether it takes `None` when it holds nothing, rather than leaving its node uncalled.
    pub takes_none: bool,
}

/// A field of a function node's config or state record, with the value its record's `Default` gives it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FunctionNodeRecordFieldWithItsDefault {
    /// The field.
    pub field: FunctionNodeNamedValue,
    /// Its value in the record's `Default`, as the evaluate entry carries it.
    pub default_value: f64,
}

/// Each field of `Record` with the value its `Default` gives it.
pub fn function_node_record_fields_with_their_defaults<Record: FunctionNodeRecord>()
-> Vec<FunctionNodeRecordFieldWithItsDefault> {
    let mut default_values = vec![0.0; Record::FIELDS.len()];
    Record::default().write_function_node_values(&mut default_values);
    Record::FIELDS
        .iter()
        .zip(default_values)
        .map(
            |(field, default_value)| FunctionNodeRecordFieldWithItsDefault {
                field: *field,
                default_value,
            },
        )
        .collect()
}

/// What `#[function_node]` declares: the node its module's Python half hands `node.rust_function(...)`.
#[derive(Debug, Clone)]
pub struct FunctionNodeDeclaration {
    /// The node type's short name, its function's name in UpperCamelCase.
    pub short_name: &'static str,
    /// Its function's doc comment.
    pub description: &'static str,
    /// Its input pins, in the order the evaluate entry reads their values.
    pub input_pins: Vec<FunctionNodeInputPin>,
    /// Its output ports, in the order the evaluate entry writes their values.
    pub output_ports: Vec<FunctionNodeOutputPort>,
    /// Its config record's fields; none when it takes no `config`.
    pub config_fields: Vec<FunctionNodeRecordFieldWithItsDefault>,
    /// Its state record's fields, each defaulted to its first call's value; none when stateless.
    pub state_fields: Vec<FunctionNodeRecordFieldWithItsDefault>,
    /// The parameter its function takes the reader's stream time as; none when it takes none.
    pub stream_time_parameter: Option<&'static str>,
    /// The parameter its function takes its state as; none when stateless.
    pub state_parameter: Option<&'static str>,
    /// The parameter its function takes its config as; none when it takes no `config`.
    pub config_parameter: Option<&'static str>,
    /// The entry the read seam calls.
    pub evaluate_entry: ModuleHostTableFunctionNodeEvaluateEntry,
}

/// Run one call of a function node's body, refusing the call when it panics so
/// no unwind crosses the evaluate entry's C boundary.
pub fn evaluate_one_function_node_call(call: impl FnOnce()) -> ModuleHostTableStatus {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(()) => MODULE_HOST_TABLE_STATUS_OK,
        Err(_) => MODULE_HOST_TABLE_STATUS_REFUSED,
    }
}

/// The `value_count` values an evaluate entry was handed at `values`; empty when it carries none.
///
/// # Safety
///
/// When `value_count` is not zero, `values` points to `value_count` f64s that
/// stay valid and unwritten for `'evaluate_call`.
pub unsafe fn function_node_values_handed_to_the_evaluate_entry<'evaluate_call>(
    values: *const f64,
    value_count: usize,
) -> &'evaluate_call [f64] {
    if value_count == 0 {
        return &[];
    }
    // SAFETY: the caller's contract.
    unsafe { std::slice::from_raw_parts(values, value_count) }
}

/// The `value_count` values an evaluate entry writes at `values`; empty when it writes none.
///
/// # Safety
///
/// When `value_count` is not zero, `values` points to `value_count` f64s that
/// stay valid and are reached through nothing else for `'evaluate_call`.
pub unsafe fn function_node_values_the_evaluate_entry_writes<'evaluate_call>(
    values: *mut f64,
    value_count: usize,
) -> &'evaluate_call mut [f64] {
    if value_count == 0 {
        return &mut [];
    }
    // SAFETY: the caller's contract.
    unsafe { std::slice::from_raw_parts_mut(values, value_count) }
}
