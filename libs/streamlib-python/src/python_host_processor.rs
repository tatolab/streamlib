// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Host processor for Python-defined processors.
//!
//! Loads Python projects and executes them via PyO3, bridging
//! input/output frames between Rust and Python. Each processor
//! instance runs in its own isolated virtual environment.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use streamlib::{
    GpuContext, LinkInput, LinkOutput, PortDescriptor, ProcessorDescriptor, ReactiveProcessor,
    Result, RuntimeContext, StreamError, VideoFrame,
};

use crate::frame_binding::PyFrame;
use crate::processor_context_proxy::{PortMetadata, PyProcessorContext};
use crate::venv_manager::VenvManager;

/// Configuration for PythonHostProcessor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, streamlib::ConfigDescriptor)]
pub struct PythonHostProcessorConfig {
    /// Path to the Python project directory (containing pyproject.toml or the processor script).
    pub project_path: PathBuf,

    /// Name of the processor class to instantiate (must have @processor decorator).
    pub class_name: String,

    /// Entry point file within the project (e.g., "processor.py" or "src/processor.py").
    /// If not specified, looks for a file matching the class name or "processor.py".
    pub entry_point: Option<String>,
}

impl Default for PythonHostProcessorConfig {
    fn default() -> Self {
        Self {
            project_path: PathBuf::from("."),
            class_name: "Processor".to_string(),
            entry_point: None,
        }
    }
}

/// Metadata extracted from Python @processor decorator.
#[derive(Debug, Clone)]
struct PythonProcessorMetadata {
    name: String,
    description: String,
    _inputs: Vec<PortDescriptor>,
    _outputs: Vec<PortDescriptor>,
}

/// Helper to convert PyErr to StreamError.
fn py_err_to_stream_error(e: PyErr, context: &str) -> StreamError {
    StreamError::Runtime(format!("{}: {}", context, e))
}

/// Generate a unique instance ID for venv isolation.
fn generate_instance_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let random: u32 = rand_simple();
    format!("py_{:x}_{:08x}", timestamp, random)
}

/// Simple random number generator (no external dependency).
fn rand_simple() -> u32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    RandomState::new().build_hasher().finish() as u32
}

/// Host processor for Python-defined processors.
///
/// This processor loads a Python project and executes it via PyO3.
/// The Python class must be decorated with `@processor` and define
/// input/output ports with `@input_port` and `@output_port`.
///
/// Each processor instance runs in its own isolated virtual environment,
/// created and managed automatically using `uv`.
#[streamlib::processor(
    execution = Reactive,
    description = "Host processor for Python-defined processors"
)]
pub struct PythonHostProcessor {
    #[streamlib::config]
    config: PythonHostProcessorConfig,

    #[streamlib::input(description = "Video input")]
    video_in: LinkInput<VideoFrame>,

    #[streamlib::output(description = "Video output")]
    video_out: Arc<LinkOutput<VideoFrame>>,

    // Venv manager (handles creation and cleanup)
    venv_manager: Option<VenvManager>,

    // Python runtime state
    py_instance: Option<Py<PyAny>>,
    py_context: Option<Py<PyProcessorContext>>,
    gpu_context: Option<Arc<GpuContext>>,
    metadata: Option<PythonProcessorMetadata>,
}

