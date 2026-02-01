// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;

use crate::core::error::{Result, StreamError};
use crate::core::execution::ExecutionConfig;
use crate::core::graph::ProcessorNode;
use crate::core::processors::{DynamicProcessorConstructorFn, ProcessorInstance};
use crate::core::runtime::BoxFuture;
use crate::core::{ProcessorDescriptor, RuntimeContext};
use crate::iceoryx2::{InputMailboxes, OutputWriter};

// ============================================================================
// SubprocessHostProcessor — Rust host for Python subprocess processors
// ============================================================================

/// Rust-side host processor for Python subprocess processors.
///
/// Implements [`DynGeneratedProcessor`] with real [`InputMailboxes`] and [`OutputWriter`]
/// (wired by the compiler like any Rust processor). Bridges to a Python subprocess
/// via stdin/stdout pipes using a length-prefixed JSON protocol.
///
/// When the thread runner calls `process()`, this sends a "process" command to
/// the Python subprocess. The Python processor may then send RPCs back (read/write)
/// which are handled by the Rust host using the real iceoryx2 infrastructure.
pub(crate) struct SubprocessHostProcessor {
    // iceoryx2 I/O (configured during wiring, same as macro-generated processors)
    inputs: InputMailboxes,
    outputs: Arc<OutputWriter>,

    // Subprocess management (populated during setup)
    child: Option<Child>,
    stdin_writer: Option<BufWriter<ChildStdin>>,
    stdout_reader: Option<BufReader<ChildStdout>>,

    // RuntimeContext for GpuContext access during process() RPCs
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
}

// ============================================================================
// DynGeneratedProcessor implementation
// ============================================================================

impl crate::core::processors::DynGeneratedProcessor for SubprocessHostProcessor {
    fn __generated_setup(&mut self, ctx: RuntimeContext) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.runtime_context = Some(ctx.clone());

            let runtime_id = ctx.runtime_id().to_string();
            let project_path = PathBuf::from(&self.project_path);

            tracing::info!(
                "[{}] Setting up Python subprocess host: entrypoint='{}', project_path='{}'",
                self.processor_id,
                self.entrypoint,
                self.project_path
            );

            // Create venv and get python executable path
            let python_executable =
                ensure_processor_venv(&runtime_id, &self.processor_id, &project_path)?;

            // Build PYTHONPATH for the subprocess.
            // In dev mode, the streamlib package source lives at
            // libs/streamlib-python/python/ relative to the workspace root.
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

            // Spawn subprocess with piped stdin/stdout.
            // Remove PYTHONHOME — the venv's python uses pyvenv.cfg, and an
            // inherited PYTHONHOME would override that and break module resolution.
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
                .spawn()
                .map_err(|e| {
                    StreamError::Runtime(format!(
                        "Failed to spawn Python subprocess for '{}': {}. Python: '{}'",
                        self.processor_id, e, python_executable
                    ))
                })?;

            let child_pid = child.id();
            tracing::info!(
                "[{}] Python subprocess spawned: pid={}",
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

            // Send setup command with processor config
            let config = self
                .processor_config
                .clone()
                .unwrap_or(serde_json::Value::Null);
            self.bridge_send_json(&serde_json::json!({
                "cmd": "setup",
                "config": config
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
                    "Python subprocess '{}' setup failed: {}",
                    self.processor_id, error
                )));
            }

            tracing::info!(
                "[{}] Python subprocess setup complete (pid={})",
                self.processor_id,
                child_pid
            );

