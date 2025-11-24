use super::types_ext::{
    PyGpuContext, PyRuntimeContext, PyStreamInput, PyStreamInputAudio1, PyStreamInputAudio2,
    PyStreamInputAudio4, PyStreamInputAudio6, PyStreamInputAudio8, PyStreamInputData,
    PyStreamOutput, PyStreamOutputAudio1, PyStreamOutputAudio2, PyStreamOutputAudio4,
    PyStreamOutputAudio6, PyStreamOutputAudio8, PyStreamOutputData,
};
use crate::core::{
    AudioFrame, DataFrame, ElementType, GpuContext, PortDescriptor, ProcessorDescriptor, Result,
    RuntimeContext, StreamElement, StreamInput, StreamOutput, StreamProcessor, VideoFrame,
    SCHEMA_AUDIO_FRAME, SCHEMA_DATA_MESSAGE, SCHEMA_VIDEO_FRAME,
};
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Metadata about a port - tells us what type of port the Python processor expects.
/// We don't store actual Rust ports here - those are created on-demand during wiring.
#[derive(Clone, Debug)]
pub struct PortMetadata {
    pub name: String,
    pub frame_type: FrameType,
    pub description: String,
    pub required: bool,
}

/// Frame type parsed from Python decorator (e.g., "VideoFrame", "AudioFrame", "DataFrame")
#[derive(Clone, Debug, PartialEq)]
pub enum FrameType {
    Video,
    Audio(usize), // number of channels
    Data,
}

impl FrameType {
    /// Parse frame type from Python decorator string
    pub fn from_string(s: &str) -> Result<Self> {
        use crate::core::StreamError;

        if s == "VideoFrame" {
            Ok(FrameType::Video)
        } else if s == "DataFrame" {
            Ok(FrameType::Data)
        } else if s.starts_with("AudioFrame<") && s.ends_with(">") {
            // Parse "AudioFrame" -> 2
            let channels_str = &s[11..s.len() - 1];
            let channels = channels_str.parse::<usize>().map_err(|_| {
                StreamError::Configuration(format!("Invalid audio channels: {}", channels_str))
            })?;

            // Validate supported channel counts (1, 2, 4, 6, 8)
            match channels {
                1 | 2 | 4 | 6 | 8 => Ok(FrameType::Audio(channels)),
                _ => Err(StreamError::Configuration(format!(
                    "Unsupported audio channel count: {}. Supported: 1, 2, 4, 6, 8",
                    channels
                ))),
            }
        } else {
            Err(StreamError::Configuration(format!(
                "Unknown frame type: {}",
                s
            )))
        }
    }

    /// Get the schema for this frame type
    pub fn schema(&self) -> Arc<crate::core::Schema> {
        match self {
            FrameType::Video => SCHEMA_VIDEO_FRAME.clone(),
            FrameType::Audio(_) => SCHEMA_AUDIO_FRAME.clone(),
            FrameType::Data => SCHEMA_DATA_MESSAGE.clone(),
        }
    }
}

/// Configuration for PythonProcessor
///
/// This is the ONLY way to create a PythonProcessor - via runtime.add_processor_with_config().
/// Direct instantiation with ::new() is not supported.
pub struct PythonProcessorConfig {
    pub python_class: Py<PyAny>,
    pub name: String,
    pub input_metadata: Vec<PortMetadata>,
    pub output_metadata: Vec<PortMetadata>,
    pub description: Option<String>,
    pub usage_context: Option<String>,
    pub tags: Vec<String>,
}

impl Clone for PythonProcessorConfig {
    fn clone(&self) -> Self {
        Python::with_gil(|py| Self {
            python_class: self.python_class.clone_ref(py),
            name: self.name.clone(),
            input_metadata: self.input_metadata.clone(),
            output_metadata: self.output_metadata.clone(),
            description: self.description.clone(),
            usage_context: self.usage_context.clone(),
            tags: self.tags.clone(),
        })
    }
}

