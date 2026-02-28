// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;

use crate::core::error::{Result, StreamError};
use crate::core::execution::ExecutionConfig;
use crate::core::graph::ProcessorNode;
use crate::core::processors::{DynamicProcessorConstructorFn, ProcessorInstance};
use crate::core::runtime::BoxFuture;
use crate::core::{ProcessorDescriptor, RuntimeContext};

use super::spawn_python_subprocess_op::ensure_processor_venv;

// ============================================================================
// PythonNativeSubprocessHostProcessor — Rust host for Python native-mode processors
// ============================================================================

/// Rust-side host processor for Python subprocess processors using native FFI.
///
/// Like [`DenoSubprocessHostProcessor`], this processor has NO [`InputMailboxes`]
/// or [`OutputWriter`]. The Python subprocess manages its own iceoryx2 I/O
/// directly via FFI to `libstreamlib_python_native.dylib`.
///
/// The Rust host is purely a lifecycle manager:
/// - Spawns Python subprocess with the subprocess runner
/// - Sends lifecycle commands (setup, run, stop, teardown) via stdin/stdout pipes
/// - Always runs in Manual execution mode on the Rust side
pub(crate) struct PythonNativeSubprocessHostProcessor {
    // NO InputMailboxes — subprocess manages its own iceoryx2 subscribers
    // NO OutputWriter — subprocess manages its own iceoryx2 publishers

    // Subprocess management (populated during setup)
    child: Option<Child>,
    stdin_writer: Option<BufWriter<ChildStdin>>,
    stdout_reader: Option<BufReader<ChildStdout>>,

    // RuntimeContext for runtime ID access
    runtime_context: Option<RuntimeContext>,

    // Config for spawning (set at construction, used during setup)
    entrypoint: String,
    project_path: String,
    processor_id: String,
    processor_config: Option<serde_json::Value>,
    execution_config: ExecutionConfig,
    descriptor_name: String,

    // Set true when bridge communication fails (broken pipe) to avoid
    // spamming errors in the thread runner's tight poll loop.
    subprocess_dead: bool,

    // Path to libstreamlib_python_native.dylib
    native_lib_path: String,

    // Port wiring info populated by the compiler's iceoryx2 service wiring phase.
    // Filled BEFORE setup() is called. Passed to the Python subprocess in the setup command.
    pub(crate) input_port_wiring: Vec<serde_json::Value>,
    pub(crate) output_port_wiring: Vec<serde_json::Value>,
}

// ============================================================================
// DynGeneratedProcessor implementation
// ============================================================================

impl crate::core::processors::DynGeneratedProcessor for PythonNativeSubprocessHostProcessor {
    fn __generated_setup(&mut self, ctx: RuntimeContext) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.runtime_context = Some(ctx.clone());

            let project_path = PathBuf::from(&self.project_path);

            tracing::info!(
                "[{}] Setting up Python native subprocess host: entrypoint='{}', project_path='{}'",
                self.processor_id,
                self.entrypoint,
                self.project_path
            );

            // Create venv and get python executable path (reuse existing function)
            let python_executable = ensure_processor_venv(&self.processor_id, &project_path)?;

            // Build PYTHONPATH for the subprocess.
            let streamlib_python_source =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../streamlib-python/python");

            let mut python_path_parts: Vec<String> = Vec::new();
            if streamlib_python_source.exists() {
                if let Ok(canonical) = streamlib_python_source.canonicalize() {
                    python_path_parts.push(canonical.to_string_lossy().to_string());
                }
            }
            if !project_path.as_os_str().is_empty() {
                python_path_parts.push(project_path.to_string_lossy().to_string());
            }
            if let Ok(existing) = std::env::var("PYTHONPATH") {
                if !existing.is_empty() {
                    python_path_parts.push(existing);
                }
            }
            let python_path = python_path_parts.join(if cfg!(unix) { ":" } else { ";" });

            // Determine the original execution mode for the subprocess
            let execution_mode = match &self.execution_config.execution {
                crate::core::execution::ProcessExecution::Reactive => "reactive",
                crate::core::execution::ProcessExecution::Continuous { .. } => "continuous",
                crate::core::execution::ProcessExecution::Manual => "manual",
            };

            tracing::info!(
                "[{}] Spawning Python native subprocess: native_lib='{}'",
                self.processor_id,
                self.native_lib_path
            );