impl PythonHostProcessor::Processor {
    /// Resolve the entry point file path.
    fn resolve_entry_point(&self) -> Result<PathBuf> {
        let project_path = &self.config.project_path;

        // If entry_point is specified, use it
        if let Some(ref entry) = self.config.entry_point {
            let path = project_path.join(entry);
            if path.exists() {
                return Ok(path);
            }
            return Err(StreamError::Configuration(format!(
                "Entry point '{}' not found in project '{}'",
                entry,
                project_path.display()
            )));
        }

        // Try common patterns
        let candidates = [
            // Direct file in project root
            format!("{}.py", self.config.class_name.to_lowercase()),
            "processor.py".to_string(),
            "main.py".to_string(),
            // In src/ directory
            format!("src/{}.py", self.config.class_name.to_lowercase()),
            "src/processor.py".to_string(),
            "src/main.py".to_string(),
        ];

        for candidate in &candidates {
            let path = project_path.join(candidate);
            if path.exists() {
                tracing::debug!(
                    "PythonHostProcessor: Auto-discovered entry point: {}",
                    path.display()
                );
                return Ok(path);
            }
        }

        // Check if project_path itself is a .py file
        if project_path.extension().map(|e| e == "py").unwrap_or(false) && project_path.exists() {
            return Ok(project_path.clone());
        }

        Err(StreamError::Configuration(format!(
            "Could not find Python entry point in '{}'. Tried: {:?}",
            project_path.display(),
            candidates
        )))
    }

    /// Load the Python script and extract processor metadata.
    fn load_python_class(&mut self, site_packages: &PathBuf) -> Result<PythonProcessorMetadata> {
        Python::attach(|py| {
            // Add venv site-packages to sys.path
            let sys = py
                .import("sys")
                .map_err(|e| py_err_to_stream_error(e, "Failed to import sys"))?;
            let path_attr = sys
                .getattr("path")
                .map_err(|e| py_err_to_stream_error(e, "Failed to get sys.path"))?;
            let path: &Bound<'_, PyList> = path_attr
                .cast()
                .map_err(|_| StreamError::Configuration("sys.path is not a list".into()))?;

            // Insert site-packages at the beginning
            path.insert(0, site_packages.to_string_lossy().as_ref())
                .map_err(|e| py_err_to_stream_error(e, "Failed to insert site-packages"))?;

            tracing::debug!(
                "PythonHostProcessor: Added site-packages to sys.path: {:?}",
                site_packages
            );

            // Process .pth files in site-packages (editable installs use these)
            // .pth files are normally processed at interpreter startup, but since
            // PyO3 auto-initializes before we add site-packages, we must process them manually
            if let Ok(entries) = std::fs::read_dir(site_packages) {
                for entry in entries.flatten() {
                    let entry_path = entry.path();
                    if entry_path.extension().map(|e| e == "pth").unwrap_or(false) {
                        // Skip _virtualenv.pth (internal)
                        if entry_path
                            .file_name()
                            .map(|n| n.to_string_lossy().starts_with('_'))
                            .unwrap_or(false)
                        {
                            continue;
                        }

                        if let Ok(contents) = std::fs::read_to_string(&entry_path) {
                            for line in contents.lines() {
                                let line = line.trim();
                                // Skip empty lines and import statements
                                if line.is_empty()
                                    || line.starts_with('#')
                                    || line.starts_with("import ")
                                {
                                    continue;
                                }
                                // Add the path from .pth file
                                tracing::debug!(
                                    "PythonHostProcessor: Adding path from {}: {}",
                                    entry_path.file_name().unwrap_or_default().to_string_lossy(),
                                    line
                                );
                                path.insert(0, line).map_err(|e| {
                                    py_err_to_stream_error(e, "Failed to insert .pth path")
                                })?;
                            }
                        }
                    }
                }
            }

            // Resolve and load the entry point
            let script_path = self.resolve_entry_point()?;

            let script_content = std::fs::read_to_string(&script_path).map_err(|e| {
                StreamError::Configuration(format!(
                    "Failed to read Python script '{}': {}",
                    script_path.display(),
                    e
                ))
            })?;

            let code_cstr = CString::new(script_content.as_str()).map_err(|e| {
                StreamError::Configuration(format!("Invalid Python script (contains null): {}", e))
            })?;
            let file_name_cstr = CString::new(script_path.to_str().unwrap_or("processor.py"))
                .map_err(|e| {
                    StreamError::Configuration(format!("Invalid file path (contains null): {}", e))
                })?;

            let module =
                PyModule::from_code(py, &code_cstr, &file_name_cstr, c"streamlib_processor")
                    .map_err(|e| {
                        StreamError::Configuration(format!("Failed to load Python module: {}", e))
                    })?;

            // Get the processor class
            let py_class = module
                .getattr(self.config.class_name.as_str())
                .map_err(|e| {
                    StreamError::Configuration(format!(
                        "Processor class '{}' not found: {}",
                        self.config.class_name, e
                    ))
                })?;

            // Extract metadata from __streamlib_metadata__ attribute
            let metadata = self.extract_metadata(py, &py_class)?;

            // Instantiate the processor class
            let py_instance = py_class.call0().map_err(|e| {
                StreamError::Configuration(format!("Failed to instantiate processor class: {}", e))
            })?;

            self.py_instance = Some(py_instance.into());

            Ok(metadata)
        })
    }

