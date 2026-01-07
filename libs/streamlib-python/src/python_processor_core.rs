// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared core logic for Python host processors.
//!
//! This module contains the common infrastructure used by all Python host
//! processor variants (Reactive, Continuous, Manual).

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Arc;

use streamlib::{
    core::links::LinkBufferReadMode,
    core::schema::{DataFrameSchemaField, PrimitiveType, SemanticVersion},
    core::schema_registry::SCHEMA_REGISTRY,
    core::RuntimeContext,
    GpuContext, PortDescriptor, Result, StreamError, TimeContext, VideoFrame,
};

use crate::frame_binding::PyFrame;
use crate::processor_context_proxy::{PortMetadata, PyProcessorContext};
use crate::venv_manager::VenvManager;

/// Configuration for Python host processors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, streamlib::ConfigDescriptor)]
pub struct PythonProcessorConfig {
    /// Path to the Python project directory (containing pyproject.toml or the processor script).
    pub project_path: PathBuf,

    /// Name of the processor class to instantiate (must have @processor decorator).
    pub class_name: String,

    /// Entry point file within the project (e.g., "processor.py" or "src/processor.py").
    /// If not specified, looks for a file matching the class name or "processor.py".
    pub entry_point: Option<String>,
}

impl Default for PythonProcessorConfig {
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
pub struct PythonProcessorMetadata {
    pub name: String,
    pub description: String,
    pub execution: String,
    pub inputs: Vec<PortDescriptor>,
    pub outputs: Vec<PortDescriptor>,
}

/// Convert PyErr to StreamError.
pub fn py_err_to_stream_error(e: PyErr, context: &str) -> StreamError {
    StreamError::Runtime(format!("{}: {}", context, e))
}

/// Shared core for Python host processors.
///
/// Contains all the common state and methods used by the three execution
/// mode variants: Reactive, Continuous, and Manual.
#[derive(Default)]
pub struct PythonProcessorCore {
    pub config: PythonProcessorConfig,
    pub venv_manager: Option<VenvManager>,
    pub py_instance: Option<Py<PyAny>>,
    pub py_context: Option<Py<PyProcessorContext>>,
    pub gpu_context: Option<Arc<GpuContext>>,
    pub time_context: Option<Arc<TimeContext>>,
    pub metadata: Option<PythonProcessorMetadata>,
}

impl PythonProcessorCore {
    /// Create a new core with the given config.
    pub fn new(config: PythonProcessorConfig) -> Self {
        Self {
            config,
            venv_manager: None,
            py_instance: None,
            py_context: None,
            gpu_context: None,
            time_context: None,
            metadata: None,
        }
    }