            let mut child = Command::new(&python_executable)
                .arg("-m")
                .arg("streamlib.subprocess_runner")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .env_remove("PYTHONHOME")
                .env("PYTHONPATH", &python_path)
                .env("STREAMLIB_ENTRYPOINT", &self.entrypoint)
                .env("STREAMLIB_PROJECT_PATH", &self.project_path)
                .env("STREAMLIB_PYTHON_NATIVE_LIB", &self.native_lib_path)
                .env("STREAMLIB_PROCESSOR_ID", &self.processor_id)
                .env("STREAMLIB_EXECUTION_MODE", execution_mode)
                .spawn()
                .map_err(|e| {
                    StreamError::Runtime(format!(
                        "Failed to spawn Python native subprocess for '{}': {}. Python: '{}'",
                        self.processor_id, e, python_executable
                    ))
                })?;

            let child_pid = child.id();
            tracing::info!(
                "[{}] Python native subprocess spawned: pid={}",
                self.processor_id,
                child_pid
            );

            let stdin = child.stdin.take().ok_or_else(|| {
                StreamError::Runtime("Failed to capture subprocess stdin".to_string())
            })?;
            let stdout = child.stdout.take().ok_or_else(|| {
                StreamError::Runtime("Failed to capture subprocess stdout".to_string())
            })?;

            self.child = Some(child);
            self.stdin_writer = Some(BufWriter::new(stdin));
            self.stdout_reader = Some(BufReader::new(stdout));

            // Send setup command with processor config and port wiring info
            let config = self
                .processor_config
                .clone()
                .unwrap_or(serde_json::Value::Null);
            self.bridge_send_json(&serde_json::json!({
                "cmd": "setup",
                "config": config,
                "processor_id": self.processor_id,
                "ports": {
                    "inputs": self.input_port_wiring,
                    "outputs": self.output_port_wiring,
                },
            }))?;

            // Wait for "ready" response
            let response = self.bridge_read_json()?;
            let rpc = response.get("rpc").and_then(|v| v.as_str()).unwrap_or("");
            if rpc != "ready" {
                let error = response
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(StreamError::Runtime(format!(
                    "Python native subprocess '{}' setup failed: {}",
                    self.processor_id, error
                )));
            }

            tracing::info!(
                "[{}] Python native subprocess setup complete (pid={})",
                self.processor_id,
                child_pid
            );