    /// Extract metadata from Python class decorated with @processor.
    fn extract_metadata(
        &self,
        _py: Python<'_>,
        py_class: &Bound<'_, PyAny>,
    ) -> Result<PythonProcessorMetadata> {
        let metadata_attr = py_class.getattr("__streamlib_metadata__").map_err(|e| {
            StreamError::Configuration(format!(
                "Processor class missing @processor decorator: {}",
                e
            ))
        })?;

        let metadata_dict: &Bound<'_, PyDict> = metadata_attr.cast().map_err(|_| {
            StreamError::Configuration("Invalid __streamlib_metadata__ format".into())
        })?;

        let name: String = metadata_dict
            .get_item("name")
            .map_err(|e| py_err_to_stream_error(e, "Failed to get 'name'"))?
            .ok_or_else(|| StreamError::Configuration("Missing 'name' in metadata".into()))?
            .extract()
            .map_err(|e| py_err_to_stream_error(e, "Failed to extract 'name'"))?;

        let description: String = metadata_dict
            .get_item("description")
            .map_err(|e| py_err_to_stream_error(e, "Failed to get 'description'"))?
            .map(|v| v.extract().unwrap_or_default())
            .unwrap_or_default();

        // Extract input ports
        let inputs_item = metadata_dict
            .get_item("inputs")
            .map_err(|e| py_err_to_stream_error(e, "Failed to get 'inputs'"))?
            .ok_or_else(|| StreamError::Configuration("Missing 'inputs' in metadata".into()))?;

        let mut inputs = Vec::new();
        for item in inputs_item
            .try_iter()
            .map_err(|e| py_err_to_stream_error(e, "Failed to iterate inputs"))?
        {
            let port_item =
                item.map_err(|e| py_err_to_stream_error(e, "Failed to get input port"))?;
            let port_dict: &Bound<'_, PyDict> = port_item
                .cast()
                .map_err(|_| StreamError::Configuration("Input port is not a dict".into()))?;

            let port_name: String = port_dict
                .get_item("name")
                .map_err(|e| py_err_to_stream_error(e, "Failed to get port name"))?
                .ok_or_else(|| StreamError::Configuration("Missing port name".into()))?
                .extract()
                .map_err(|e| py_err_to_stream_error(e, "Failed to extract port name"))?;

            let port_desc: String = port_dict
                .get_item("description")
                .map_err(|e| py_err_to_stream_error(e, "Failed to get port description"))?
                .map(|v| v.extract().unwrap_or_default())
                .unwrap_or_default();

            // Try 'schema' first (new API), fall back to 'frame_type' (deprecated)
            let schema: Option<String> = port_dict
                .get_item("schema")
                .ok()
                .flatten()
                .and_then(|v| v.extract().ok());

            let frame_type: String = schema.unwrap_or_else(|| {
                port_dict
                    .get_item("frame_type")
                    .ok()
                    .flatten()
                    .and_then(|v| v.extract().ok())
                    .unwrap_or_else(|| "VideoFrame".to_string())
            });

            inputs.push(PortDescriptor::new(
                &port_name,
                &port_desc,
                &frame_type,
                true,
            ));
        }

        // Extract output ports
        let outputs_item = metadata_dict
            .get_item("outputs")
            .map_err(|e| py_err_to_stream_error(e, "Failed to get 'outputs'"))?
            .ok_or_else(|| StreamError::Configuration("Missing 'outputs' in metadata".into()))?;

        let mut outputs = Vec::new();
        for item in outputs_item
            .try_iter()
            .map_err(|e| py_err_to_stream_error(e, "Failed to iterate outputs"))?
        {
            let port_item =
                item.map_err(|e| py_err_to_stream_error(e, "Failed to get output port"))?;
            let port_dict: &Bound<'_, PyDict> = port_item
                .cast()
                .map_err(|_| StreamError::Configuration("Output port is not a dict".into()))?;

            let port_name: String = port_dict
                .get_item("name")
                .map_err(|e| py_err_to_stream_error(e, "Failed to get port name"))?
                .ok_or_else(|| StreamError::Configuration("Missing port name".into()))?
                .extract()
                .map_err(|e| py_err_to_stream_error(e, "Failed to extract port name"))?;

            let port_desc: String = port_dict
                .get_item("description")
                .map_err(|e| py_err_to_stream_error(e, "Failed to get port description"))?
                .map(|v| v.extract().unwrap_or_default())
                .unwrap_or_default();

            // Try 'schema' first (new API), fall back to 'frame_type' (deprecated)
            let schema: Option<String> = port_dict
                .get_item("schema")
                .ok()
                .flatten()
                .and_then(|v| v.extract().ok());

            let frame_type: String = schema.unwrap_or_else(|| {
                port_dict
                    .get_item("frame_type")
                    .ok()
                    .flatten()
                    .and_then(|v| v.extract().ok())
                    .unwrap_or_else(|| "VideoFrame".to_string())
            });

            outputs.push(PortDescriptor::new(
                &port_name,
                &port_desc,
                &frame_type,
                true,
            ));
        }

        tracing::info!(
            "PythonHostProcessor: Extracted metadata for '{}' ({} inputs, {} outputs)",
            name,
            inputs.len(),
            outputs.len()
        );

        Ok(PythonProcessorMetadata {
            name,
            description,
            _inputs: inputs,
            _outputs: outputs,
        })
    }
}