// Workaround: Python processors can't be serialized (they contain Python objects)
// This is fine - they're dynamically created from decorators, not loaded from config files
impl Serialize for PythonProcessorConfig {
    fn serialize<S>(&self, _serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Err(serde::ser::Error::custom(
            "PythonProcessorConfig cannot be serialized - Python processors are created dynamically"
        ))
    }
}

impl<'de> Deserialize<'de> for PythonProcessorConfig {
    fn deserialize<D>(_deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Err(serde::de::Error::custom(
            "PythonProcessorConfig cannot be deserialized - Python processors are created dynamically"
        ))
    }
}

impl Default for PythonProcessorConfig {
    fn default() -> Self {
        Python::with_gil(|py| Self {
            python_class: py.None(),
            name: String::new(),
            input_metadata: Vec::new(),
            output_metadata: Vec::new(),
            description: None,
            usage_context: None,
            tags: Vec::new(),
        })
    }
}

pub struct PythonProcessor {
    python_class: Py<PyAny>,
    python_instance: Option<Py<PyAny>>,

    name: String,

    // METADATA ONLY - no actual Rust ports stored!
    // Ports are created on-demand during wiring and injected as FFI wrappers
    input_metadata: Vec<PortMetadata>,
    output_metadata: Vec<PortMetadata>,

    gpu_context: Option<GpuContext>,

    description: Option<String>,
    usage_context: Option<String>,
    tags: Vec<String>,
}

impl PythonProcessor {
    // NO ::new() method!
    // PythonProcessor follows the same pattern as AppleCameraProcessor:
    // - Users must use: runtime.add_processor_with_config::<PythonProcessor>(config)
    // - The from_config() method is the ONLY way to create instances

    pub fn input_metadata(&self) -> &[PortMetadata] {
        &self.input_metadata
    }

    pub fn output_metadata(&self) -> &[PortMetadata] {
        &self.output_metadata
    }

