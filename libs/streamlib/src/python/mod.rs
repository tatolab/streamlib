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
//! from streamlib import (
//!     camera_processor, display_processor, processor,
//!     StreamRuntime, StreamInput, StreamOutput, VideoFrame
//! )
//!
//! @camera_processor(device_id=0)
//! def camera():
//!     pass  # Uses Rust CameraProcessor
//!
//! @display_processor(title="Feed")
//! def display():
//!     pass  # Uses Rust DisplayProcessor
//!
//! @processor(
//!     description="Custom video processor",
//!     tags=["filter"]
//! )
//! class MyFilter:
//!     class InputPorts:
//!         video = StreamInput(VideoFrame)
//!
//!     class OutputPorts:
//!         video = StreamOutput(VideoFrame)
//!
//!     def process(self, tick):
//!         frame = self.input_ports().video.read_latest()
//!         # Process frame...
//!         self.output_ports().video.write(frame)
//!
//! runtime = StreamRuntime(fps=30)
//! runtime.add_stream(camera)
//! runtime.add_stream(MyFilter)
//! runtime.add_stream(display)
//! runtime.connect(camera.output_ports().video, MyFilter.input_ports().video)
//! runtime.connect(MyFilter.output_ports().video, display.input_ports().video)
//! runtime.run()
//! ```

mod runtime;
mod types;
mod types_ext;
mod decorators;
mod error;
mod port;
mod processor;
mod gpu_wrappers;

use pyo3::prelude::*;

pub use error::{PyStreamError, Result};
pub use runtime::{PyStreamRuntime, PyStream, TestPort};
pub use port::ProcessorPort;
pub use types::PyVideoFrame;
pub use decorators::{camera_processor, display_processor, processor as processor_decorator, ProcessorProxy, PortsProxy};
pub use processor::PythonProcessor;

/// Register the streamlib Python module
///
/// This function is called by the main lib.rs when the python feature is enabled
/// to register all Python bindings.
pub fn register_python_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Register core types
    m.add_class::<types::PyVideoFrame>()?;
    m.add_class::<runtime::PyStreamRuntime>()?;
    m.add_class::<runtime::PyStream>()?;
    m.add_class::<port::ProcessorPort>()?;
    m.add_class::<runtime::TestPort>()?;  // Test struct
    m.add_class::<decorators::ProcessorProxy>()?;
    m.add_class::<decorators::PortsProxy>()?;

    // Register new port and context types
    m.add_class::<types_ext::PyStreamInput>()?;
    m.add_class::<types_ext::PyStreamOutput>()?;
    m.add_class::<types_ext::PyTimedTick>()?;
    m.add_class::<types_ext::PyGpuContext>()?;
    m.add_class::<types_ext::PyInputPorts>()?;
    m.add_class::<types_ext::PyOutputPorts>()?;

    // Register GPU wrapper classes
    m.add_class::<gpu_wrappers::PyWgpuDevice>()?;
    m.add_class::<gpu_wrappers::PyWgpuQueue>()?;
    m.add_class::<gpu_wrappers::PyWgpuShaderModule>()?;
    m.add_class::<gpu_wrappers::PyWgpuBuffer>()?;
    m.add_class::<gpu_wrappers::PyWgpuBindGroupLayout>()?;
    m.add_class::<gpu_wrappers::PyWgpuPipelineLayout>()?;
    m.add_class::<gpu_wrappers::PyWgpuComputePipeline>()?;
    m.add_class::<gpu_wrappers::PyWgpuBindGroup>()?;
    m.add_class::<gpu_wrappers::PyWgpuCommandEncoder>()?;
    m.add_class::<gpu_wrappers::PyWgpuComputePass>()?;
    m.add_class::<gpu_wrappers::PyWgpuTexture>()?;
    m.add_class::<gpu_wrappers::PyWgpuTextureView>()?;

    // Register wgpu enum classes (replaces wgpu-py dependency)
    m.add_class::<gpu_wrappers::PyBufferUsage>()?;
    m.add_class::<gpu_wrappers::PyShaderStage>()?;
    m.add_class::<gpu_wrappers::PyTextureSampleType>()?;
    m.add_class::<gpu_wrappers::PyTextureViewDimension>()?;
    m.add_class::<gpu_wrappers::PyStorageTextureAccess>()?;
    m.add_class::<gpu_wrappers::PyTextureFormat>()?;
    m.add_class::<gpu_wrappers::PyBufferBindingType>()?;

    // Register decorators
    m.add_function(wrap_pyfunction!(decorators::camera_processor, m)?)?;
    m.add_function(wrap_pyfunction!(decorators::display_processor, m)?)?;
    m.add_function(wrap_pyfunction!(decorators::processor, m)?)?;

    // Module metadata
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("__doc__", "Real-time streaming infrastructure for AI agents")?;

    // Add marker classes for syntax sugar in @processor decorator
    // These are simple Python classes that allow syntax like: video = StreamInput(VideoFrame)
    // The actual ports are created in Rust and injected at runtime
    let py = m.py();

    // Test: add a simple string first
    m.add("TEST_MARKER", "test_value")?;

    // Define StreamInput using type()
    let type_fn = py.eval_bound("type", None, None)?;

    let stream_input_dict = pyo3::types::PyDict::new_bound(py);
    stream_input_dict.set_item("__init__", py.eval_bound(
        "lambda self, type_hint=None: None",
        None,
        None
    )?)?;
    stream_input_dict.set_item("__repr__", py.eval_bound(
        "lambda self: 'StreamInput(VideoFrame)'",
        None,
        None
    )?)?;

    let stream_input = type_fn.call1((
        "StreamInput",
        py.eval_bound("(object,)", None, None)?,
        stream_input_dict,
    ))?;

    m.add("StreamInput", stream_input)?;

    // Define StreamOutput the same way
    let stream_output_dict = pyo3::types::PyDict::new_bound(py);
    stream_output_dict.set_item("__init__", py.eval_bound(
        "lambda self, type_hint=None: None",
        None,
        None
    )?)?;
    stream_output_dict.set_item("__repr__", py.eval_bound(
        "lambda self: 'StreamOutput(VideoFrame)'",
        None,
        None
    )?)?;

    let stream_output = type_fn.call1((
        "StreamOutput",
        py.eval_bound("(object,)", None, None)?,
        stream_output_dict,
    ))?;

    m.add("StreamOutput", stream_output)?;

    Ok(())
}
