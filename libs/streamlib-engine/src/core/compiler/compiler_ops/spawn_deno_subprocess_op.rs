// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use crate::core::error::{Result, Error};
use crate::core::execution::ExecutionConfig;
use crate::core::graph::ProcessorNode;
use crate::core::processors::DynamicProcessorConstructorFn;
use crate::core::{
    ProcessorDescriptor, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};

use super::subprocess_bridge::{
    spawn_fd_line_reader, validate_subprocess_protocol, EscalateTransport, SubprocessBridge,
    PROTOCOL_VERSION_ENV, STREAMLIB_SUBPROCESS_PROTOCOL_VERSION,
};

// ============================================================================
// DenoSubprocessHostProcessor — Rust host for Deno subprocess processors
// ============================================================================

/// Rust-side host processor for TypeScript/Deno subprocess processors.
///
/// Unlike the Python subprocess host, this processor has NO [`InputMailboxes`]
/// or [`OutputWriter`]. The Deno subprocess manages its own iceoryx2 I/O
/// directly via FFI to `libstreamlib_deno_native.dylib`.
///
/// The Rust host is purely a lifecycle manager:
/// - Spawns `deno run` with the subprocess runner
/// - Sends lifecycle commands (setup, run, stop, teardown) via stdin/stdout pipes
/// - Relays escalate-on-behalf requests from the subprocess through
///   [`GpuContextLimitedAccess::escalate`]
/// - Always runs in Manual execution mode on the Rust side
pub(crate) struct DenoSubprocessHostProcessor {
    // Subprocess management (populated during setup)
    child: Option<Child>,
    bridge: Option<SubprocessBridge>,

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

    // Path to the native FFI library for Deno
    native_lib_path: String,

    // Port wiring info populated by the compiler's iceoryx2 service wiring phase.
    // Filled BEFORE setup() is called. Passed to the Deno subprocess in the setup command.
    pub(crate) input_port_wiring: Vec<serde_json::Value>,
    pub(crate) output_port_wiring: Vec<serde_json::Value>,
}

// ============================================================================
// DynGeneratedProcessor implementation
// ============================================================================