    /// Resolve the entry point file path.
    pub fn resolve_entry_point(&self) -> Result<PathBuf> {
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
                    "PythonProcessorCore: Auto-discovered entry point: {}",
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
    pub fn load_python_class(
        &mut self,
        site_packages: &PathBuf,
    ) -> Result<PythonProcessorMetadata> {
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
                "PythonProcessorCore: Added site-packages to sys.path: {:?}",
                site_packages
            );

            // Process .pth files in site-packages (editable installs use these)
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
                                if line.is_empty()
                                    || line.starts_with('#')
                                    || line.starts_with("import ")
                                {
                                    continue;
                                }
                                tracing::debug!(
                                    "PythonProcessorCore: Adding path from {}: {}",
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

            // Register any @schema-decorated classes in SCHEMA_REGISTRY
            if let Err(e) = self.register_python_schemas(py, &module) {
                tracing::warn!(
                    "PythonProcessorCore: Failed to register Python schemas: {}",
                    e
                );
            }

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
    pub fn extract_metadata(
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

        // Extract execution mode (defaults to "Reactive")
        let execution: String = metadata_dict
            .get_item("execution")
            .map_err(|e| py_err_to_stream_error(e, "Failed to get 'execution'"))?
            .map(|v| v.extract().unwrap_or_else(|_| "Reactive".to_string()))
            .unwrap_or_else(|| "Reactive".to_string());

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
            "PythonProcessorCore: Extracted metadata for '{}' (execution={}, {} inputs, {} outputs)",
            name,
            execution,
            inputs.len(),
            outputs.len()
        );

        Ok(PythonProcessorMetadata {
            name,
            description,
            execution,
            inputs,
            outputs,
        })
    }

    /// Scan Python module for @schema-decorated classes and register them in SCHEMA_REGISTRY.
    pub fn register_python_schemas(
        &self,
        py: Python<'_>,
        module: &Bound<'_, PyModule>,
    ) -> Result<()> {
        let builtins = py
            .import("builtins")
            .map_err(|e| py_err_to_stream_error(e, "Failed to import builtins"))?;
        let dir_fn = builtins
            .getattr("dir")
            .map_err(|e| py_err_to_stream_error(e, "Failed to get dir function"))?;
        let names = dir_fn
            .call1((module,))
            .map_err(|e| py_err_to_stream_error(e, "Failed to call dir()"))?;

        for name in names
            .try_iter()
            .map_err(|e| py_err_to_stream_error(e, "Failed to iterate names"))?
        {
            let name = name.map_err(|e| py_err_to_stream_error(e, "Failed to get name"))?;
            let name_str: String = name
                .extract()
                .map_err(|e| py_err_to_stream_error(e, "Failed to extract name"))?;

            if name_str.starts_with('_') {
                continue;
            }

            let attr = match module.getattr(name_str.as_str()) {
                Ok(attr) => attr,
                Err(_) => continue,
            };

            let schema_attr = match attr.getattr("__streamlib_schema__") {
                Ok(schema) => schema,
                Err(_) => continue,
            };

            if let Err(e) = self.register_schema_from_python(py, &name_str, &schema_attr) {
                tracing::warn!(
                    "PythonProcessorCore: Failed to register schema '{}': {}",
                    name_str,
                    e
                );
            }
        }

        Ok(())
    }

    /// Register a single schema from Python metadata in SCHEMA_REGISTRY.
    pub fn register_schema_from_python(
        &self,
        _py: Python<'_>,
        class_name: &str,
        schema_attr: &Bound<'_, PyAny>,
    ) -> Result<()> {
        let schema_dict: &Bound<'_, PyDict> = schema_attr.cast().map_err(|_| {
            StreamError::Configuration(format!(
                "Invalid __streamlib_schema__ format on class '{}'",
                class_name
            ))
        })?;

        let schema_name: String = schema_dict
            .get_item("name")
            .map_err(|e| py_err_to_stream_error(e, "Failed to get schema name"))?
            .ok_or_else(|| StreamError::Configuration("Missing 'name' in schema metadata".into()))?
            .extract()
            .map_err(|e| py_err_to_stream_error(e, "Failed to extract schema name"))?;

        let fields_list = schema_dict
            .get_item("fields")
            .map_err(|e| py_err_to_stream_error(e, "Failed to get schema fields"))?
            .ok_or_else(|| {
                StreamError::Configuration("Missing 'fields' in schema metadata".into())
            })?;

        let mut schema_fields = Vec::new();

        for field in fields_list
            .try_iter()
            .map_err(|e| py_err_to_stream_error(e, "Failed to iterate fields"))?
        {
            let field = field.map_err(|e| py_err_to_stream_error(e, "Failed to get field"))?;
            let field_dict: &Bound<'_, PyDict> = field
                .cast()
                .map_err(|_| StreamError::Configuration("Field is not a dict".into()))?;

            let field_name: String = field_dict
                .get_item("name")
                .map_err(|e| py_err_to_stream_error(e, "Failed to get field name"))?
                .ok_or_else(|| StreamError::Configuration("Missing field name".into()))?
                .extract()
                .map_err(|e| py_err_to_stream_error(e, "Failed to extract field name"))?;

            let primitive_type_str: String = field_dict
                .get_item("primitive_type")
                .map_err(|e| py_err_to_stream_error(e, "Failed to get primitive_type"))?
                .ok_or_else(|| StreamError::Configuration("Missing primitive_type".into()))?
                .extract()
                .map_err(|e| py_err_to_stream_error(e, "Failed to extract primitive_type"))?;

            let shape: Vec<usize> = field_dict
                .get_item("shape")
                .map_err(|e| py_err_to_stream_error(e, "Failed to get shape"))?
                .map(|v| v.extract().unwrap_or_default())
                .unwrap_or_default();

            let description: String = field_dict
                .get_item("description")
                .map_err(|e| py_err_to_stream_error(e, "Failed to get description"))?
                .map(|v| v.extract().unwrap_or_default())
                .unwrap_or_default();

            let primitive = match primitive_type_str.as_str() {
                "bool" => PrimitiveType::Bool,
                "i32" => PrimitiveType::I32,
                "i64" => PrimitiveType::I64,
                "u32" => PrimitiveType::U32,
                "u64" => PrimitiveType::U64,
                "f32" => PrimitiveType::F32,
                "f64" => PrimitiveType::F64,
                other => {
                    return Err(StreamError::Configuration(format!(
                        "Unknown primitive type '{}' in schema field '{}'",
                        other, field_name
                    )));
                }
            };

            schema_fields.push(DataFrameSchemaField {
                name: field_name,
                description,
                type_name: primitive_type_str,
                shape,
                internal: false,
                primitive: Some(primitive),
            });
        }

