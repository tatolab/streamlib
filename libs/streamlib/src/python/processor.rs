//! Python processor - executes Python class instances as stream processors

use pyo3::prelude::*;
use crate::core::{
    StreamProcessor, TimedTick, Result,
    StreamInput, StreamOutput, VideoFrame, GpuContext,
    ProcessorDescriptor, PortDescriptor, SCHEMA_VIDEO_FRAME,
};
use super::types_ext::{PyStreamInput, PyStreamOutput, PyInputPorts, PyOutputPorts, PyGpuContext};
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;

/// Input ports for Python processors (dynamically sized)
pub struct PythonInputPorts {
    /// Map of port names to input ports (wrapped in Arc<Mutex<>> for sharing with Python)
    /// Currently only supports VideoFrame ports
    pub ports: HashMap<String, Arc<Mutex<StreamInput<VideoFrame>>>>,
}

/// Output ports for Python processors (dynamically sized)
pub struct PythonOutputPorts {
    /// Map of port names to output ports (wrapped in Arc<Mutex<>> for sharing with Python)
    /// Currently only supports VideoFrame ports
    pub ports: HashMap<String, Arc<Mutex<StreamOutput<VideoFrame>>>>,
}

/// Python processor that executes a Python class instance on each tick
///
/// This processor:
/// - Stores a Python class (from decorator)
/// - Instantiates the class in on_start()
/// - Has configurable input/output ports based on decorator metadata
/// - Acquires GIL on each tick to call the instance.process(tick) method
/// - Handles VideoFrame inputs/outputs
pub struct PythonProcessor {
    /// Python class (from decorator)
    python_class: Py<PyAny>,

    /// Python instance (created in on_start)
    python_instance: Option<Py<PyAny>>,

    /// Processor name (for logging/debugging)
    name: String,

    /// Rust input ports (for connection wiring in runtime.rs)
    input_ports: PythonInputPorts,

    /// Rust output ports (for connection wiring in runtime.rs)
    output_ports: PythonOutputPorts,

    /// Python port wrappers (passed to Python instance)
    py_input_ports: Option<Py<PyInputPorts>>,
    py_output_ports: Option<Py<PyOutputPorts>>,
    py_gpu_context: Option<Py<PyGpuContext>>,

    /// GPU context
    gpu_context: Option<GpuContext>,

    /// Schema metadata for AI discovery
    description: Option<String>,
    usage_context: Option<String>,
    tags: Vec<String>,
}

impl PythonProcessor {
    /// Create a new Python processor
    ///
    /// # Arguments
    /// * `python_class` - Python class (from decorator)
    /// * `name` - Processor name for logging
    /// * `input_port_names` - Names of input ports
    /// * `output_port_names` - Names of output ports
    /// * `description` - Human-readable description for AI agents
    /// * `usage_context` - When/how to use this processor
    /// * `tags` - Tags for categorization/discovery
    pub fn new(
        python_class: Py<PyAny>,
        name: String,
        input_port_names: Vec<String>,
        output_port_names: Vec<String>,
        description: Option<String>,
        usage_context: Option<String>,
        tags: Vec<String>,
    ) -> Result<Self> {
        // Create Rust input ports (wrapped in Arc<Mutex<>> for sharing with Python)
        let mut input_ports_map = HashMap::new();
        for port_name in input_port_names {
            input_ports_map.insert(port_name.clone(), Arc::new(Mutex::new(StreamInput::new(&port_name))));
        }

        // Create Rust output ports (wrapped in Arc<Mutex<>> for sharing with Python)
        let mut output_ports_map = HashMap::new();
        for port_name in output_port_names {
            output_ports_map.insert(port_name.clone(), Arc::new(Mutex::new(StreamOutput::new(&port_name))));
        }

        Ok(Self {
            python_class,
            python_instance: None,
            name,
            input_ports: PythonInputPorts {
                ports: input_ports_map,
            },
            output_ports: PythonOutputPorts {
                ports: output_ports_map,
            },
            py_input_ports: None,
            py_output_ports: None,
            py_gpu_context: None,
            gpu_context: None,
            description,
            usage_context,
            tags,
        })
    }

    /// Get input ports
    pub fn input_ports(&mut self) -> &mut PythonInputPorts {
        &mut self.input_ports
    }

    /// Get output ports
    pub fn output_ports(&mut self) -> &mut PythonOutputPorts {
        &mut self.output_ports
    }
}

impl PythonProcessor {
    /// Get a descriptor for this specific processor instance
    ///
    /// Since Python processors are defined dynamically, each instance has its own unique descriptor.
    pub fn get_descriptor(&self) -> ProcessorDescriptor {
        // Create a descriptor based on this instance's metadata
        let mut descriptor = ProcessorDescriptor::new(
            &self.name,
            self.description.as_deref().unwrap_or("Custom Python processor")
        );

        // Add usage context if provided
        if let Some(usage) = &self.usage_context {
            descriptor = descriptor.with_usage_context(usage);
        }

        // Add input ports (all VideoFrame for now)
        for (port_name, _) in &self.input_ports.ports {
            descriptor = descriptor.with_input(PortDescriptor::new(
                port_name,
                Arc::clone(&SCHEMA_VIDEO_FRAME),
                true,
                format!("Input port '{}'", port_name),
            ));
        }

        // Add output ports (all VideoFrame for now)
        for (port_name, _) in &self.output_ports.ports {
            descriptor = descriptor.with_output(PortDescriptor::new(
                port_name,
                Arc::clone(&SCHEMA_VIDEO_FRAME),
                true,
                format!("Output port '{}'", port_name),
            ));
        }

        // Add tags
        if !self.tags.is_empty() {
            descriptor = descriptor.with_tags(self.tags.clone());
        }

        descriptor
    }
}