impl ReactiveProcessor for PythonHostProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        // Store GPU context
        self.gpu_context = Some(Arc::new(ctx.gpu.clone()));

        // Generate unique instance ID and create venv manager
        let instance_id = generate_instance_id();
        tracing::info!(
            "PythonHostProcessor: Setting up with instance ID '{}'",
            instance_id
        );

        let mut venv_manager = VenvManager::new(&instance_id)?;

        // Ensure venv exists (creates it, installs deps, injects streamlib)
        let venv_path = venv_manager.ensure_venv(&self.config.project_path)?;

        // Get site-packages path for Python
        let site_packages = venv_manager.get_site_packages(&venv_path)?;

        // Store venv manager for cleanup
        self.venv_manager = Some(venv_manager);

        // Load Python class
        self.metadata = Some(self.load_python_class(&site_packages)?);

        let metadata = self.metadata.as_ref().unwrap();
        tracing::info!(
            "PythonHostProcessor: Loaded '{}' from '{}'",
            metadata.name,
            self.config.project_path.display()
        );

        // Call Python setup() if it exists
        let gpu_context = self.gpu_context.clone().unwrap();
        Python::attach(|py| {
            if let Some(ref instance) = self.py_instance {
                let instance = instance.bind(py);

                // Create ProcessorContext for Python
                let py_ctx = PyProcessorContext::new(py, gpu_context.clone())
                    .map_err(|e| py_err_to_stream_error(e, "Failed to create ProcessorContext"))?;

                // Register input ports with schema metadata
                for input in &metadata._inputs {
                    py_ctx.register_input_port(PortMetadata {
                        name: input.name.clone(),
                        schema: Some(input.schema.clone()),
                        description: input.description.clone(),
                    });
                }

                // Register output ports with schema metadata
                for output in &metadata._outputs {
                    py_ctx.register_output_port(PortMetadata {
                        name: output.name.clone(),
                        schema: Some(output.schema.clone()),
                        description: output.description.clone(),
                    });
                }

                self.py_context = Some(
                    Py::new(py, py_ctx)
                        .map_err(|e| py_err_to_stream_error(e, "Failed to wrap context"))?,
                );

                // Call setup() if defined
                if instance
                    .hasattr("setup")
                    .map_err(|e| py_err_to_stream_error(e, "Failed to check hasattr"))?
                {
                    let ctx_ref = self.py_context.as_ref().unwrap().bind(py);
                    instance.call_method1("setup", (ctx_ref,)).map_err(|e| {
                        let traceback = e
                            .traceback(py)
                            .map(|tb| tb.format().unwrap_or_default())
                            .unwrap_or_default();
                        StreamError::Runtime(format!("Python setup() failed: {}\n{}", e, traceback))
                    })?;
                    tracing::debug!("PythonHostProcessor: Python setup() completed");
                }
            }
            Ok::<_, StreamError>(())
        })?;

        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        // Call Python teardown
        Python::attach(|py| {
            if let Some(ref instance) = self.py_instance {
                let instance = instance.bind(py);
                if instance.hasattr("teardown").unwrap_or(false) {
                    // Pass context to teardown if available
                    let result = if let Some(ref ctx) = self.py_context {
                        let ctx_ref = ctx.bind(py);
                        instance.call_method1("teardown", (ctx_ref,))
                    } else {
                        instance.call_method0("teardown")
                    };
                    if let Err(e) = result {
                        tracing::warn!("Python teardown() failed: {}", e);
                    }
                }
            }
        });

        let name = self
            .metadata
            .as_ref()
            .map(|m| m.name.as_str())
            .unwrap_or("unknown");

        // Clean up Python state
        self.py_instance = None;
        self.py_context = None;

        // Clean up venv (removes the directory)
        if let Some(ref mut venv_manager) = self.venv_manager {
            if let Err(e) = venv_manager.cleanup() {
                tracing::warn!("PythonHostProcessor: Venv cleanup failed: {}", e);
            }
        }
        self.venv_manager = None;

        tracing::info!("PythonHostProcessor: Teardown complete for '{}'", name);

        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Read input frame
        let input_frame = match self.video_in.read() {
            Some(frame) => frame,
            None => return Ok(()),
        };

        Python::attach(|py| {
            let instance = self
                .py_instance
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("Python instance not initialized".into()))?;

            let ctx = self
                .py_context
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("Python context not initialized".into()))?;

            // Set input frame on context using new API
            let ctx_borrowed = ctx.borrow(py);
            ctx_borrowed.set_input_frame(
                "video_in",
                Some(PyFrame::from_video_frame(input_frame.clone())),
            );

            // Call Python process()
            let ctx_ref = ctx.bind(py);
            instance
                .bind(py)
                .call_method1("process", (ctx_ref,))
                .map_err(|e| {
                    let traceback = e
                        .traceback(py)
                        .map(|tb| tb.format().unwrap_or_default())
                        .unwrap_or_default();
                    StreamError::Runtime(format!("Python process() failed: {}\n{}", e, traceback))
                })?;

            // Extract output frame using new API
            if let Some(py_frame) = ctx_borrowed.take_output_frame("video_out") {
                if let Some(video_frame) = py_frame.as_video_frame() {
                    self.video_out.write(video_frame.clone());
                }
            }

            Ok::<_, StreamError>(())
        })?;

        Ok(())
    }
}

impl PythonHostProcessor::Processor {
    /// Get the ProcessorDescriptor using the Python class name.
    pub fn descriptor_from_metadata(&self) -> Option<ProcessorDescriptor> {
        self.metadata
            .as_ref()
            .map(|m| ProcessorDescriptor::new(&m.name, &m.description).with_version("1.0.0"))
    }
}
