// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Runtime link infrastructure for actual data flow.
//!
//! - `LinkInstance`: Owns the ring buffer
//! - `LinkOutputDataWriter`/`LinkInputDataReader`: Weak references for graceful degradation
//! - `LinkOutput`/`LinkInput`: Processor-facing port API

mod link_input;
mod link_input_data_reader;
mod link_instance;
mod link_output;
mod link_output_data_writer;
mod link_output_to_processor_message;

pub use link_input::LinkInput;
pub use link_input_data_reader::LinkInputDataReader;
pub use link_instance::{AnyLinkInstance, BoxedLinkInstance, LinkInstance, LinkInstanceInner};
pub use link_output::LinkOutput;
pub use link_output_data_writer::LinkOutputDataWriter;
pub use link_output_to_processor_message::LinkOutputToProcessorMessage;
