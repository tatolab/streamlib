//! Python bindings for StreamRuntime

use pyo3::prelude::*;
use super::error::PyStreamError;
use super::port::ProcessorPort;
use super::decorators::ProcessorProxy;
use std::sync::{Arc, Mutex};
use crate::StreamRuntime;

#[cfg(any(target_os = "macos", target_os = "ios"))]
use crate::apple::{AppleCameraProcessor, AppleDisplayProcessor};

// Test struct to debug PyO3 issue
#[pyclass(module = "streamlib")]
pub struct TestPort {
    #[pyo3(get)]
    pub name: String,
}

#[pymethods]
impl TestPort {
    #[new]
    fn new(name: String) -> Self {
        Self { name }
    }

    fn test(&self) -> String {
        format!("Test: {}", self.name)
    }
}

/// Python wrapper for a Stream (processor instance)
#[pyclass(name = "Stream", module = "streamlib")]
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

// No longer need StreamMetadata - we use ProcessorProxy directly

/// Python wrapper for StreamRuntime
///
/// The runtime manages the streaming pipeline, processor lifecycle, and execution.
#[pyclass(name = "StreamRuntime", module = "streamlib")]
pub struct PyStreamRuntime {
    /// The actual Rust runtime (wrapped in Option so we can move it)
    runtime: Option<StreamRuntime>,
    /// Frame rate
    fps: u32,
    /// Frame width
    width: u32,
    /// Frame height
    height: u32,
    /// Enable GPU acceleration
    enable_gpu: bool,
    /// Registered processor proxies (before instantiation)
    processors: Arc<Mutex<Vec<Py<ProcessorProxy>>>>,
    /// Connections (source_port -> destination_port)
    connections: Arc<Mutex<Vec<(ProcessorPort, ProcessorPort)>>>,
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
        // Create the actual Rust runtime
        let runtime = StreamRuntime::new(fps as f64);