        if SCHEMA_REGISTRY.contains(&schema_name) {
            tracing::debug!(
                "PythonProcessorCore: Schema '{}' already registered, skipping",
                schema_name
            );
            return Ok(());
        }

        SCHEMA_REGISTRY
            .register_dataframe_schema(
                schema_name.clone(),
                SemanticVersion::new(1, 0, 0),
                schema_fields.clone(),
                LinkBufferReadMode::SkipToLatest,
                16,
            )
            .map_err(|e| {
                StreamError::Configuration(format!(
                    "Failed to register schema '{}': {}",
                    schema_name, e
                ))
            })?;

        tracing::info!(
            "PythonProcessorCore: Registered schema '{}' with {} fields",
            schema_name,
            schema_fields.len()
        );

        Ok(())
    }

    /// Common setup logic for all Python host processors.
    pub fn setup_common(
        &mut self,
        config: PythonProcessorConfig,
        ctx: &RuntimeContext,
    ) -> Result<()> {
        self.config = config;
        self.gpu_context = Some(Arc::new(ctx.gpu.clone()));
        self.time_context = Some(Arc::clone(&ctx.time));

        // Get IDs from RuntimeContext
        let runtime_id = ctx.runtime_id();
        let processor_id = ctx.processor_id().ok_or_else(|| {
            StreamError::Runtime(
                "processor_id not set on RuntimeContext. \
                 Did spawn_processor_op call with_processor_id()?"
                    .into(),
            )
        })?;

        tracing::info!(
            "PythonProcessorCore: Setting up processor {} in runtime {}",
            processor_id.as_str(),
            runtime_id.as_str()
        );

        let mut venv_manager = VenvManager::new(runtime_id, processor_id)?;
        let venv_path = venv_manager.ensure_venv(&self.config.project_path)?;
        let site_packages = venv_manager.get_site_packages(&venv_path)?;

        self.venv_manager = Some(venv_manager);
        self.metadata = Some(self.load_python_class(&site_packages)?);

        let metadata = self.metadata.as_ref().unwrap();
        tracing::info!(
            "PythonProcessorCore: Loaded '{}' from '{}'",
            metadata.name,
            self.config.project_path.display()
        );

        Ok(())
    }