impl crate::core::processors::DynGeneratedProcessor for DenoSubprocessHostProcessor {
    fn __generated_setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> Result<()> {
        (|| -> Result<()> {
            let project_path = PathBuf::from(&self.project_path);

            tracing::info!(
                "[{}] Setting up Deno subprocess host: entrypoint='{}', project_path='{}'",
                self.processor_id,
                self.entrypoint,
                self.project_path
            );

            // Locate deno binary
            let deno_binary = which_deno()?;

            // Resolve native lib path via the shared resolver (env override →
            // registry-built home cache → monorepo `target/` dev fallback).
            // Previously this fell through to an unchecked `target/debug` path
            // string, which handed a registry consumer a dead path silently;
            // the shared resolver checks each tier and errors clearly.
            let native_lib_path = if !self.native_lib_path.is_empty() {
                self.native_lib_path.clone()
            } else {
                super::native_lib_resolver::resolve_subprocess_native_lib_path(
                    super::native_lib_resolver::SubprocessNativeRuntime::Deno,
                )?
            };

            // The SDK is resolved from the registry by version, never from a
            // workspace path: the Deno package's deno.json declares `streamlib`
            // (npm:@tatolab/streamlib-deno@<version>) and a sibling .npmrc
            // points the @tatolab scope at Gitea. The engine launches the SDK's
            // runner as the bare specifier `streamlib/subprocess_runner.ts`,
            // resolved through that config — the direct mirror of how the
            // Python op runs `-m streamlib.subprocess_runner` from the
            // registry-installed venv. The processor's own `import "streamlib"`
            // resolves through the same config. Deno fetches + caches the SDK on
            // first run, so no separate install step is needed. Dev iteration is
            // publish-a-dev-version + bump the package's declared `streamlib`
            // (no path escape hatch, by design — parity with the Python loop).
            let project_deno_config = project_path.join("deno.json");
            if !project_deno_config.exists() {
                return Err(Error::Runtime(format!(
                    "Deno package at '{}' has no deno.json — it must declare \
                     `streamlib` (npm:@tatolab/streamlib-deno@<version>) plus a \
                     sibling .npmrc pointing the @tatolab scope at the registry, \
                     so the engine can resolve the SDK runner by version.",
                    project_path.display()
                )));
            }

            // Determine the original TypeScript execution mode for the subprocess
            let execution_mode = match &self.execution_config.execution {
                crate::core::execution::ProcessExecution::Reactive => "reactive",
                crate::core::execution::ProcessExecution::Continuous { .. } => "continuous",
                crate::core::execution::ProcessExecution::Manual => "manual",
            };

            tracing::info!(
                "[{}] Spawning Deno subprocess: binary='{}', config='{}', native_lib='{}'",
                self.processor_id,
                deno_binary,
                project_deno_config.display(),
                native_lib_path
            );

            let mut command = Command::new(&deno_binary);
            command
                .arg("run")
                // Polyglot subprocesses run with full host trust — Rust has
                // no sandbox, Python has no permission gate, and `--allow-ffi`
                // alone is already the dominant capability (a Deno
                // subprocess with FFI can do anything any other polyglot
                // subprocess can). `--allow-all` brings Deno's posture in
                // line with the other two runtimes; the alternative
                // (per-permission allowlist) adds developer-experience
                // friction without raising the security bar, since FFI
                // bypasses every other gate anyway. If a future deployment
                // needs Deno specifically sandboxed beyond what Python +
                // Rust are, that's a separate axis worth its own design.
                .arg("--allow-all")
                .arg("--no-prompt")
                .arg("--unstable-webgpu")
                // Resolve `streamlib` (the runner here, and `import "streamlib"`
                // inside the dynamically-imported processor) through the
                // package's deno.json + sibling .npmrc.
                .arg("--config")
                .arg(&project_deno_config)
                .arg("streamlib/subprocess_runner.ts")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env("STREAMLIB_ENTRYPOINT", &self.entrypoint)
                .env(
                    "STREAMLIB_PROJECT_PATH",
                    project_path.to_string_lossy().as_ref(),
                )
                .env("STREAMLIB_NATIVE_LIB_PATH", &native_lib_path)
                .env("STREAMLIB_PROCESSOR_ID", &self.processor_id)
                .env("STREAMLIB_EXECUTION_MODE", execution_mode)
                // Advertise the engine's subprocess protocol version. The SDK
                // refuses to run at startup if it can't speak it — the engine →
                // SDK handshake direction (the reverse is validated from the
                // `ready` response below).
                .env(
                    PROTOCOL_VERSION_ENV,
                    STREAMLIB_SUBPROCESS_PROTOCOL_VERSION.to_string(),
                );

            #[cfg(target_os = "linux")]
            command.env("STREAMLIB_SURFACE_SOCKET", ctx.host_base().surface_socket_path());

            // Escalate IPC rides a dedicated `AF_UNIX` socketpair, not
            // fd1/fd2, so the subprocess's stdout/stderr can be captured
            // as `intercepted` log pipes without corrupting the framed
            // JSON protocol. See #451.
            let mut escalate_transport = EscalateTransport::attach(&mut command)?;

            let mut child = command.spawn()
                .map_err(|e| {
                    Error::Runtime(format!(
                        "Failed to spawn Deno subprocess for '{}': {}. Deno: '{}'",
                        self.processor_id, e, deno_binary
                    ))
                })?;

            escalate_transport.release_child_end();

            let child_pid = child.id();
            tracing::info!(
                "[{}] Deno subprocess spawned: pid={}",
                self.processor_id,
                child_pid
            );

            // Capture fd1 and fd2 from the subprocess as `intercepted`
            // log pipes. Each line produced on either pipe surfaces as
            // `tracing::warn!(intercepted=true, channel="fd1"|"fd2",
            // source="deno")` in the unified JSONL. fd1 used to be
            // reserved for framed IPC; #451 moved IPC to a socketpair
            // so fd1 is now free to capture raw writes.
            if let Some(stdout) = child.stdout.take() {
                spawn_fd_line_reader(
                    stdout,
                    "dn-stdout",
                    "fd1",
                    &self.processor_id,
                );
            }
            if let Some(stderr) = child.stderr.take() {
                spawn_fd_line_reader(
                    stderr,
                    "dn-stderr",
                    "fd2",
                    &self.processor_id,
                );
            }

            // Clone the sandbox so the bridge reader thread can dispatch
            // escalate requests on behalf of the subprocess.
            let sandbox = ctx.gpu_limited_access().clone();
            let escalate_stream = escalate_transport.into_parent_stream();
            let bridge = SubprocessBridge::new(
                escalate_stream,
                sandbox,
                self.processor_id.clone(),
            )?;

            self.child = Some(child);
            self.bridge = Some(bridge);

            // Send setup command with processor config and port wiring info.
            // `capability: "full"` mirrors the Rust-side `RuntimeContextFullAccess`
            // passed to `__generated_setup` — the subprocess must construct a
            // full-access ctx for the TS `setup(ctx)` call.
            let config = self
                .processor_config
                .clone()
                .unwrap_or(serde_json::Value::Null);
            self.bridge_send(&serde_json::json!({
                "cmd": "setup",
                "capability": "full",
                "config": config,
                "processor_id": self.processor_id,
                "ports": {
                    "inputs": self.input_port_wiring,
                    "outputs": self.output_port_wiring,
                },
            }))?;

            // Wait for "ready" response
            let response = self.bridge_recv()?;
            let rpc = response.get("rpc").and_then(|v| v.as_str()).unwrap_or("");
            if rpc != "ready" {
                let error = response
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(Error::Runtime(format!(
                    "Deno subprocess '{}' setup failed: {}",
                    self.processor_id, error
                )));
            }

            // Validate the SDK → engine handshake direction: the `ready`
            // response echoes the SDK's protocol version. An incompatible
            // installed SDK is caught here, at setup, rather than as a deep
            // FFI/escalate crash later.
            let sdk_protocol = response
                .get("protocol_version")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);
            validate_subprocess_protocol(sdk_protocol, &self.processor_id)?;

            tracing::info!(
                "[{}] Deno subprocess setup complete (pid={})",
                self.processor_id,
                child_pid
            );

            Ok(())
        })()
    }

    fn __generated_teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> Result<()> {
        (|| -> Result<()> {
            tracing::info!("[{}] Tearing down Deno subprocess", self.processor_id);

            // Send teardown command (best-effort)
            if self.bridge.is_some() {
                if let Err(e) = self.bridge_send(
                    &serde_json::json!({"cmd": "teardown", "capability": "full"}),
                ) {
                    tracing::warn!(
                        "[{}] Failed to send teardown command: {}",
                        self.processor_id,
                        e
                    );
                } else {
                    // Wait for done (bounded so a stuck subprocess doesn't block teardown)
                    match self.bridge_recv_timeout(Duration::from_secs(5)) {
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

            // Drop bridge — closes stdin, reader thread sees EOF on stdout
            // and exits, escalate registry is cleared.
            self.bridge.take();

            // Wait for subprocess to exit (with timeout)
            if let Some(mut child) = self.child.take() {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            tracing::info!(
                                "[{}] Deno subprocess exited: {}",
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
                                "[{}] Deno subprocess did not exit, killing",
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
        })()
    }

    fn __generated_on_pause(
        &mut self,
        _ctx: &RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        (|| -> Result<()> {
            if self.subprocess_dead {
                return Ok(());
            }
            if let Err(e) = self.bridge_send(
                &serde_json::json!({"cmd": "on_pause", "capability": "limited"}),
            ) {
                tracing::warn!("[{}] Failed to send on_pause: {}", self.processor_id, e);
                self.subprocess_dead = true;
                return Ok(());
            }
            match self.bridge_recv() {
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
        })()
    }

    fn __generated_on_resume(
        &mut self,
        _ctx: &RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        (|| -> Result<()> {
            if self.subprocess_dead {
                return Ok(());
            }
            if let Err(e) = self.bridge_send(
                &serde_json::json!({"cmd": "on_resume", "capability": "limited"}),
            ) {
                tracing::warn!("[{}] Failed to send on_resume: {}", self.processor_id, e);
                self.subprocess_dead = true;
                return Ok(());
            }
            match self.bridge_recv() {
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
        })()
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        // Deno subprocess manages its own iceoryx2 I/O via FFI.
        // The Rust host runs in Manual mode — process() is never called
        // by the thread runner for Manual processors.
        Ok(())
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
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

        // `run` enters the subprocess's execution loop; inside the loop
        // `process()` is dispatched with limited access, `on_pause`/`on_resume`
        // delivered concurrently are also limited. No single capability applies
        // to the command itself — the `capability` field is informational.
        self.bridge_send(&serde_json::json!({
            "cmd": "run",
            "capability": "limited",
            "execution": execution_mode,
            "interval_ms": interval_ms,
        }))?;

        // "run" enters a loop in the subprocess — no immediate response expected.
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.subprocess_dead {
            return Ok(());
        }
        if let Err(e) = self.bridge_send(
            &serde_json::json!({"cmd": "stop", "capability": "full"}),
        ) {
            tracing::warn!(
                "[{}] Subprocess pipe broken on stop: {}",
                self.processor_id,
                e
            );
            self.subprocess_dead = true;
            return Ok(());
        }
        match self.bridge_recv() {
            Ok(response) => {
                let rpc = response.get("rpc").and_then(|v| v.as_str()).unwrap_or("");
                if rpc != "stopped" {
                    let error = response
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    tracing::warn!("[{}] Deno stop() error: {}", self.processor_id, error);
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
        // Always Manual on the Rust side — the Deno subprocess manages its own loop
        ExecutionConfig::new(crate::core::execution::ProcessExecution::Manual)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        false
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        false
    }

    fn set_iceoryx2_resources(
        &mut self,
        _output_writer: Option<crate::iceoryx2::OutputWriter>,
        _input_mailboxes: Option<crate::iceoryx2::InputMailboxes>,
    ) -> crate::core::Result<()> {
        // Deno subprocess host wrappers don't carry host-side
        // iceoryx2 inner Arcs — the subprocess owns its own
        // iceoryx2 transport.
        Ok(())
    }

    fn iceoryx2_output_writer_inner(
        &self,
    ) -> Option<Arc<crate::iceoryx2::OutputWriterInner>> {
        None
    }

    fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<Arc<crate::iceoryx2::InputMailboxesInner>> {
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
            "runtime": "deno",
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
}

// ============================================================================
// Bridge protocol helpers (length-prefixed JSON, same as Python subprocess)
// ============================================================================

impl DenoSubprocessHostProcessor {
    /// Send a length-prefixed JSON message to the subprocess stdin.
    fn bridge_send(&mut self, msg: &serde_json::Value) -> Result<()> {
        let bridge = self.bridge.as_ref().ok_or_else(|| {
            Error::Runtime("Subprocess bridge not initialized".to_string())
        })?;
        bridge.send(msg)
    }

    /// Read a length-prefixed JSON message from the subprocess stdout.
    fn bridge_recv(&mut self) -> Result<serde_json::Value> {
        let bridge = self.bridge.as_ref().ok_or_else(|| {
            Error::Runtime("Subprocess bridge not initialized".to_string())
        })?;
        bridge.recv_lifecycle()
    }

    fn bridge_recv_timeout(&mut self, timeout: Duration) -> Result<serde_json::Value> {
        let bridge = self.bridge.as_ref().ok_or_else(|| {
            Error::Runtime("Subprocess bridge not initialized".to_string())
        })?;
        bridge
            .recv_lifecycle_timeout(timeout)
            .map_err(|e| Error::Runtime(format!("bridge recv timed out: {e}")))
    }
}

// ============================================================================
// Constructor factory for dynamic registration
// ============================================================================

/// Create a dynamic constructor for a Deno subprocess processor.
///
/// The constructor creates a [`DenoSubprocessHostProcessor`] with NO InputMailboxes
/// or OutputWriter. The Deno subprocess manages its own iceoryx2 I/O via FFI.
/// The Rust host always runs in Manual execution mode.
pub(crate) fn create_deno_subprocess_host_constructor(
    descriptor: &ProcessorDescriptor,
    execution_config: ExecutionConfig,
    project_path: std::path::PathBuf,
) -> DynamicProcessorConstructorFn {
    let descriptor_clone = descriptor.clone();
    let entrypoint = descriptor.entrypoint.clone().unwrap_or_default();
    let project_path_str = project_path.to_string_lossy().to_string();

    Box::new(move |node: &ProcessorNode| {
        Ok(Box::new(DenoSubprocessHostProcessor {
            child: None,
            bridge: None,
            entrypoint: entrypoint.clone(),
            project_path: project_path_str.clone(),
            processor_id: node.id.to_string(),
            processor_config: node.config.clone(),
            execution_config,
            descriptor_name: descriptor_clone.name.to_string(),
            subprocess_dead: false,
            native_lib_path: String::new(),
            input_port_wiring: Vec::new(),
            output_port_wiring: Vec::new(),
        }) as Box<dyn crate::core::processors::DynGeneratedProcessor + Send>)
    })
}

// ============================================================================
// Helper functions
// ============================================================================

/// Locate the `deno` binary on the system PATH.
fn which_deno() -> Result<String> {
    // Check DENO_PATH env var first
    if let Ok(path) = std::env::var("DENO_PATH") {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
    }

    // Try to find deno on PATH using `which`
    let output = Command::new("which")
        .arg("deno")
        .output()
        .map_err(|e| {
            Error::Runtime(format!(
                "Failed to locate deno binary: {}. Install with: curl -fsSL https://deno.land/install.sh | sh",
                e
            ))
        })?;

    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(path);
        }
    }

    Err(Error::Runtime(
        "deno binary not found on PATH. Install with: curl -fsSL https://deno.land/install.sh | sh"
            .to_string(),
    ))
}
