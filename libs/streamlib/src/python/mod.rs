//! streamlib-python: Python bindings for streamlib
//!
//! This crate provides Python bindings for the streamlib real-time streaming infrastructure.
//! It enables both:
//! 1. Users to build streaming pipelines in Python using decorators
//! 2. AI agents to submit dynamic Python processor code to a running runtime
//!
//! ## Architecture
//!
//! - **Pre-built processors**: Decorators like `@camera_processor` bind to Rust implementations
//! - **Dynamic processors**: `@processor` decorator wraps Python functions
//! - **Zero-copy GPU**: Python sees opaque handles, GPU memory stays on GPU
//!
//! ## Example
//!
//! ```python
//! from streamlib import camera_processor, display_processor, processor, StreamRuntime
//!
//! @camera_processor(device_id=0)
//! def camera():
//!     pass  # Uses Rust CameraProcessor
//!
//! @display_processor(title="Feed")
//! def display():
//!     pass  # Uses Rust DisplayProcessor
//!
//! @processor(inputs=["video"], outputs=["video"])
//! def my_filter(frame):
//!     # Dynamic Python code
//!     return frame
//!
//! runtime = StreamRuntime(fps=30)
//! runtime.add_processor(camera)
//! runtime.add_processor(display)
//! runtime.connect(camera.outputs['video'], display.inputs['video'])
//! runtime.run()
//! ```

mod runtime;
mod types;
mod decorators;
mod error;
mod port;

use pyo3::prelude::*;

pub use error::{PyStreamError, Result};
pub use runtime::{PyStreamRuntime, PyStream, TestPort};
pub use port::ProcessorPort;
pub use types::PyVideoFrame;
pub use decorators::{camera_processor, display_processor, processor, ProcessorProxy, PortsProxy};

/// Register the streamlib Python module
///
/// This function is called by the main lib.rs when the python feature is enabled
/// to register all Python bindings.
pub fn register_python_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Register types
    m.add_class::<types::PyVideoFrame>()?;
    m.add_class::<runtime::PyStreamRuntime>()?;
    m.add_class::<runtime::PyStream>()?;
    m.add_class::<port::ProcessorPort>()?;
    m.add_class::<runtime::TestPort>()?;  // Test struct
    m.add_class::<decorators::ProcessorProxy>()?;
    m.add_class::<decorators::PortsProxy>()?;

    // Register decorators
    m.add_function(wrap_pyfunction!(decorators::camera_processor, m)?)?;
    m.add_function(wrap_pyfunction!(decorators::display_processor, m)?)?;
    m.add_function(wrap_pyfunction!(decorators::processor, m)?)?;

    // Module metadata
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("__doc__", "Real-time streaming infrastructure for AI agents")?;

    Ok(())
}