    /// Initialize Python context and call Python setup() if defined.
    pub fn init_python_context(&mut self) -> Result<()> {
        let processor_name = self
            .metadata
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_else(|| "unknown".to_string());

        tracing::info!(
            "[{}] setup() ENTER - initializing Python context",
            processor_name
        );

        let gpu_context = self
            .gpu_context
            .clone()
            .ok_or_else(|| StreamError::Runtime("GPU context not initialized".into()))?;
        let time_context = self
            .time_context
            .clone()
            .ok_or_else(|| StreamError::Runtime("Time context not initialized".into()))?;
        let metadata = self
            .metadata
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("Metadata not loaded".into()))?;

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Python::attach(|py| {
                if let Some(ref instance) = self.py_instance {
                    let instance = instance.bind(py);

                    tracing::debug!("[{}] setup() creating ProcessorContext", processor_name);
                    let py_ctx =
                        PyProcessorContext::new(py, gpu_context.clone(), time_context.clone())
                            .map_err(|e| {
                                py_err_to_stream_error(e, "Failed to create ProcessorContext")
                            })?;

                    // Register input ports with schema metadata
                    for input in &metadata.inputs {
                        py_ctx.register_input_port(PortMetadata {
                            name: input.name.clone(),
                            schema: Some(input.schema.clone()),
                            description: input.description.clone(),
                        });
                    }

                    // Register output ports with schema metadata
                    for output in &metadata.outputs {
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
                        tracing::info!("[{}] setup() calling Python setup()", processor_name);
                        let ctx_ref = self.py_context.as_ref().unwrap().bind(py);
                        instance.call_method1("setup", (ctx_ref,)).map_err(|e| {
                            let traceback = e
                                .traceback(py)
                                .map(|tb| tb.format().unwrap_or_default())
                                .unwrap_or_default();
                            tracing::error!(
                                "[{}] setup() Python exception: {}\n{}",
                                processor_name,
                                e,
                                traceback
                            );
                            StreamError::Runtime(format!(
                                "Python setup() failed: {}\n{}",
                                e, traceback
                            ))
                        })?;
                        tracing::info!("[{}] setup() Python setup() completed", processor_name);
                    }
                }
                Ok::<_, StreamError>(())
            })
        }));

        match result {
            Ok(inner_result) => {
                tracing::info!("[{}] setup() EXIT", processor_name);
                inner_result
            }
            Err(panic_info) => {
                let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                tracing::error!("[{}] setup() PANIC: {}", processor_name, panic_msg);
                Err(StreamError::Runtime(format!(
                    "Python setup() panicked: {}",
                    panic_msg
                )))
            }
        }
    }

    /// Common teardown logic for all Python host processors.
    pub fn teardown_common(&mut self) -> Result<()> {
        // Call Python teardown
        Python::attach(|py| {
            if let Some(ref instance) = self.py_instance {
                let instance = instance.bind(py);
                if instance.hasattr("teardown").unwrap_or(false) {
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

        self.py_instance = None;
        self.py_context = None;

        if let Some(ref mut venv_manager) = self.venv_manager {
            if let Err(e) = venv_manager.cleanup() {
                tracing::warn!("PythonProcessorCore: Venv cleanup failed: {}", e);
            }
        }
        self.venv_manager = None;

        tracing::info!("PythonProcessorCore: Teardown complete for '{}'", name);

        Ok(())
    }

    /// Call Python process() method.
    pub fn call_python_process(
        &self,
        input_frame: Option<VideoFrame>,
    ) -> Result<Option<VideoFrame>> {
        let processor_name = self
            .metadata
            .as_ref()
            .map(|m| m.name.as_str())
            .unwrap_or("unknown");

        tracing::trace!("[{}] process() ENTER", processor_name);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Python::attach(|py| {
                let instance = self.py_instance.as_ref().ok_or_else(|| {
                    StreamError::Runtime("Python instance not initialized".into())
                })?;

                let ctx = self
                    .py_context
                    .as_ref()
                    .ok_or_else(|| StreamError::Runtime("Python context not initialized".into()))?;

                // Set input frame on context
                tracing::trace!("[{}] process() setting input frame", processor_name);
                let ctx_borrowed = ctx.borrow(py);
                if let Some(frame) = input_frame {
                    tracing::trace!(
                        "[{}] process() input frame: {}x{} format={:?}",
                        processor_name,
                        frame.width(),
                        frame.height(),
                        frame.pixel_format()
                    );
                    ctx_borrowed
                        .set_input_frame("video_in", Some(PyFrame::from_video_frame(frame)));
                } else {
                    ctx_borrowed.set_input_frame("video_in", None);
                }

                // Call Python process()
                tracing::trace!("[{}] process() calling Python", processor_name);
                let ctx_ref = ctx.bind(py);
                instance
                    .bind(py)
                    .call_method1("process", (ctx_ref,))
                    .map_err(|e| {
                        let traceback = e
                            .traceback(py)
                            .map(|tb| tb.format().unwrap_or_default())
                            .unwrap_or_default();
                        tracing::error!(
                            "[{}] process() Python exception: {}\n{}",
                            processor_name,
                            e,
                            traceback
                        );
                        StreamError::Runtime(format!(
                            "Python process() failed: {}\n{}",
                            e, traceback
                        ))
                    })?;
                tracing::trace!("[{}] process() Python returned", processor_name);

                // Extract output frame
                let output = if let Some(py_frame) = ctx_borrowed.take_output_frame("video_out") {
                    tracing::trace!("[{}] process() got output frame", processor_name);
                    py_frame.as_video_frame().cloned()
                } else {
                    tracing::trace!("[{}] process() no output frame", processor_name);
                    None
                };

                Ok::<_, StreamError>(output)
            })
        }));

        match result {
            Ok(inner_result) => {
                tracing::trace!("[{}] process() EXIT", processor_name);
                inner_result
            }
            Err(panic_info) => {
                let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
                tracing::error!("[{}] process() PANIC: {}", processor_name, panic_msg);
                Err(StreamError::Runtime(format!(
                    "Python process() panicked: {}",
                    panic_msg
                )))
            }
        }
    }

    /// Call Python start() method (for Manual mode).
    pub fn call_python_start(&self) -> Result<()> {
        Python::attach(|py| {
            let instance = self
                .py_instance
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("Python instance not initialized".into()))?;

            let ctx = self
                .py_context
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("Python context not initialized".into()))?;

            let instance = instance.bind(py);

            if !instance
                .hasattr("start")
                .map_err(|e| py_err_to_stream_error(e, "hasattr check"))?
            {
                return Err(StreamError::Runtime(
                    "Python processor missing start() method for Manual mode".into(),
                ));
            }

            let ctx_ref = ctx.bind(py);
            instance.call_method1("start", (ctx_ref,)).map_err(|e| {
                let traceback = e
                    .traceback(py)
                    .map(|tb| tb.format().unwrap_or_default())
                    .unwrap_or_default();
                StreamError::Runtime(format!("Python start() failed: {}\n{}", e, traceback))
            })?;

            tracing::debug!("PythonProcessorCore: Python start() completed");
            Ok::<_, StreamError>(())
        })
    }

    /// Call Python stop() method (for Manual mode).
    pub fn call_python_stop(&self) -> Result<()> {
        Python::attach(|py| {
            let instance = self
                .py_instance
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("Python instance not initialized".into()))?;

            let instance = instance.bind(py);

            // stop() is optional - if not defined, just return Ok
            if !instance
                .hasattr("stop")
                .map_err(|e| py_err_to_stream_error(e, "hasattr check"))?
            {
                tracing::debug!("PythonProcessorCore: Python processor has no stop() method");
                return Ok(());
            }

            let ctx = self.py_context.as_ref();
            let result = if let Some(ctx) = ctx {
                let ctx_ref = ctx.bind(py);
                instance.call_method1("stop", (ctx_ref,))
            } else {
                instance.call_method0("stop")
            };

            result.map_err(|e| {
                let traceback = e
                    .traceback(py)
                    .map(|tb| tb.format().unwrap_or_default())
                    .unwrap_or_default();
                StreamError::Runtime(format!("Python stop() failed: {}\n{}", e, traceback))
            })?;

            tracing::debug!("PythonProcessorCore: Python stop() completed");
            Ok::<_, StreamError>(())
        })
    }

    /// Call Python on_pause() method if defined.
    pub fn call_python_on_pause(&self) -> Result<()> {
        Python::attach(|py| {
            let instance = self
                .py_instance
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("Python instance not initialized".into()))?;

            let instance = instance.bind(py);

            // on_pause() is optional
            if !instance
                .hasattr("on_pause")
                .map_err(|e| py_err_to_stream_error(e, "hasattr check"))?
            {
                return Ok(());
            }

            let ctx = self.py_context.as_ref();
            let result = if let Some(ctx) = ctx {
                let ctx_ref = ctx.bind(py);
                instance.call_method1("on_pause", (ctx_ref,))
            } else {
                instance.call_method0("on_pause")
            };

            result.map_err(|e| {
                let traceback = e
                    .traceback(py)
                    .map(|tb| tb.format().unwrap_or_default())
                    .unwrap_or_default();
                StreamError::Runtime(format!("Python on_pause() failed: {}\n{}", e, traceback))
            })?;

            tracing::debug!("PythonProcessorCore: Python on_pause() completed");
            Ok::<_, StreamError>(())
        })
    }

    /// Call Python on_resume() method if defined.
    pub fn call_python_on_resume(&self) -> Result<()> {
        Python::attach(|py| {
            let instance = self
                .py_instance
                .as_ref()
                .ok_or_else(|| StreamError::Runtime("Python instance not initialized".into()))?;

            let instance = instance.bind(py);

            // on_resume() is optional
            if !instance
                .hasattr("on_resume")
                .map_err(|e| py_err_to_stream_error(e, "hasattr check"))?
            {
                return Ok(());
            }

            let ctx = self.py_context.as_ref();
            let result = if let Some(ctx) = ctx {
                let ctx_ref = ctx.bind(py);
                instance.call_method1("on_resume", (ctx_ref,))
            } else {
                instance.call_method0("on_resume")
            };

            result.map_err(|e| {
                let traceback = e
                    .traceback(py)
                    .map(|tb| tb.format().unwrap_or_default())
                    .unwrap_or_default();
                StreamError::Runtime(format!("Python on_resume() failed: {}\n{}", e, traceback))
            })?;

            tracing::debug!("PythonProcessorCore: Python on_resume() completed");
            Ok::<_, StreamError>(())
        })
    }
}