        Self {
            runtime: Some(runtime),
            fps,
            width,
            height,
            enable_gpu,
            processors: Arc::new(Mutex::new(Vec::new())),
            connections: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Add a stream (processor) to the runtime
    ///
    /// # Arguments
    /// * `processor` - ProcessorProxy returned by a decorator
    fn add_stream(&self, py: Python<'_>, processor: Py<ProcessorProxy>) -> PyResult<()> {
        let name = processor.borrow(py).processor_name.clone();

        let mut processors = self.processors.lock().unwrap();
        processors.push(processor);

        tracing::info!("Added processor: {}", name);
        Ok(())
    }

    /// Connect two ports
    ///
    /// # Arguments
    /// * `source` - Source port (output)
    /// * `destination` - Destination port (input)
    fn connect(&self, source: ProcessorPort, destination: ProcessorPort) -> PyResult<()> {
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
    fn run(&mut self, py: Python<'_>) -> PyResult<()> {
        println!("🎥 StreamRuntime starting...");
        println!("   FPS: {}", self.fps);
        println!("   Resolution: {}x{}", self.width, self.height);
        println!("   GPU: {}", if self.enable_gpu { "enabled" } else { "disabled" });

        let processors = self.processors.lock().unwrap();
        println!("   Processors: {}", processors.len());

        let connections = self.connections.lock().unwrap();
        println!("   Connections: {}", connections.len());

        // Instantiate the Rust runtime
        let mut runtime = self.runtime.take().ok_or_else(|| {
            PyStreamError::Runtime("Runtime already started".to_string())
        })?;

        // Instantiate processors based on ProcessorProxy metadata
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            use std::collections::HashMap;
            use crate::core::processors::{
                CameraProcessor as CameraProcessorTrait,
                DisplayProcessor as DisplayProcessorTrait,
            };

            // Enum to hold different processor types temporarily
            enum ProcessorInstance {
                Camera(AppleCameraProcessor),
                Display(AppleDisplayProcessor),
                Python(super::processor::PythonProcessor),
            }

            // Step 1: Instantiate all processors (don't box yet)
            let mut processor_instances: HashMap<String, ProcessorInstance> = HashMap::new();

            for proxy in processors.iter() {
                let proxy_ref = proxy.borrow(py);
                let processor_type = &proxy_ref.processor_type;
                let processor_name = &proxy_ref.processor_name;
                let config = &proxy_ref.config;

                match processor_type.as_str() {
                    "CameraProcessor" => {
                        println!("   Creating CameraProcessor: {}", processor_name);

                        // Extract device_id from config if present
                        let device_id = config.as_ref()
                            .and_then(|c| c.bind(py).get_item("device_id").ok().flatten())
                            .and_then(|v| v.extract::<String>().ok());

                        let processor = if let Some(id) = device_id {
                            AppleCameraProcessor::with_device_id(&id)
                        } else {
                            AppleCameraProcessor::new()
                        }.map_err(|e| PyStreamError::Runtime(format!("Failed to create camera: {}", e)))?;

                        processor_instances.insert(processor_name.clone(), ProcessorInstance::Camera(processor));
                    }
                    "DisplayProcessor" => {
                        println!("   Creating DisplayProcessor: {}", processor_name);

                        let mut processor = AppleDisplayProcessor::with_size(self.width, self.height)
                            .map_err(|e| PyStreamError::Runtime(format!("Failed to create display: {}", e)))?;

                        // Extract title from config if present
                        if let Some(title) = config.as_ref()
                            .and_then(|c| c.bind(py).get_item("title").ok().flatten())
                            .and_then(|v| v.extract::<String>().ok()) {
                            processor.set_window_title(&title);
                        }

                        processor_instances.insert(processor_name.clone(), ProcessorInstance::Display(processor));
                    }
                    "PythonProcessor" => {
                        println!("   Creating PythonProcessor: {}", processor_name);

                        // Extract Python class from ProcessorProxy
                        let python_class = proxy_ref.python_class.as_ref()
                            .ok_or_else(|| PyStreamError::Runtime("PythonProcessor missing python_class".to_string()))?
                            .clone_ref(py);

                        // Create PythonProcessor with class, port metadata, and schema
                        let processor = super::processor::PythonProcessor::new(
                            python_class,
                            processor_name.clone(),
                            proxy_ref.input_port_names.clone(),
                            proxy_ref.output_port_names.clone(),
                            proxy_ref.description.clone(),
                            proxy_ref.usage_context.clone(),
                            proxy_ref.tags.clone(),
                        ).map_err(|e| PyStreamError::Runtime(format!("Failed to create Python processor: {}", e)))?;

                        processor_instances.insert(processor_name.clone(), ProcessorInstance::Python(processor));
                    }
                    _ => {
                        return Err(PyStreamError::Runtime(
                            format!("Unknown processor type: {}", processor_type)
                        ).into());
                    }
                }
            }

            // Step 2: Wire connections between processors
            println!("   Wiring {} connections...", connections.len());
            for (source_port, dest_port) in connections.iter() {
                println!("      {} → {}",
                    format!("{}.{}", source_port.processor_name, source_port.port_name),
                    format!("{}.{}", dest_port.processor_name, dest_port.port_name)
                );

                // To avoid double borrow, remove source processor temporarily
                let mut source_proc = processor_instances.remove(&source_port.processor_name)
                    .ok_or_else(|| PyStreamError::Connection(
                        format!("Source processor '{}' not found", source_port.processor_name)
                    ))?;

                // Get mutable reference to destination processor
                let dest_proc = processor_instances.get_mut(&dest_port.processor_name)
                    .ok_or_else(|| PyStreamError::Connection(
                        format!("Destination processor '{}' not found", dest_port.processor_name)
                    ))?;

                // Wire the connection based on processor types and port names
                match (&mut source_proc, dest_proc, source_port.port_name.as_str(), dest_port.port_name.as_str()) {
                    // Camera → Display
                    (ProcessorInstance::Camera(camera), ProcessorInstance::Display(display), "video", "video") => {
                        runtime.connect(
                            &mut camera.output_ports().video,
                            &mut display.input_ports().video,
                        ).map_err(|e| PyStreamError::Connection(format!("Failed to connect: {}", e)))?;
                    }
                    // Camera → Python
                    (ProcessorInstance::Camera(camera), ProcessorInstance::Python(python), "video", port_name) => {
                        let python_input_arc = python.input_ports().ports.get(port_name)
                            .ok_or_else(|| PyStreamError::Connection(
                                format!("Python processor has no input port '{}'", port_name)
                            ))?;
                        let mut python_input_guard = python_input_arc.lock().unwrap();
                        runtime.connect(
                            &mut camera.output_ports().video,
                            &mut *python_input_guard,
                        ).map_err(|e| PyStreamError::Connection(format!("Failed to connect: {}", e)))?;
                    }
                    // Python → Display
                    (ProcessorInstance::Python(python), ProcessorInstance::Display(display), port_name, "video") => {
                        let python_output_arc = python.output_ports().ports.get(port_name)
                            .ok_or_else(|| PyStreamError::Connection(
                                format!("Python processor has no output port '{}'", port_name)
                            ))?;
                        let mut python_output_guard = python_output_arc.lock().unwrap();
                        runtime.connect(
                            &mut *python_output_guard,
                            &mut display.input_ports().video,
                        ).map_err(|e| PyStreamError::Connection(format!("Failed to connect: {}", e)))?;
                    }
                    // Python → Python
                    (ProcessorInstance::Python(source_py), ProcessorInstance::Python(dest_py), source_port_name, dest_port_name) => {
                        let source_output_arc = source_py.output_ports().ports.get(source_port_name)
                            .ok_or_else(|| PyStreamError::Connection(
                                format!("Source Python processor has no output port '{}'", source_port_name)
                            ))?;
                        let dest_input_arc = dest_py.input_ports().ports.get(dest_port_name)
                            .ok_or_else(|| PyStreamError::Connection(
                                format!("Destination Python processor has no input port '{}'", dest_port_name)
                            ))?;
                        let mut source_output_guard = source_output_arc.lock().unwrap();
                        let mut dest_input_guard = dest_input_arc.lock().unwrap();
                        runtime.connect(
                            &mut *source_output_guard,
                            &mut *dest_input_guard,
                        ).map_err(|e| PyStreamError::Connection(format!("Failed to connect: {}", e)))?;
                    }
                    _ => {
                        return Err(PyStreamError::Connection(
                            format!("Unsupported connection: {}.{} → {}.{}",
                                source_port.processor_name, source_port.port_name,
                                dest_port.processor_name, dest_port.port_name)
                        ).into());
                    }
                }

                // Put source processor back
                processor_instances.insert(source_port.processor_name.clone(), source_proc);
            }

            drop(processors);
            drop(connections);

            // Step 3: Box processors and add to runtime
            println!("   Adding processors to runtime...");
            for (_name, processor) in processor_instances {
                match processor {
                    ProcessorInstance::Camera(camera) => {
                        runtime.add_processor(Box::new(camera));
                    }
                    ProcessorInstance::Display(display) => {
                        runtime.add_processor(Box::new(display));
                    }
                    ProcessorInstance::Python(python) => {
                        runtime.add_processor(Box::new(python));
                    }
                }
            }
        }

        println!("✅ Processors instantiated");
        println!("🚀 Starting runtime...\n");

        // Run the runtime (blocks until Ctrl+C)
        py.allow_threads(|| {
            // Create a tokio runtime for async execution
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| PyStreamError::Runtime(format!("Failed to create tokio runtime: {}", e)))?;

            rt.block_on(async {
                runtime.start().await
                    .map_err(|e| PyStreamError::Runtime(format!("Failed to start: {}", e)))?;

                runtime.run().await
                    .map_err(|e| PyStreamError::Runtime(format!("Runtime error: {}", e)))?;

                Ok::<(), PyStreamError>(())
            })?;

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