    /// Discover input ports from Python class decorated with @video_input, @audio_input, etc.
    fn discover_input_ports<'py>(
        _py: Python<'py>,
        python_class: &pyo3::Bound<'py, PyAny>,
    ) -> Result<Vec<PortMetadata>> {
        use crate::core::StreamError;

        match python_class.getattr("__streamlib_inputs__") {
            Ok(inputs_dict) => {
                // Parse dictionary of port metadata
                let mut port_metadata = Vec::new();

                if let Ok(dict) = inputs_dict.downcast::<pyo3::types::PyDict>() {
                    for (name, metadata) in dict.iter() {
                        let port_name = name.extract::<String>().map_err(|e| {
                            StreamError::Configuration(format!("Invalid port name: {}", e))
                        })?;

                        // Extract metadata fields
                        let frame_type_str = metadata
                            .getattr("frame_type")
                            .and_then(|v| v.extract::<String>())
                            .map_err(|e| {
                                StreamError::Configuration(format!(
                                    "Missing or invalid frame_type for port '{}': {}",
                                    port_name, e
                                ))
                            })?;

                        let description = metadata
                            .getattr("description")
                            .and_then(|v| v.extract::<String>())
                            .unwrap_or_else(|_| format!("Input port '{}'", port_name));

                        let required = metadata
                            .getattr("required")
                            .and_then(|v| v.extract::<bool>())
                            .unwrap_or(true);

                        // Parse frame type
                        let frame_type = FrameType::from_string(&frame_type_str)?;

                        port_metadata.push(PortMetadata {
                            name: port_name,
                            frame_type,
                            description,
                            required,
                        });
                    }
                }

                Ok(port_metadata)
            }
            Err(_) => {
                // No decorator metadata found
                Ok(Vec::new())
            }
        }
    }

    /// Discover output ports from Python class decorated with @video_output, @audio_output, etc.
    fn discover_output_ports<'py>(
        _py: Python<'py>,
        python_class: &pyo3::Bound<'py, PyAny>,
    ) -> Result<Vec<PortMetadata>> {
        use crate::core::StreamError;

        match python_class.getattr("__streamlib_outputs__") {
            Ok(outputs_dict) => {
                // Parse dictionary of port metadata
                let mut port_metadata = Vec::new();

                if let Ok(dict) = outputs_dict.downcast::<pyo3::types::PyDict>() {
                    for (name, metadata) in dict.iter() {
                        let port_name = name.extract::<String>().map_err(|e| {
                            StreamError::Configuration(format!("Invalid port name: {}", e))
                        })?;

                        // Extract metadata fields
                        let frame_type_str = metadata
                            .getattr("frame_type")
                            .and_then(|v| v.extract::<String>())
                            .map_err(|e| {
                                StreamError::Configuration(format!(
                                    "Missing or invalid frame_type for port '{}': {}",
                                    port_name, e
                                ))
                            })?;

                        let description = metadata
                            .getattr("description")
                            .and_then(|v| v.extract::<String>())
                            .unwrap_or_else(|_| format!("Output port '{}'", port_name));

                        let required = metadata
                            .getattr("required")
                            .and_then(|v| v.extract::<bool>())
                            .unwrap_or(false);

                        // Parse frame type
                        let frame_type = FrameType::from_string(&frame_type_str)?;

                        port_metadata.push(PortMetadata {
                            name: port_name,
                            frame_type,
                            description,
                            required,
                        });
                    }
                }

                Ok(port_metadata)
            }
            Err(_) => {
                // No decorator metadata found
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
        Python::with_gil(|py| -> Result<Self> {
            let class_ref = python_class.bind(py);

            // Discover ports from Python decorators
            let input_metadata = Self::discover_input_ports(py, class_ref)?;
            let output_metadata = Self::discover_output_ports(py, class_ref)?;

            tracing::info!(
                "[PythonProcessor] Discovered {} input ports and {} output ports from Python class",
                input_metadata.len(),
                output_metadata.len()
            );

            // Construct directly - same pattern as from_config
            Ok(Self {
                python_class,
                python_instance: None,
                name,
                input_metadata,
                output_metadata,
                gpu_context: None,
                description,
                usage_context,
                tags,
            })
        })
    }
}

impl PythonProcessor {
    pub fn get_descriptor(&self) -> ProcessorDescriptor {
        let mut descriptor = ProcessorDescriptor::new(
            &self.name,
            self.description
                .as_deref()
                .unwrap_or("Custom Python processor"),
        );

        if let Some(usage) = &self.usage_context {
            descriptor = descriptor.with_usage_context(usage);
        }

        // Build descriptor from metadata
        for meta in &self.input_metadata {
            descriptor = descriptor.with_input(PortDescriptor::new(
                &meta.name,
                meta.frame_type.schema(),
                meta.required,
                &meta.description,
            ));
        }

        for meta in &self.output_metadata {
            descriptor = descriptor.with_output(PortDescriptor::new(
                &meta.name,
                meta.frame_type.schema(),
                false, // outputs are never required
                &meta.description,
            ));
        }

        if !self.tags.is_empty() {
            descriptor = descriptor.with_tags(self.tags.clone());
        }

        descriptor
    }
}

impl StreamElement for PythonProcessor {
    fn name(&self) -> &str {
        &self.name
    }

    fn element_type(&self) -> ElementType {
        // Determine from metadata
        match (
            self.input_metadata.is_empty(),
            self.output_metadata.is_empty(),
        ) {
            (false, true) => ElementType::Sink,
            (true, false) => ElementType::Source,
            _ => ElementType::Transform,
        }
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        Some(self.get_descriptor())
    }

    fn __generated_setup(&mut self, runtime_context: &RuntimeContext) -> Result<()> {
        use crate::core::StreamError;

        let gpu_context = &runtime_context.gpu;
        self.gpu_context = Some(gpu_context.clone());

        Python::with_gil(|py| -> Result<()> {
            // Instantiate Python class
            self.python_instance = Some(self.python_class.call0(py).map_err(|e| {
                StreamError::Configuration(format!("Failed to instantiate Python processor: {}", e))
            })?);

            let instance = self.python_instance.as_ref().unwrap();

            // Inject GPU context
            let py_gpu_context =
                Py::new(py, PyGpuContext::from_rust(gpu_context)).map_err(|e| {
                    StreamError::Configuration(format!(
                        "Failed to create GPU context wrapper: {}",
                        e
                    ))
                })?;
            instance
                .setattr(py, "_gpu_context", &py_gpu_context)
                .map_err(|e| {
                    StreamError::Configuration(format!("Failed to inject GPU context: {}", e))
                })?;

            // Inject gpu_context() accessor method
            let setup_code = r#"
def gpu_context(self):
    return self._gpu_context
"#;
            let module = py.import_bound("types").map_err(|e| {
                StreamError::Configuration(format!("Failed to import types module: {}", e))
            })?;
            let method_type = module.getattr("MethodType").map_err(|e| {
                StreamError::Configuration(format!("Failed to get MethodType: {}", e))
            })?;

            let locals = pyo3::types::PyDict::new_bound(py);
            py.run_bound(setup_code, None, Some(&locals)).map_err(|e| {
                StreamError::Configuration(format!("Failed to define gpu_context method: {}", e))
            })?;

            let func = locals
                .get_item("gpu_context")
                .map_err(|e| {
                    StreamError::Configuration(format!("Failed to get gpu_context: {}", e))
                })?
                .ok_or_else(|| {
                    StreamError::Configuration("gpu_context not found in locals".to_string())
                })?;

            let bound_method = method_type.call1((func, instance)).map_err(|e| {
                StreamError::Configuration(format!("Failed to bind gpu_context: {}", e))
            })?;

            instance
                .setattr(py, "gpu_context", bound_method)
                .map_err(|e| {
                    StreamError::Configuration(format!("Failed to set gpu_context: {}", e))
                })?;

            // Call Python's setup(ctx) if it exists
            let instance_bound = instance.bind(py);
            let has_setup = instance_bound
                .hasattr("setup")
                .map_err(|e| StreamError::Configuration(format!("hasattr error: {}", e)))?;

            if has_setup {
                tracing::info!("[PythonProcessor:{}] Calling Python setup(ctx)", self.name);
                let py_ctx =
                    Py::new(py, PyRuntimeContext::from_rust(runtime_context)).map_err(|e| {
                        StreamError::Configuration(format!(
                            "Failed to create runtime context wrapper: {}",
                            e
                        ))
                    })?;

                instance.call_method1(py, "setup", (py_ctx,)).map_err(|e| {
                    let traceback = if let Some(traceback) = e.traceback_bound(py) {
                        match traceback.format() {
                            Ok(tb) => format!("\n{}", tb),
                            Err(_) => String::new(),
                        }
                    } else {
                        String::new()
                    };
                    StreamError::Configuration(format!("Python setup() failed: {}{}", e, traceback))
                })?;
            }

            Ok(())
        })?;

        tracing::info!(
            "[PythonProcessor:{}] Setup complete and instantiated",
            self.name
        );
        Ok(())
    }

    fn __generated_teardown(&mut self) -> Result<()> {
        use crate::core::StreamError;

        // Call Python's teardown() if it exists
        if let Some(instance) = &self.python_instance {
            Python::with_gil(|py| -> Result<()> {
                let instance_bound = instance.bind(py);
                let has_teardown = instance_bound
                    .hasattr("teardown")
                    .map_err(|e| StreamError::Configuration(format!("hasattr error: {}", e)))?;

                if has_teardown {
                    tracing::info!("[PythonProcessor:{}] Calling Python teardown()", self.name);

                    instance.call_method0(py, "teardown").map_err(|e| {
                        let traceback = if let Some(traceback) = e.traceback_bound(py) {
                            match traceback.format() {
                                Ok(tb) => format!("\n{}", tb),
                                Err(_) => String::new(),
                            }
                        } else {
                            String::new()
                        };
                        StreamError::Configuration(format!(
                            "Python teardown() failed: {}{}",
                            e, traceback
                        ))
                    })?;
                }
                Ok(())
            })?;
        }

        tracing::info!("[PythonProcessor:{}] Teardown complete", self.name);
        Ok(())
    }
}

impl StreamProcessor for PythonProcessor {
    type Config = PythonProcessorConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        // Construct directly - same pattern as AppleCameraProcessor
        Ok(Self {
            python_class: config.python_class,
            python_instance: None,
            name: config.name,
            input_metadata: config.input_metadata,
            output_metadata: config.output_metadata,
            gpu_context: None,
            description: config.description,
            usage_context: config.usage_context,
            tags: config.tags,
        })
    }

    fn descriptor() -> Option<ProcessorDescriptor> {
        None
    }

    fn process(&mut self) -> Result<()> {
        use crate::core::StreamError;

        if let Some(instance) = &self.python_instance {
            Python::with_gil(|py| -> Result<()> {
                instance.call_method0(py, "process").map_err(|e| {
                    let traceback = if let Some(traceback) = e.traceback_bound(py) {
                        match traceback.format() {
                            Ok(tb) => format!("\n{}", tb),
                            Err(_) => String::new(),
                        }
                    } else {
                        String::new()
                    };
                    StreamError::Configuration(format!(
                        "Python process() failed: {}{}",
                        e, traceback
                    ))
                })?;

                Ok(())
            })
        } else {
            Ok(())
        }
    }

    fn get_input_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        // Look up port type from metadata
        self.input_metadata
            .iter()
            .find(|m| m.name == port_name)
            .map(|m| match &m.frame_type {
                FrameType::Video => crate::core::bus::PortType::Video,
                FrameType::Audio(_) => crate::core::bus::PortType::Audio,
                FrameType::Data => crate::core::bus::PortType::Data,
            })
    }

    fn get_output_port_type(&self, port_name: &str) -> Option<crate::core::bus::PortType> {
        // Look up port type from metadata
        self.output_metadata
            .iter()
            .find(|m| m.name == port_name)
            .map(|m| match &m.frame_type {
                FrameType::Video => crate::core::bus::PortType::Video,
                FrameType::Audio(_) => crate::core::bus::PortType::Audio,
                FrameType::Data => crate::core::bus::PortType::Data,
            })
    }

    fn wire_input_connection(
        &mut self,
        _port_name: &str,
        _connection: Arc<dyn std::any::Any + Send + Sync>,
    ) -> bool {
        // Phase 1 not supported - use wire_input_consumer
        false
    }

    fn wire_output_connection(
        &mut self,
        _port_name: &str,
        _connection: Arc<dyn std::any::Any + Send + Sync>,
    ) -> bool {
        // Phase 1 not supported - use wire_output_producer
        false
    }

    fn wire_input_consumer(
        &mut self,
        port_name: &str,
        consumer: Box<dyn std::any::Any + Send>,
    ) -> bool {
        // Find port metadata
        let port_meta = match self.input_metadata.iter().find(|m| m.name == port_name) {
            Some(meta) => meta,
            None => {
                tracing::warn!(
                    "[PythonProcessor:{}] No metadata found for input port '{}'",
                    self.name,
                    port_name
                );
                return false;
            }
        };

        // Downcast consumer based on frame type and create FFI wrapper
        let result = match &port_meta.frame_type {
            FrameType::Video => {
                if let Ok(typed_consumer) =
                    consumer.downcast::<crate::core::OwnedConsumer<VideoFrame>>()
                {
                    // Phase 0.5: Create port with plug, then add connection
                    let stream_input = StreamInput::new(port_name);

                    // Generate temporary connection ID for backwards compatibility
                    let temp_id = crate::core::bus::connection_id::__private::new_unchecked(
                        format!("{}.wire_compat_{}", port_name, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
                    );
                    let (tx, _rx) = crossbeam_channel::bounded(1);
                    let source_addr = crate::core::PortAddress::new("unknown", port_name);
                    let _ = stream_input.add_connection(temp_id, *typed_consumer, source_addr, tx);

                    // Inject into Python instance
                    Python::with_gil(|py| {
                        if let Some(instance) = &self.python_instance {
                            let py_wrapper = PyStreamInput::from_port(stream_input);
                            instance.setattr(py, port_name, py_wrapper).is_ok()
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Cannot wire VideoFrame before instance created", self.name);
                            false
                        }
                    })
                } else {
                    tracing::warn!(
                        "[PythonProcessor:{}] Failed to downcast VideoFrame consumer for port '{}'",
                        self.name,
                        port_name
                    );
                    false
                }
            }
            FrameType::Audio(channels) => {
                // Handle all supported audio channel counts
                match channels {
                    1 => {
                        if let Ok(typed_consumer) =
                            consumer.downcast::<crate::core::OwnedConsumer<AudioFrame>>()
                        {
                            let stream_input = StreamInput::new(port_name);
                            stream_input.set_consumer(*typed_consumer);

                            Python::with_gil(|py| {
                                if let Some(instance) = &self.python_instance {
                                    let py_wrapper = PyStreamInputAudio1::from_port(stream_input);
                                    instance.setattr(py, port_name, py_wrapper).is_ok()
                                } else {
                                    tracing::warn!("[PythonProcessor:{}] Cannot wire AudioFrame before instance created", self.name);
                                    false
                                }
                            })
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Failed to downcast AudioFrame consumer for port '{}'", self.name, port_name);
                            false
                        }
                    }
                    2 => {
                        if let Ok(typed_consumer) =
                            consumer.downcast::<crate::core::OwnedConsumer<AudioFrame>>()
                        {
                            let stream_input = StreamInput::new(port_name);
                            stream_input.set_consumer(*typed_consumer);

                            Python::with_gil(|py| {
                                if let Some(instance) = &self.python_instance {
                                    let py_wrapper = PyStreamInputAudio2::from_port(stream_input);
                                    instance.setattr(py, port_name, py_wrapper).is_ok()
                                } else {
                                    tracing::warn!("[PythonProcessor:{}] Cannot wire AudioFrame before instance created", self.name);
                                    false
                                }
                            })
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Failed to downcast AudioFrame consumer for port '{}'", self.name, port_name);
                            false
                        }
                    }
                    4 => {
                        if let Ok(typed_consumer) =
                            consumer.downcast::<crate::core::OwnedConsumer<AudioFrame>>()
                        {
                            let stream_input = StreamInput::new(port_name);
                            stream_input.set_consumer(*typed_consumer);

                            Python::with_gil(|py| {
                                if let Some(instance) = &self.python_instance {
                                    let py_wrapper = PyStreamInputAudio4::from_port(stream_input);
                                    instance.setattr(py, port_name, py_wrapper).is_ok()
                                } else {
                                    tracing::warn!("[PythonProcessor:{}] Cannot wire AudioFrame before instance created", self.name);
                                    false
                                }
                            })
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Failed to downcast AudioFrame consumer for port '{}'", self.name, port_name);
                            false
                        }
                    }
                    6 => {
                        if let Ok(typed_consumer) =
                            consumer.downcast::<crate::core::OwnedConsumer<AudioFrame>>()
                        {
                            let stream_input = StreamInput::new(port_name);
                            stream_input.set_consumer(*typed_consumer);

                            Python::with_gil(|py| {
                                if let Some(instance) = &self.python_instance {
                                    let py_wrapper = PyStreamInputAudio6::from_port(stream_input);
                                    instance.setattr(py, port_name, py_wrapper).is_ok()
                                } else {
                                    tracing::warn!("[PythonProcessor:{}] Cannot wire AudioFrame before instance created", self.name);
                                    false
                                }
                            })
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Failed to downcast AudioFrame consumer for port '{}'", self.name, port_name);
                            false
                        }
                    }
                    8 => {
                        if let Ok(typed_consumer) =
                            consumer.downcast::<crate::core::OwnedConsumer<AudioFrame>>()
                        {
                            let stream_input = StreamInput::new(port_name);
                            stream_input.set_consumer(*typed_consumer);

                            Python::with_gil(|py| {
                                if let Some(instance) = &self.python_instance {
                                    let py_wrapper = PyStreamInputAudio8::from_port(stream_input);
                                    instance.setattr(py, port_name, py_wrapper).is_ok()
                                } else {
                                    tracing::warn!("[PythonProcessor:{}] Cannot wire AudioFrame before instance created", self.name);
                                    false
                                }
                            })
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Failed to downcast AudioFrame consumer for port '{}'", self.name, port_name);
                            false
                        }
                    }
                    _ => {
                        tracing::warn!(
                            "[PythonProcessor:{}] Unsupported audio channel count {} for port '{}'",
                            self.name,
                            channels,
                            port_name
                        );
                        false
                    }
                }
            }
            FrameType::Data => {
                if let Ok(typed_consumer) =
                    consumer.downcast::<crate::core::OwnedConsumer<DataFrame>>()
                {
                    let stream_input = StreamInput::new(port_name);
                    stream_input.set_consumer(*typed_consumer);

                    Python::with_gil(|py| {
                        if let Some(instance) = &self.python_instance {
                            let py_wrapper = PyStreamInputData::from_port(stream_input);
                            instance.setattr(py, port_name, py_wrapper).is_ok()
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Cannot wire DataFrame before instance created", self.name);
                            false
                        }
                    })
                } else {
                    tracing::warn!(
                        "[PythonProcessor:{}] Failed to downcast DataFrame consumer for port '{}'",
                        self.name,
                        port_name
                    );
                    false
                }
            }
        };

        if result {
            tracing::info!(
                "[PythonProcessor:{}] Successfully wired input port '{}' (type: {:?})",
                self.name,
                port_name,
                port_meta.frame_type
            );
        }

        result
    }

    fn wire_output_producer(
        &mut self,
        port_name: &str,
        producer: Box<dyn std::any::Any + Send>,
    ) -> bool {
        // Find port metadata
        let port_meta = match self.output_metadata.iter().find(|m| m.name == port_name) {
            Some(meta) => meta,
            None => {
                tracing::warn!(
                    "[PythonProcessor:{}] No metadata found for output port '{}'",
                    self.name,
                    port_name
                );
                return false;
            }
        };

        // Downcast producer based on frame type and create FFI wrapper
        let result = match &port_meta.frame_type {
            FrameType::Video => {
                if let Ok(typed_producer) =
                    producer.downcast::<crate::core::OwnedProducer<VideoFrame>>()
                {
                    // Create StreamOutput and wrap in PyStreamOutput
                    let stream_output = StreamOutput::new(port_name);
                    stream_output.add_producer(*typed_producer);

                    // Inject into Python instance
                    Python::with_gil(|py| {
                        if let Some(instance) = &self.python_instance {
                            let py_wrapper = PyStreamOutput::from_port(stream_output);
                            instance.setattr(py, port_name, py_wrapper).is_ok()
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Cannot wire VideoFrame before instance created", self.name);
                            false
                        }
                    })
                } else {
                    tracing::warn!(
                        "[PythonProcessor:{}] Failed to downcast VideoFrame producer for port '{}'",
                        self.name,
                        port_name
                    );
                    false
                }
            }
            FrameType::Audio(channels) => {
                // Handle all supported audio channel counts
                match channels {
                    1 => {
                        if let Ok(typed_producer) =
                            producer.downcast::<crate::core::OwnedProducer<AudioFrame>>()
                        {
                            let stream_output = StreamOutput::new(port_name);
                            stream_output.add_producer(*typed_producer);

                            Python::with_gil(|py| {
                                if let Some(instance) = &self.python_instance {
                                    let py_wrapper = PyStreamOutputAudio1::from_port(stream_output);
                                    instance.setattr(py, port_name, py_wrapper).is_ok()
                                } else {
                                    tracing::warn!("[PythonProcessor:{}] Cannot wire AudioFrame before instance created", self.name);
                                    false
                                }
                            })
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Failed to downcast AudioFrame producer for port '{}'", self.name, port_name);
                            false
                        }
                    }
                    2 => {
                        if let Ok(typed_producer) =
                            producer.downcast::<crate::core::OwnedProducer<AudioFrame>>()
                        {
                            let stream_output = StreamOutput::new(port_name);
                            stream_output.add_producer(*typed_producer);

                            Python::with_gil(|py| {
                                if let Some(instance) = &self.python_instance {
                                    let py_wrapper = PyStreamOutputAudio2::from_port(stream_output);
                                    instance.setattr(py, port_name, py_wrapper).is_ok()
                                } else {
                                    tracing::warn!("[PythonProcessor:{}] Cannot wire AudioFrame before instance created", self.name);
                                    false
                                }
                            })
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Failed to downcast AudioFrame producer for port '{}'", self.name, port_name);
                            false
                        }
                    }
                    4 => {
                        if let Ok(typed_producer) =
                            producer.downcast::<crate::core::OwnedProducer<AudioFrame>>()
                        {
                            let stream_output = StreamOutput::new(port_name);
                            stream_output.add_producer(*typed_producer);

                            Python::with_gil(|py| {
                                if let Some(instance) = &self.python_instance {
                                    let py_wrapper = PyStreamOutputAudio4::from_port(stream_output);
                                    instance.setattr(py, port_name, py_wrapper).is_ok()
                                } else {
                                    tracing::warn!("[PythonProcessor:{}] Cannot wire AudioFrame before instance created", self.name);
                                    false
                                }
                            })
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Failed to downcast AudioFrame producer for port '{}'", self.name, port_name);
                            false
                        }
                    }
                    6 => {
                        if let Ok(typed_producer) =
                            producer.downcast::<crate::core::OwnedProducer<AudioFrame>>()
                        {
                            let stream_output = StreamOutput::new(port_name);
                            stream_output.add_producer(*typed_producer);

                            Python::with_gil(|py| {
                                if let Some(instance) = &self.python_instance {
                                    let py_wrapper = PyStreamOutputAudio6::from_port(stream_output);
                                    instance.setattr(py, port_name, py_wrapper).is_ok()
                                } else {
                                    tracing::warn!("[PythonProcessor:{}] Cannot wire AudioFrame before instance created", self.name);
                                    false
                                }
                            })
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Failed to downcast AudioFrame producer for port '{}'", self.name, port_name);
                            false
                        }
                    }
                    8 => {
                        if let Ok(typed_producer) =
                            producer.downcast::<crate::core::OwnedProducer<AudioFrame>>()
                        {
                            let stream_output = StreamOutput::new(port_name);
                            stream_output.add_producer(*typed_producer);

                            Python::with_gil(|py| {
                                if let Some(instance) = &self.python_instance {
                                    let py_wrapper = PyStreamOutputAudio8::from_port(stream_output);
                                    instance.setattr(py, port_name, py_wrapper).is_ok()
                                } else {
                                    tracing::warn!("[PythonProcessor:{}] Cannot wire AudioFrame before instance created", self.name);
                                    false
                                }
                            })
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Failed to downcast AudioFrame producer for port '{}'", self.name, port_name);
                            false
                        }
                    }
                    _ => {
                        tracing::warn!(
                            "[PythonProcessor:{}] Unsupported audio channel count {} for port '{}'",
                            self.name,
                            channels,
                            port_name
                        );
                        false
                    }
                }
            }
            FrameType::Data => {
                if let Ok(typed_producer) =
                    producer.downcast::<crate::core::OwnedProducer<DataFrame>>()
                {
                    let stream_output = StreamOutput::new(port_name);
                    stream_output.add_producer(*typed_producer);

                    Python::with_gil(|py| {
                        if let Some(instance) = &self.python_instance {
                            let py_wrapper = PyStreamOutputData::from_port(stream_output);
                            instance.setattr(py, port_name, py_wrapper).is_ok()
                        } else {
                            tracing::warn!("[PythonProcessor:{}] Cannot wire DataFrame before instance created", self.name);
                            false
                        }
                    })
                } else {
                    tracing::warn!(
                        "[PythonProcessor:{}] Failed to downcast DataFrame producer for port '{}'",
                        self.name,
                        port_name
                    );
                    false
                }
            }
        };

        if result {
            tracing::info!(
                "[PythonProcessor:{}] Successfully wired output port '{}' (type: {:?})",
                self.name,
                port_name,
                port_meta.frame_type
            );
        }

        result
    }
}
