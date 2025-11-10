
use pyo3::prelude::*;
use crate::core::{
    StreamProcessor, TimedTick, Result,
    StreamInput, StreamOutput, VideoFrame, GpuContext,
    ProcessorDescriptor, PortDescriptor, SCHEMA_VIDEO_FRAME,
};
use super::types_ext::{PyStreamInput, PyStreamOutput, PyInputPorts, PyOutputPorts, PyGpuContext};
use std::collections::HashMap;
use std::sync::Arc;

pub struct PythonInputPorts {
    pub ports: HashMap<String, StreamInput<VideoFrame>>,
}

pub struct PythonOutputPorts {
    pub ports: HashMap<String, StreamOutput<VideoFrame>>,
}

pub struct PythonProcessor {
    python_class: Py<PyAny>,

    python_instance: Option<Py<PyAny>>,

    name: String,

    input_ports: PythonInputPorts,

    output_ports: PythonOutputPorts,

    py_input_ports: Option<Py<PyInputPorts>>,
    py_output_ports: Option<Py<PyOutputPorts>>,
    py_gpu_context: Option<Py<PyGpuContext>>,

    gpu_context: Option<GpuContext>,

    description: Option<String>,
    usage_context: Option<String>,
    tags: Vec<String>,
}

impl PythonProcessor {
    pub fn new(
        python_class: Py<PyAny>,
        name: String,
        input_port_names: Vec<String>,
        output_port_names: Vec<String>,
        description: Option<String>,
        usage_context: Option<String>,
        tags: Vec<String>,
    ) -> Result<Self> {
        let mut input_ports_map = HashMap::new();
        for port_name in input_port_names {
            input_ports_map.insert(port_name.clone(), StreamInput::new(&port_name));
        }

        let mut output_ports_map = HashMap::new();
        for port_name in output_port_names {
            output_ports_map.insert(port_name.clone(), StreamOutput::new(&port_name));
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

    /// Discover input ports from Python class decorated with @StreamProcessor
    fn discover_input_ports(py: Python, python_class: &PyAny) -> Result<Vec<String>> {
        use crate::core::StreamError;

        match python_class.getattr("__streamlib_inputs__") {
            Ok(inputs_dict) => {
                // Parse dictionary of port metadata
                let mut port_names = Vec::new();

                if let Ok(dict) = inputs_dict.downcast::<pyo3::types::PyDict>() {
                    for (name, _metadata) in dict.iter() {
                        if let Ok(port_name) = name.extract::<String>() {
                            port_names.push(port_name);
                        }
                    }
                }

                Ok(port_names)
            }
            Err(_) => {
                // No @StreamProcessor decorator or no input ports
                Ok(Vec::new())
            }
        }
    }

    /// Discover output ports from Python class decorated with @StreamProcessor
    fn discover_output_ports(py: Python, python_class: &PyAny) -> Result<Vec<String>> {
        use crate::core::StreamError;

        match python_class.getattr("__streamlib_outputs__") {
            Ok(outputs_dict) => {
                // Parse dictionary of port metadata
                let mut port_names = Vec::new();

                if let Ok(dict) = outputs_dict.downcast::<pyo3::types::PyDict>() {
                    for (name, _metadata) in dict.iter() {
                        if let Ok(port_name) = name.extract::<String>() {
                            port_names.push(port_name);
                        }
                    }
                }

                Ok(port_names)
            }
            Err(_) => {
                // No @StreamProcessor decorator or no output ports
                Ok(Vec::new())
            }
        }
    }

    /// Create a PythonProcessor from a Python class using decorator-based port discovery
    pub fn from_python_class(
        python_class: Py<PyAny>,
        name: String,
        description: Option<String>,
        usage_context: Option<String>,
        tags: Vec<String>,
    ) -> Result<Self> {
        use crate::core::StreamError;

        Python::with_gil(|py| -> Result<Self> {
            let class_ref = python_class.bind(py);

            // Discover ports from Python decorators
            let input_port_names = Self::discover_input_ports(py, class_ref)?;
            let output_port_names = Self::discover_output_ports(py, class_ref)?;

            tracing::info!(
                "[PythonProcessor] Discovered {} input ports and {} output ports from Python class",
                input_port_names.len(),
                output_port_names.len()
            );

            // Use the standard constructor with discovered port names
            Self::new(
                python_class,
                name,
                input_port_names,
                output_port_names,
                description,
                usage_context,
                tags,
            )
        })
    }

    pub fn input_ports(&mut self) -> &mut PythonInputPorts {
        &mut self.input_ports
    }

    pub fn output_ports(&mut self) -> &mut PythonOutputPorts {
        &mut self.output_ports
    }
}

impl PythonProcessor {
    pub fn get_descriptor(&self) -> ProcessorDescriptor {
        let mut descriptor = ProcessorDescriptor::new(
            &self.name,
            self.description.as_deref().unwrap_or("Custom Python processor")
        );

        if let Some(usage) = &self.usage_context {
            descriptor = descriptor.with_usage_context(usage);
        }

        for (port_name, _) in &self.input_ports.ports {
            descriptor = descriptor.with_input(PortDescriptor::new(
                port_name,
                Arc::clone(&SCHEMA_VIDEO_FRAME),
                true,
                format!("Input port '{}'", port_name),
            ));
        }

        for (port_name, _) in &self.output_ports.ports {
            descriptor = descriptor.with_output(PortDescriptor::new(
                port_name,
                Arc::clone(&SCHEMA_VIDEO_FRAME),
                true,
                format!("Output port '{}'", port_name),
            ));
        }

        if !self.tags.is_empty() {
            descriptor = descriptor.with_tags(self.tags.clone());
        }

        descriptor
    }
}

impl StreamProcessor for PythonProcessor {
    fn descriptor() -> Option<ProcessorDescriptor> {
        None
    }

    fn on_start(&mut self, gpu_context: &GpuContext) -> Result<()> {
        use crate::core::StreamError;

        self.gpu_context = Some(gpu_context.clone());

        Python::with_gil(|py| -> Result<()> {
            self.python_instance = Some(
                self.python_class.call0(py)
                    .map_err(|e| StreamError::Configuration(format!("Failed to instantiate Python processor: {}", e)))?
            );

            let mut py_inputs_map = HashMap::new();
            for (name, rust_port) in &self.input_ports.ports {
                // Wrap the port in Arc<Mutex<>> for Python access
                py_inputs_map.insert(name.clone(), PyStreamInput {
                    port: Arc::new(parking_lot::Mutex::new(rust_port.clone())),
                });
            }

            let mut py_outputs_map = HashMap::new();
            for (name, rust_port) in &self.output_ports.ports {
                // Wrap the port in Arc<Mutex<>> for Python access
                py_outputs_map.insert(name.clone(), PyStreamOutput {
                    port: Arc::new(parking_lot::Mutex::new(rust_port.clone())),
                });
            }

            self.py_input_ports = Some(Py::new(py, PyInputPorts::new(py_inputs_map))
                .map_err(|e| StreamError::Configuration(format!("Failed to create input ports wrapper: {}", e)))?);

            self.py_output_ports = Some(Py::new(py, PyOutputPorts::new(py_outputs_map))
                .map_err(|e| StreamError::Configuration(format!("Failed to create output ports wrapper: {}", e)))?);

            self.py_gpu_context = Some(Py::new(py, PyGpuContext::from_rust(gpu_context))
                .map_err(|e| StreamError::Configuration(format!("Failed to create GPU context wrapper: {}", e)))?);

            let instance = self.python_instance.as_ref().unwrap();
            instance.setattr(py, "_input_ports", self.py_input_ports.as_ref().unwrap())
                .map_err(|e| StreamError::Configuration(format!("Failed to inject input ports: {}", e)))?;
            instance.setattr(py, "_output_ports", self.py_output_ports.as_ref().unwrap())
                .map_err(|e| StreamError::Configuration(format!("Failed to inject output ports: {}", e)))?;
            instance.setattr(py, "_gpu_context", self.py_gpu_context.as_ref().unwrap())
                .map_err(|e| StreamError::Configuration(format!("Failed to inject GPU context: {}", e)))?;

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

            let locals = pyo3::types::PyDict::new_bound(py);
            py.run_bound(setup_code, None, Some(&locals))
                .map_err(|e| StreamError::Configuration(format!("Failed to define accessor methods: {}", e)))?;

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
                let py_tick = Py::new(py, PyTimedTick::from_rust(tick))
                    .map_err(|e| StreamError::Configuration(format!("Failed to create tick wrapper: {}", e)))?;

                instance.call_method1(py, "process", (py_tick,))
                    .map_err(|e| {
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

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        // For now, PythonProcessor only supports VideoFrame ports
        if self.input_ports.ports.contains_key(port_name) {
            Some(crate::core::bus::PortType::Video)
        } else {
            None
        }
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        // For now, PythonProcessor only supports VideoFrame ports
        if self.output_ports.ports.contains_key(port_name) {
            Some(crate::core::bus::PortType::Video)
        } else {
            None
        }
    }

    fn wire_input_connection(&mut self, port_name: &str, connection: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;

        if let Some(port) = self.input_ports.ports.get_mut(port_name) {
            if let Ok(typed_conn) = connection.downcast::<Arc<ProcessorConnection<VideoFrame>>>() {
                port.set_connection(Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }

    fn wire_output_connection(&mut self, port_name: &str, connection: Arc<dyn std::any::Any + Send + Sync>) -> bool {
        use crate::core::bus::ProcessorConnection;

        if let Some(port) = self.output_ports.ports.get_mut(port_name) {
            if let Ok(typed_conn) = connection.downcast::<Arc<ProcessorConnection<VideoFrame>>>() {
                port.add_connection(Arc::clone(&typed_conn));
                return true;
            }
        }
        false
    }
}