            Ok(())
        })
    }

    fn __generated_teardown(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            tracing::info!("[{}] Tearing down Python subprocess", self.processor_id);

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
                                "[{}] Python subprocess exited: {}",
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
                                "[{}] Python subprocess did not exit, killing",
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
        Box::pin(async { Ok(()) })
    }

    fn __generated_on_resume(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn process(&mut self) -> Result<()> {
        if self.subprocess_dead {
            return Ok(());
        }

        // Send "process" command to Python
        if let Err(e) = self.bridge_send_json(&serde_json::json!({"cmd": "process"})) {
            tracing::warn!(
                "[{}] Subprocess pipe broken, marking dead: {}",
                self.processor_id,
                e
            );
            self.subprocess_dead = true;
            return Ok(());
        }

        // Handle RPCs from Python until "done"
        loop {
            let msg = match self.bridge_read_json() {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(
                        "[{}] Subprocess pipe broken, marking dead: {}",
                        self.processor_id,
                        e
                    );
                    self.subprocess_dead = true;
                    return Ok(());
                }
            };
            let rpc = msg.get("rpc").and_then(|v| v.as_str()).unwrap_or("");

            match rpc {
                "read" => {
                    let port = msg.get("port").and_then(|v| v.as_str()).ok_or_else(|| {
                        StreamError::Runtime("read RPC missing 'port' field".to_string())
                    })?;

                    match self.inputs.read_raw(port) {
                        Ok(Some((data, timestamp_ns))) => {
                            let header = serde_json::json!({
                                "ts": timestamp_ns,
                                "data_len": data.len()
                            });
                            self.bridge_send_json(&header)?;
                            self.bridge_send_binary(&data)?;
                        }
                        Ok(None) => {
                            self.bridge_send_json(&serde_json::json!({
                                "data_len": 0
                            }))?;
                        }
                        Err(e) => {
                            self.bridge_send_json(&serde_json::json!({
                                "data_len": 0,
                                "error": e.to_string()
                            }))?;
                        }
                    }
                }
                "write" => {
                    let port = msg.get("port").and_then(|v| v.as_str()).ok_or_else(|| {
                        StreamError::Runtime("write RPC missing 'port' field".to_string())
                    })?;
                    let timestamp_ns = msg.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
                    let data_len =
                        msg.get("data_len").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                    let data = if data_len > 0 {
                        self.bridge_read_binary(data_len)?
                    } else {
                        Vec::new()
                    };

                    match self.outputs.write_raw(port, &data, timestamp_ns) {
                        Ok(()) => {
                            self.bridge_send_json(&serde_json::json!({"ok": true}))?;
                        }
                        Err(e) => {
                            self.bridge_send_json(&serde_json::json!({
                                "ok": false,
                                "error": e.to_string()
                            }))?;
                        }
                    }
                }
                "resolve_surface" => {
                    let surface_id =
                        msg.get("surface_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                StreamError::Runtime("resolve_surface missing 'surface_id'".into())
                            })?;

                    #[cfg(target_os = "macos")]
                    {
                        let gpu = &self
                            .runtime_context
                            .as_ref()
                            .ok_or_else(|| StreamError::Runtime("No RuntimeContext".into()))?
                            .gpu;

                        let buffer = gpu.check_out_surface(surface_id)?;
                        let iosurface = buffer.buffer_ref().iosurface_ref().ok_or_else(|| {
                            StreamError::Runtime("Buffer has no IOSurface backing".into())
                        })?;
                        let iosurface_id =
                            unsafe { crate::apple::corevideo_ffi::IOSurfaceGetID(iosurface) };

                        self.bridge_send_json(&serde_json::json!({
                            "iosurface_id": iosurface_id,
                            "width": buffer.width,
                            "height": buffer.height,
                        }))?;
                    }

                    #[cfg(not(target_os = "macos"))]
                    {
                        tracing::error!(
                            "[{}] resolve_surface RPC not supported on this platform. \
                             macOS uses IOSurface; Linux needs Vulkan DMA-BUF; Windows needs DirectX shared textures.",
                            self.processor_id
                        );
                        self.bridge_send_json(&serde_json::json!({
                            "error": format!(
                                "resolve_surface not supported on {} — \
                                 zero-copy pixel sharing requires platform-specific implementation \
                                 (Linux: Vulkan DMA-BUF, Windows: DirectX shared textures)",
                                std::env::consts::OS
                            )
                        }))?;
                    }
                }
                "acquire_surface" => {
                    let w = msg.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                    let h = msg.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

                    #[cfg(target_os = "macos")]
                    {
                        let gpu = &self
                            .runtime_context
                            .as_ref()
                            .ok_or_else(|| StreamError::Runtime("No RuntimeContext".into()))?
                            .gpu;

                        let (pool_id, buffer) =
                            gpu.acquire_pixel_buffer(w, h, crate::core::rhi::PixelFormat::Bgra32)?;

                        let iosurface = buffer.buffer_ref().iosurface_ref().ok_or_else(|| {
                            StreamError::Runtime("Acquired buffer has no IOSurface backing".into())
                        })?;
                        let iosurface_id =
                            unsafe { crate::apple::corevideo_ffi::IOSurfaceGetID(iosurface) };

                        self.bridge_send_json(&serde_json::json!({
                            "surface_id": pool_id.to_string(),
                            "iosurface_id": iosurface_id,
                            "width": w,
                            "height": h,
                        }))?;
                    }

                    #[cfg(not(target_os = "macos"))]
                    {
                        tracing::error!(
                            "[{}] acquire_surface RPC not supported on this platform. \
                             macOS uses IOSurface; Linux needs Vulkan DMA-BUF; Windows needs DirectX shared textures.",
                            self.processor_id
                        );
                        self.bridge_send_json(&serde_json::json!({
                            "error": format!(
                                "acquire_surface not supported on {} — \
                                 zero-copy pixel sharing requires platform-specific implementation \
                                 (Linux: Vulkan DMA-BUF, Windows: DirectX shared textures)",
                                std::env::consts::OS
                            )
                        }))?;
                    }
                }
                "done" => {
                    if let Some(error) = msg.get("error").and_then(|v| v.as_str()) {
                        tracing::warn!("[{}] Python process() error: {}", self.processor_id, error);
                    }
                    break;
                }
                other => {
                    tracing::warn!(
                        "[{}] Unknown RPC from Python: '{}'",
                        self.processor_id,
                        other
                    );
                    break;
                }
            }
        }

        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        self.bridge_send_json(&serde_json::json!({"cmd": "start"}))?;
        let response = self.bridge_read_json()?;
        let rpc = response.get("rpc").and_then(|v| v.as_str()).unwrap_or("");
        if rpc != "ready" {
            let error = response
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(StreamError::Runtime(format!(
                "Python start() failed: {}",
                error
            )));
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.bridge_send_json(&serde_json::json!({"cmd": "stop"}))?;
        let response = self.bridge_read_json()?;
        let rpc = response.get("rpc").and_then(|v| v.as_str()).unwrap_or("");
        if rpc != "done" {
            let error = response
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            tracing::warn!("[{}] Python stop() error: {}", self.processor_id, error);
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
        self.execution_config
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        true
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        true
    }

    fn get_iceoryx2_output_writer(&self) -> Option<Arc<OutputWriter>> {
        Some(self.outputs.clone())
    }

    fn get_iceoryx2_input_mailboxes(&mut self) -> Option<&mut InputMailboxes> {
        Some(&mut self.inputs)
    }

    fn apply_config_json(&mut self, _config_json: &serde_json::Value) -> Result<()> {
        Ok(())
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        serde_json::json!({
            "subprocess_pid": self.child.as_ref().map(|c| c.id()),
            "entrypoint": self.entrypoint,
            "project_path": self.project_path,
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
// Bridge protocol helpers
// ============================================================================

impl SubprocessHostProcessor {
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

    /// Send raw binary data to the subprocess stdin.
    fn bridge_send_binary(&mut self, data: &[u8]) -> Result<()> {
        let writer = self
            .stdin_writer
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Subprocess stdin not available".to_string()))?;

        writer.write_all(data).map_err(|e| {
            StreamError::Runtime(format!("Failed to write binary to subprocess stdin: {}", e))
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

    /// Read raw binary data of a specific length from the subprocess stdout.
    fn bridge_read_binary(&mut self, len: usize) -> Result<Vec<u8>> {
        let reader = self
            .stdout_reader
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("Subprocess stdout not available".to_string()))?;

        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).map_err(|e| {
            StreamError::Runtime(format!(
                "Failed to read binary from subprocess stdout: {}",
                e
            ))
        })?;

        Ok(buf)
    }
}

// ============================================================================
// Constructor factory for dynamic registration
// ============================================================================

/// Create a dynamic constructor for a Python subprocess processor.
///
/// The constructor creates a [`SubprocessHostProcessor`] with real [`InputMailboxes`]
/// and [`OutputWriter`], wired by the compiler like any Rust processor.
/// The Python subprocess is spawned during setup, not construction.
pub(crate) fn create_subprocess_host_constructor(
    descriptor: &ProcessorDescriptor,
    execution_config: ExecutionConfig,
) -> DynamicProcessorConstructorFn {
    let descriptor_clone = descriptor.clone();
    let entrypoint = descriptor.entrypoint.clone().unwrap_or_default();

    Box::new(move |node: &ProcessorNode| {
        let mut inputs = InputMailboxes::new();
        for input in &descriptor_clone.inputs {
            inputs.add_port(&input.name, 1, Default::default());
        }
        let outputs = Arc::new(OutputWriter::new());

        let project_path = node
            .config
            .as_ref()
            .and_then(|c| c.get("project_path"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        Ok(Box::new(SubprocessHostProcessor {
            inputs,
            outputs,
            child: None,
            stdin_writer: None,
            stdout_reader: None,
            runtime_context: None,
            entrypoint: entrypoint.clone(),
            project_path,
            processor_id: node.id.to_string(),
            processor_config: node.config.clone(),
            execution_config,
            descriptor_name: descriptor_clone.name.clone(),
            subprocess_dead: false,
        }) as ProcessorInstance)
    })
}

// ============================================================================
// Venv management
// ============================================================================

/// Ensure a processor venv exists in STREAMLIB_HOME, install deps, and return the python path.
///
/// Venv location: `~/.streamlib/runtimes/{runtime_id}/processors/{processor_id}/venv`
/// Uses `uv` for fast venv creation and dependency installation.
/// The shared UV cache (`~/.streamlib/cache/uv`) avoids re-downloading packages.
fn ensure_processor_venv(
    runtime_id: &str,
    processor_id: &str,
    project_path: &Path,
) -> Result<String> {
    let venv_dir = crate::core::streamlib_home::get_processor_venv_dir(runtime_id, processor_id);
    let uv_cache_dir = crate::core::streamlib_home::get_uv_cache_dir();

    // Platform-specific python binary path within venv
    #[cfg(unix)]
    let venv_python = venv_dir.join("bin").join("python");
    #[cfg(windows)]
    let venv_python = venv_dir.join("Scripts").join("python.exe");

    // Create venv if it doesn't exist
    if !venv_python.exists() {
        tracing::info!("[{}] Creating venv at {}", processor_id, venv_dir.display());

        // Ensure parent directories exist
        std::fs::create_dir_all(venv_dir.parent().unwrap_or(&venv_dir)).map_err(|e| {
            StreamError::Runtime(format!("Failed to create venv parent directory: {}", e))
        })?;

        let output = run_uv(
            &["venv", venv_dir.to_str().unwrap_or(""), "--python", "3.12"],
            &uv_cache_dir,
        )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(StreamError::Runtime(format!(
                "Failed to create venv for processor '{}': {}",
                processor_id, stderr
            )));
        }

        tracing::info!("[{}] Venv created", processor_id);
    } else {
        tracing::debug!(
            "[{}] Reusing existing venv at {}",
            processor_id,
            venv_dir.display()
        );
    }

    // Install project dependencies (if project_path is valid)
    if !project_path.as_os_str().is_empty() && project_path.join("pyproject.toml").exists() {
        tracing::info!(
            "[{}] Installing project deps from {}",
            processor_id,
            project_path.display()
        );

        let venv_python_str = venv_python.to_string_lossy().to_string();
        let project_path_str = project_path.to_string_lossy().to_string();

        let output = run_uv(
            &[
                "pip",
                "install",
                "-e",
                &project_path_str,
                "--python",
                &venv_python_str,
            ],
            &uv_cache_dir,
        )?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                "[{}] Failed to install project deps (continuing): {}",
                processor_id,
                stderr
            );
        }
    }

    Ok(venv_python.to_string_lossy().to_string())
}

/// Run a `uv` command with the given args and cache directory.
fn run_uv(args: &[&str], uv_cache_dir: &Path) -> Result<std::process::Output> {
    Command::new("uv")
        .args(args)
        .env("UV_CACHE_DIR", uv_cache_dir.to_str().unwrap_or(""))
        .output()
        .map_err(|e| {
            StreamError::Runtime(format!(
                "Failed to run uv (is uv installed?): {}. Install with: curl -LsSf https://astral.sh/uv/install.sh | sh",
                e
            ))
        })
}
