//! Runtime link infrastructure for actual data flow.
//!
//! - `LinkInstance`: Owns the ring buffer
//! - `LinkOutputDataWriter`/`LinkInputDataReader`: Weak references for graceful degradation
//! - `LinkOutput`/`LinkInput`: Processor-facing port API

pub mod link_input;
pub mod link_input_data_reader;
pub mod link_instance;
pub mod link_output;
pub mod link_output_data_writer;
pub mod link_output_to_processor_message;

pub use link_input::LinkInput;
pub use link_input_data_reader::LinkInputDataReader;
pub use link_instance::{AnyLinkInstance, BoxedLinkInstance, LinkInstance, LinkInstanceInner};
pub use link_output::LinkOutput;
pub use link_output_data_writer::LinkOutputDataWriter;
pub use link_output_to_processor_message::LinkOutputToProcessorMessage;