impl StreamProcessor for PythonProcessor {
    fn descriptor() -> Option<ProcessorDescriptor> {
        // Python processors don't have a static descriptor because each instance
        // is unique (defined by Python code). Use get_descriptor() on instances instead.
        None
    }

    fn on_start(&mut self, gpu_context: &GpuContext) -> Result<()> {
        use crate::core::StreamError;

        self.gpu_context = Some(gpu_context.clone());

        Python::with_gil(|py| -> Result<()> {
            // 1. Instantiate Python class
            self.python_instance = Some(
                self.python_class.call0(py)
                    .map_err(|e| StreamError::Configuration(format!("Failed to instantiate Python processor: {}", e)))?
            );

            // 2. Create Python port wrappers from Rust ports (sharing Arc references)
            let mut py_inputs_map = HashMap::new();
            for (name, rust_port_arc) in &self.input_ports.ports {
                py_inputs_map.insert(name.clone(), PyStreamInput {
                    port: Arc::clone(rust_port_arc),
                });
            }

            let mut py_outputs_map = HashMap::new();
            for (name, rust_port_arc) in &self.output_ports.ports {
                py_outputs_map.insert(name.clone(), PyStreamOutput {
                    port: Arc::clone(rust_port_arc),
                });
            }

            self.py_input_ports = Some(Py::new(py, PyInputPorts::new(py_inputs_map))
                .map_err(|e| StreamError::Configuration(format!("Failed to create input ports wrapper: {}", e)))?);

            self.py_output_ports = Some(Py::new(py, PyOutputPorts::new(py_outputs_map))
                .map_err(|e| StreamError::Configuration(format!("Failed to create output ports wrapper: {}", e)))?);

            self.py_gpu_context = Some(Py::new(py, PyGpuContext::from_rust(gpu_context))
                .map_err(|e| StreamError::Configuration(format!("Failed to create GPU context wrapper: {}", e)))?);

            // 3. Inject into Python instance
            let instance = self.python_instance.as_ref().unwrap();
            instance.setattr(py, "_input_ports", self.py_input_ports.as_ref().unwrap())
                .map_err(|e| StreamError::Configuration(format!("Failed to inject input ports: {}", e)))?;
            instance.setattr(py, "_output_ports", self.py_output_ports.as_ref().unwrap())
                .map_err(|e| StreamError::Configuration(format!("Failed to inject output ports: {}", e)))?;
            instance.setattr(py, "_gpu_context", self.py_gpu_context.as_ref().unwrap())
                .map_err(|e| StreamError::Configuration(format!("Failed to inject GPU context: {}", e)))?;

            // 4. Add accessor methods via Python code
            let setup_code = r#"
def input_ports(self):
    return self._input_ports

def output_ports(self):
    return self._output_ports

def gpu_context(self):
    return self._gpu_context
"#;

            let module = py.import_bound("types").map_err(|e| StreamError::Configuration(format!("Failed to import types module: {}", e)))?;
            let method_type = module.getattr("MethodType").map_err(|e| StreamError::Configuration(format!("Failed to get MethodType: {}", e)))?;

            // Compile and execute setup code
            let locals = pyo3::types::PyDict::new_bound(py);
            py.run_bound(setup_code, None, Some(&locals))
                .map_err(|e| StreamError::Configuration(format!("Failed to define accessor methods: {}", e)))?;

            // Bind methods to instance
            for method_name in ["input_ports", "output_ports", "gpu_context"] {
                let func = locals.get_item(method_name)
                    .map_err(|e| StreamError::Configuration(format!("Failed to get {}: {}", method_name, e)))?
                    .ok_or_else(|| StreamError::Configuration(format!("{} not found in locals", method_name)))?;

                let bound_method = method_type.call1((func, instance))
                    .map_err(|e| StreamError::Configuration(format!("Failed to bind {}: {}", method_name, e)))?;

                instance.setattr(py, method_name, bound_method)
                    .map_err(|e| StreamError::Configuration(format!("Failed to set {}: {}", method_name, e)))?;
            }

            Ok(())
        })?;

        tracing::info!("[PythonProcessor:{}] Started and instantiated", self.name);
        Ok(())
    }

    fn process(&mut self, tick: TimedTick) -> Result<()> {
        use crate::core::StreamError;
        use super::types_ext::PyTimedTick;

        if let Some(instance) = &self.python_instance {
            Python::with_gil(|py| -> Result<()> {
                // Create PyTimedTick wrapper
                let py_tick = Py::new(py, PyTimedTick::from_rust(tick))
                    .map_err(|e| StreamError::Configuration(format!("Failed to create tick wrapper: {}", e)))?;

                // Call instance.process(tick)
                instance.call_method1(py, "process", (py_tick,))
                    .map_err(|e| {
                        // Extract Python traceback for better error messages
                        let traceback = if let Some(traceback) = e.traceback_bound(py) {
                            match traceback.format() {
                                Ok(tb) => format!("\n{}", tb),
                                Err(_) => String::new(),
                            }
                        } else {
                            String::new()
                        };
                        StreamError::Configuration(format!("Python process() failed: {}{}", e, traceback))
                    })?;

                Ok(())
            })
        } else {
            // If no instance (shouldn't happen after on_start), just skip
            Ok(())
        }
    }

    fn on_stop(&mut self) -> Result<()> {
        tracing::info!("[PythonProcessor:{}] Stopped", self.name);
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