            Ok(())
        })
    }

    fn __generated_teardown(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            tracing::info!(
                "[{}] Tearing down Python native subprocess",
                self.processor_id
            );

            // Send teardown command (best-effort)
            if self.stdin_writer.is_some() {
                if let Err(e) = self.bridge_send_json(&serde_json::json!({"cmd": "teardown"})) {
                    tracing::warn!(
                        "[{}] Failed to send teardown command: {}",
                        self.processor_id,
                        e
                    );
                } else {
                    // Wait for done (with timeout via read)
                    match self.bridge_read_json() {
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(
                                "[{}] Failed to read teardown response: {}",
                                self.processor_id,
                                e
                            );
                        }
                    }
                }
            }

            // Drop pipes to signal EOF
            self.stdin_writer = None;
            self.stdout_reader = None;

            // Wait for subprocess to exit (with timeout)
            if let Some(mut child) = self.child.take() {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            tracing::info!(
                                "[{}] Python native subprocess exited: {}",
                                self.processor_id,
                                status
                            );
                            break;
                        }
                        Ok(None) if std::time::Instant::now() < deadline => {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        _ => {
                            tracing::warn!(
                                "[{}] Python native subprocess did not exit, killing",
                                self.processor_id
                            );
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                    }
                }
            }

            Ok(())
        })
    }

    fn __generated_on_pause(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            if self.subprocess_dead {
                return Ok(());
            }
            if let Err(e) = self.bridge_send_json(&serde_json::json!({"cmd": "on_pause"})) {
                tracing::warn!("[{}] Failed to send on_pause: {}", self.processor_id, e);
                self.subprocess_dead = true;
                return Ok(());
            }
            match self.bridge_read_json() {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        "[{}] Failed to read on_pause response: {}",
                        self.processor_id,
                        e
                    );
                    self.subprocess_dead = true;
                }
            }
            Ok(())
        })
    }

    fn __generated_on_resume(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            if self.subprocess_dead {
                return Ok(());
            }
            if let Err(e) = self.bridge_send_json(&serde_json::json!({"cmd": "on_resume"})) {
                tracing::warn!("[{}] Failed to send on_resume: {}", self.processor_id, e);
                self.subprocess_dead = true;
                return Ok(());
            }
            match self.bridge_read_json() {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        "[{}] Failed to read on_resume response: {}",
                        self.processor_id,
                        e
                    );
                    self.subprocess_dead = true;
                }
            }
            Ok(())
        })
    }

    fn process(&mut self) -> Result<()> {
        // Python native subprocess manages its own iceoryx2 I/O via FFI.
        // The Rust host runs in Manual mode — process() is never called
        // by the thread runner for Manual processors.
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        if self.subprocess_dead {
            return Ok(());
        }

        // Determine original execution mode from the stored config
        let execution_mode = match &self.execution_config.execution {
            crate::core::execution::ProcessExecution::Reactive => "reactive",
            crate::core::execution::ProcessExecution::Continuous { .. } => "continuous",
            crate::core::execution::ProcessExecution::Manual => "manual",
        };

        let interval_ms = self.execution_config.execution.interval_ms().unwrap_or(0);

        self.bridge_send_json(&serde_json::json!({
            "cmd": "run",
            "execution": execution_mode,
            "interval_ms": interval_ms,
        }))?;

        // "run" enters a loop in the subprocess — no immediate response expected.
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if self.subprocess_dead {
            return Ok(());
        }
        if let Err(e) = self.bridge_send_json(&serde_json::json!({"cmd": "stop"})) {
            tracing::warn!(
                "[{}] Subprocess pipe broken on stop: {}",
                self.processor_id,
                e
            );
            self.subprocess_dead = true;
            return Ok(());
        }
        match self.bridge_read_json() {
            Ok(response) => {
                let rpc = response.get("rpc").and_then(|v| v.as_str()).unwrap_or("");
                if rpc != "stopped" {
                    let error = response
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    tracing::warn!(
                        "[{}] Python native stop() error: {}",
                        self.processor_id,
                        error
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    "[{}] Failed to read stop response: {}",
                    self.processor_id,
                    e
                );
                self.subprocess_dead = true;
            }
        }
        Ok(())
    }

    fn name(&self) -> &str {
        &self.descriptor_name
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        None
    }

    fn execution_config(&self) -> ExecutionConfig {
        // Always Manual on the Rust side — the Python subprocess manages its own loop
        ExecutionConfig::new(crate::core::execution::ProcessExecution::Manual)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        false
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        false
    }

    fn get_iceoryx2_output_writer(&self) -> Option<Arc<crate::iceoryx2::OutputWriter>> {
        None
    }

    fn get_iceoryx2_input_mailboxes(&mut self) -> Option<&mut crate::iceoryx2::InputMailboxes> {
        None
    }

    fn apply_config_json(&mut self, _config_json: &serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        serde_json::json!({
            "subprocess_pid": self.child.as_ref().map(|c| c.id()),
            "entrypoint": self.entrypoint,
            "project_path": self.project_path,
            "runtime": "python-native",
        })
    }

    fn config_json(&self) -> serde_json::Value {
        self.processor_config
            .clone()
            .unwrap_or(serde_json::Value::Null)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn get_audio_converter_status_arc(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::core::utils::ProcessorAudioConverterStatus>>>
    {
        None
    }
}

// ============================================================================
// Bridge protocol helpers (length-prefixed JSON, same as Deno subprocess)
// ============================================================================

impl PythonNativeSubprocessHostProcessor {
    /// Send a length-prefixed JSON message to the subprocess stdin.
    fn bridge_send_json(&mut self, msg: &serde_json::Value) -> Result<()> {
        let writer = self
            .stdin_writer
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Subprocess stdin not available".to_string()))?;

        let json_bytes = serde_json::to_vec(msg).map_err(|e| {
            StreamError::Runtime(format!("Failed to serialize bridge message: {}", e))
        })?;

        let len = json_bytes.len() as u32;
        writer.write_all(&len.to_be_bytes()).map_err(|e| {
            StreamError::Runtime(format!("Failed to write to subprocess stdin: {}", e))
        })?;
        writer.write_all(&json_bytes).map_err(|e| {
            StreamError::Runtime(format!("Failed to write to subprocess stdin: {}", e))
        })?;
        writer.flush().map_err(|e| {
            StreamError::Runtime(format!("Failed to flush subprocess stdin: {}", e))
        })?;

        Ok(())
    }

    /// Read a length-prefixed JSON message from the subprocess stdout.
    fn bridge_read_json(&mut self) -> Result<serde_json::Value> {
        let reader = self
            .stdout_reader
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Subprocess stdout not available".to_string()))?;

        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf).map_err(|e| {
            StreamError::Runtime(format!("Failed to read from subprocess stdout: {}", e))
        })?;

        let len = u32::from_be_bytes(len_buf) as usize;
        let mut msg_buf = vec![0u8; len];
        reader.read_exact(&mut msg_buf).map_err(|e| {
            StreamError::Runtime(format!(
                "Failed to read message from subprocess stdout: {}",
                e
            ))
        })?;

        serde_json::from_slice(&msg_buf)
            .map_err(|e| StreamError::Runtime(format!("Failed to parse subprocess message: {}", e)))
    }
}

// ============================================================================
// Constructor factory for dynamic registration
// ============================================================================

/// Create a dynamic constructor for a Python native-mode subprocess processor.
///
/// The constructor creates a [`PythonNativeSubprocessHostProcessor`] with NO InputMailboxes
/// or OutputWriter. The Python subprocess manages its own iceoryx2 I/O via FFI.
/// The Rust host always runs in Manual execution mode.
pub(crate) fn create_python_native_subprocess_host_constructor(
    descriptor: &ProcessorDescriptor,
    execution_config: ExecutionConfig,
    project_path: std::path::PathBuf,
    native_lib_path: String,
) -> DynamicProcessorConstructorFn {
    let descriptor_clone = descriptor.clone();
    let entrypoint = descriptor.entrypoint.clone().unwrap_or_default();
    let project_path_str = project_path.to_string_lossy().to_string();

    Box::new(move |node: &ProcessorNode| {
        Ok(Box::new(PythonNativeSubprocessHostProcessor {
            child: None,
            stdin_writer: None,
            stdout_reader: None,
            runtime_context: None,
            entrypoint: entrypoint.clone(),
            project_path: project_path_str.clone(),
            processor_id: node.id.to_string(),
            processor_config: node.config.clone(),
            execution_config,
            descriptor_name: descriptor_clone.name.clone(),
            subprocess_dead: false,
            native_lib_path: native_lib_path.clone(),
            input_port_wiring: Vec::new(),
            output_port_wiring: Vec::new(),
        }) as ProcessorInstance)
    })
}

// ============================================================================
// Native lib path resolution
// ============================================================================

/// Resolve the path to the Python native FFI library.
///
/// Resolution order:
/// 1. `STREAMLIB_PYTHON_NATIVE_LIB` environment variable
/// 2. Default path: `{workspace_root}/target/debug/libstreamlib_python_native.dylib`
/// 3. Release path: `{workspace_root}/target/release/libstreamlib_python_native.dylib`
pub(crate) fn resolve_python_native_lib_path() -> Result<String> {
    // Tier 1: Environment variable
    if let Ok(path) = std::env::var("STREAMLIB_PYTHON_NATIVE_LIB") {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
    }

    // Tier 2: Default path relative to workspace root
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lib_name = if cfg!(target_os = "macos") {
        "libstreamlib_python_native.dylib"
    } else if cfg!(target_os = "linux") {
        "libstreamlib_python_native.so"
    } else {
        "streamlib_python_native.dll"
    };

    let default_path = workspace_root.join("target/debug").join(lib_name);
    if default_path.exists() {
        if let Ok(canonical) = default_path.canonicalize() {
            return Ok(canonical.to_string_lossy().to_string());
        }
    }

    // Tier 3: Release path
    let release_path = workspace_root.join("target/release").join(lib_name);
    if release_path.exists() {
        if let Ok(canonical) = release_path.canonicalize() {
            return Ok(canonical.to_string_lossy().to_string());
        }
    }

    Err(StreamError::Runtime(format!(
        "Python native FFI library not found. Expected at '{}' or '{}'. Build with: cargo build -p streamlib-python-native",
        default_path.display(),
        release_path.display()
    )))
}
