//! Graph compilation pipeline.
//!
//! Converts graph topology changes into running processor instances.
//! The compilation process has 4 phases:
//! 1. CREATE - Instantiate processor instances from factory
//! 2. WIRE - Create ring buffers and connect ports
//! 3. SETUP - Call __generated_setup on each processor
//! 4. START - Spawn threads based on execution config

mod compiler;
pub(crate) mod delta;
mod phase;
pub(crate) mod phases;
pub mod wiring;

pub use self::compiler::Compiler;
pub use delta::{
    compute_delta, compute_delta_with_config, GraphDelta, LinkConfigChange, ProcessorConfigChange,
};
pub use phase::{CompilePhase, CompileResult};
pub use phases::{shutdown_all_processors, shutdown_processor};
pub use wiring::{LinkInputDataReaderWrapper, LinkOutputDataWriterWrapper};
