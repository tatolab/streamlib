//! Python bindings for StreamRuntime

use pyo3::prelude::*;
use super::error::PyStreamError;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;

/// Python wrapper for a processor port
#[pyclass(name = "Port")]
#[derive(Clone)]
pub struct PyPort {
    /// Processor name
    pub(crate) processor_name: String,
    /// Port name
    pub(crate) port_name: String,
    /// Port direction (input or output)
    pub(crate) is_input: bool,
}

#[pymethods]
impl PyPort {
    /// String representation
    fn __repr__(&self) -> String {
        let direction = if self.is_input { "input" } else { "output" };
        format!("Port({}.{}, {})", self.processor_name, self.port_name, direction)
    }
}

/// Python wrapper for a Stream (processor instance)
#[pyclass(name = "Stream")]
pub struct PyStream {
    /// Processor name/identifier
    pub(crate) name: String,
    /// Input ports
    #[pyo3(get)]
    pub inputs: PyObject,
    /// Output ports
    #[pyo3(get)]
    pub outputs: PyObject,
    /// Whether this is a pre-built processor
    pub(crate) is_prebuilt: bool,
    /// Processor type (for pre-built)
    pub(crate) processor_type: Option<String>,
}

#[pymethods]
impl PyStream {
    /// String representation
    fn __repr__(&self) -> String {
        format!("Stream(name={})", self.name)
    }
}

/// Python wrapper for StreamRuntime
///
/// The runtime manages the streaming pipeline, processor lifecycle, and execution.
#[pyclass(name = "StreamRuntime")]
pub struct PyStreamRuntime {
    /// Frame rate
    fps: u32,
    /// Frame width
    width: u32,
    /// Frame height
    height: u32,
    /// Enable GPU acceleration
    enable_gpu: bool,
    /// Registered streams (processors)
    streams: Arc<Mutex<HashMap<String, PyObject>>>,
    /// Connections (source_port -> destination_port)
    connections: Arc<Mutex<Vec<(PyPort, PyPort)>>>,
}

#[pymethods]
impl PyStreamRuntime {
    /// Create a new StreamRuntime
    ///
    /// # Arguments
    /// * `fps` - Target frame rate (default: 30)
    /// * `width` - Frame width (default: 1920)
    /// * `height` - Frame height (default: 1080)
    /// * `enable_gpu` - Enable GPU acceleration (default: true)
    #[new]
    #[pyo3(signature = (fps=30, width=1920, height=1080, enable_gpu=true))]
    fn new(fps: u32, width: u32, height: u32, enable_gpu: bool) -> Self {
        Self {
            fps,
            width,
            height,
            enable_gpu,
            streams: Arc::new(Mutex::new(HashMap::new())),
            connections: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Add a stream (processor) to the runtime
    ///
    /// # Arguments
    /// * `stream` - The stream to add (can be a decorated function or PyStream)
    fn add_stream(&self, py: Python<'_>, stream: PyObject) -> PyResult<()> {
        // Extract the function name as the stream identifier
        let name: String = stream.getattr(py, "__name__")?.extract(py)?;

        let mut streams = self.streams.lock().unwrap();
        streams.insert(name, stream);

        Ok(())
    }

    /// Connect two ports
    ///
    /// # Arguments
    /// * `source` - Source port (output)
    /// * `destination` - Destination port (input)
    fn connect(&self, source: PyPort, destination: PyPort) -> PyResult<()> {
        if source.is_input {
            return Err(PyStreamError::Connection(
                "Source port must be an output port".to_string()
            ).into());
        }
        if !destination.is_input {
            return Err(PyStreamError::Connection(
                "Destination port must be an input port".to_string()
            ).into());
        }

        let mut connections = self.connections.lock().unwrap();
        connections.push((source, destination));

        Ok(())
    }

    /// Run the streaming pipeline
    ///
    /// This starts the pipeline and blocks until interrupted (Ctrl+C).
    fn run(&self, py: Python<'_>) -> PyResult<()> {
        // TODO: Implement actual runtime execution
        // This will:
        // 1. Initialize GPU context
        // 2. Instantiate all processors
        // 3. Set up connections
        // 4. Start the frame loop
        // 5. Run until Ctrl+C

        py.allow_threads(|| {
            println!("🎥 StreamRuntime starting...");
            println!("   FPS: {}", self.fps);
            println!("   Resolution: {}x{}", self.width, self.height);
            println!("   GPU: {}", if self.enable_gpu { "enabled" } else { "disabled" });

            let streams = self.streams.lock().unwrap();
            println!("   Streams: {}", streams.len());

            let connections = self.connections.lock().unwrap();
            println!("   Connections: {}", connections.len());

            println!("\n⚠️  Runtime execution not yet implemented");
            println!("   This is a placeholder - actual execution coming soon!");

            Ok(())
        })
    }

    /// String representation
    fn __repr__(&self) -> String {
        format!(
            "StreamRuntime(fps={}, resolution={}x{}, gpu={})",
            self.fps, self.width, self.height, self.enable_gpu
        )
    }
}
