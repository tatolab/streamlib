// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Length-prefixed JSON escalate-IPC bridge shared by the Python and Deno
//! subprocess host processors.
//!
//! Frames travel over a dedicated [`UnixStream`] pair created by
//! [`EscalateTransport::attach`] before spawn — not over the subprocess's
//! stdin/stdout. The parent keeps one half of the socketpair and the
//! child inherits the other via `STREAMLIB_ESCALATE_FD`, freeing fd1/fd2
//! to be captured as intercepted log pipes by the host.
//!
//! Two roles travel over the same socket:
//! 1. Lifecycle RPC (`setup`, `run`, `stop`, `teardown`, `on_pause`,
//!    `on_resume`, …) — initiated by the host, the subprocess replies with
//!    `rpc: "ready" | "stopped" | "ok" | "done" | "error"`.
//! 2. Escalate-on-behalf (`rpc: "escalate_request"`) — initiated by the
//!    subprocess, the host replies with `rpc: "escalate_response"`.
//!
//! A dedicated reader thread (`br-…`) owns the parent-side read half and
//! demultiplexes incoming messages: escalate requests are dispatched
//! inline through [`subprocess_escalate::process_bridge_message`], and
//! anything else is forwarded to the main thread over an mpsc channel for
//! the lifecycle RPC to consume. Writes in both directions serialize
//! through a shared `Arc<Mutex<BufWriter<UnixStream>>>` so the main
//! thread and the reader thread can't interleave halves of a
//! length-prefixed frame.

use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::core::context::GpuContextLimitedAccess;
use crate::core::error::{Error, Result};

use super::subprocess_escalate::{
    EscalateHandleRegistry, process_bridge_message,
    release_surface_share_and_texture_cache_for_handle,
};

/// Env var advertising the inherited child-end fd number of the escalate
/// socketpair. The subprocess opens this fd as a duplex UNIX socket and
/// uses it as the framed-IPC transport.
pub(crate) const ESCALATE_FD_ENV: &str = "STREAMLIB_ESCALATE_FD";

/// Helper-process protocol version — the coordinate the engine and the
/// helper's Python half handshake on. Covers the escalate IPC schema and this
/// lifecycle-command protocol; bump it, in lockstep with `_helper.py`'s
/// mirror constant, when either changes incompatibly.
///
/// Engine and helper ship in one wheel, so this cannot disagree with itself in
/// a correct install. What the handshake catches is a stale process still
/// running an older build, or a different `streamlib` earlier on the child's
/// `sys.path` — both of which would otherwise surface as a mis-parsed op deep
/// inside an escalate round trip.
///
/// v2: the compute escalate ops carry named binding arrays.
pub const STREAMLIB_SUBPROCESS_PROTOCOL_VERSION: u32 = 2;

/// Oldest helper protocol this engine accepts. Equal to the current version:
/// the two halves ship together, so there is no supported skew, and accepting
/// an older helper would let it mis-parse an op that changed shape.
pub(crate) const MIN_SUPPORTED_SUBPROCESS_PROTOCOL: u32 = 2;

/// Env var the engine sets to advertise [`STREAMLIB_SUBPROCESS_PROTOCOL_VERSION`]
/// to the subprocess. The SDK reads it at startup and refuses to run if it
/// can't speak the engine's protocol (the engine → SDK handshake direction).
pub const PROTOCOL_VERSION_ENV: &str = "STREAMLIB_PROTOCOL_VERSION";

/// Validate the protocol version an SDK reported (in its `ready` response)
/// against the engine's supported range — the SDK → engine handshake
/// direction. Fails loud with an actionable named error so an incompatible
/// installed SDK is caught at setup, never as a deep FFI/escalate crash.
pub fn validate_subprocess_protocol(sdk_version: Option<u32>, processor_id: &str) -> Result<()> {
    let sdk_version = sdk_version.ok_or_else(|| {
        Error::Runtime(format!(
            "[{processor_id}] subprocess protocol handshake failed: the SDK did \
             not report a protocol version. The installed streamlib is older \
             than this engine's handshake (engine speaks \
             v{MIN_SUPPORTED_SUBPROCESS_PROTOCOL}..=v{STREAMLIB_SUBPROCESS_PROTOCOL_VERSION}); \
             bump the package's declared streamlib version."
        ))
    })?;
    if !(MIN_SUPPORTED_SUBPROCESS_PROTOCOL..=STREAMLIB_SUBPROCESS_PROTOCOL_VERSION)
        .contains(&sdk_version)
    {
        return Err(Error::Runtime(format!(
            "[{processor_id}] subprocess protocol mismatch: the installed \
             streamlib SDK speaks protocol v{sdk_version}, this engine speaks \
             v{MIN_SUPPORTED_SUBPROCESS_PROTOCOL}..=v{STREAMLIB_SUBPROCESS_PROTOCOL_VERSION}. \
             Align the package's declared streamlib version to one compatible \
             with this engine."
        )));
    }
    Ok(())
}

/// Socketpair-backed escalate IPC transport. The parent holds one half
/// and the subprocess inherits the other via [`ESCALATE_FD_ENV`].
pub struct EscalateTransport {
    parent_end: UnixStream,
    /// Kept alive so the child fd stays open across `Command::spawn`. The
    /// caller drops this after spawn so only the subprocess holds the
    /// child end.
    child_end: Option<UnixStream>,
}

impl EscalateTransport {
    /// Create a socketpair, register `pre_exec` on `command` to clear
    /// `FD_CLOEXEC` on the child-end fd, and set [`ESCALATE_FD_ENV`] on
    /// the command's environment.
    ///
    /// After `command.spawn()`, call [`Self::release_child_end`] so only
    /// the subprocess retains the child-side fd.
    pub fn attach(command: &mut Command) -> Result<Self> {
        let (parent_end, child_end) = UnixStream::pair()
            .map_err(|e| Error::Runtime(format!("failed to create escalate socketpair: {e}")))?;

        let child_fd: RawFd = child_end.as_raw_fd();

        // Clear FD_CLOEXEC on the child-end fd between fork and exec so
        // the execed subprocess inherits it. `fcntl` is async-signal-safe
        // so it's legal to call from `pre_exec`.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(child_fd, libc::F_GETFD);
                if flags < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let rc = libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                if rc < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        command.env(ESCALATE_FD_ENV, child_fd.to_string());

        Ok(Self {
            parent_end,
            child_end: Some(child_end),
        })
    }

    /// Drop the parent's reference to the child-end fd. Call this after
    /// `command.spawn()` succeeds so only the subprocess keeps it open.
    pub fn release_child_end(&mut self) {
        self.child_end.take();
    }

    /// Consume the transport and return the parent-side [`UnixStream`].
    pub fn into_parent_stream(mut self) -> UnixStream {
        self.child_end.take();
        self.parent_end
    }
}

/// Shared writer handle. The host's lifecycle path and the reader
/// thread's escalate-response path both write through this mutex.
type SharedWriter = Arc<Mutex<BufWriter<UnixStream>>>;

/// Bridge for one subprocess. Drop the value to tear the reader thread
/// down cleanly (shutdown the parent-side socket read half; reader
/// thread exits on EOF).
pub struct SubprocessBridge {
    processor_id: String,
    writer: SharedWriter,
    lifecycle_rx: Receiver<serde_json::Value>,
    registry: Arc<EscalateHandleRegistry>,
    /// Held for teardown: the drop path evicts what the registry's acquires
    /// entered into the parent's texture cache, which needs the same
    /// capability the reader thread dispatches against.
    sandbox: GpuContextLimitedAccess,
    reader: Option<JoinHandle<()>>,
    dead: Arc<Mutex<bool>>,
}

impl SubprocessBridge {
    /// Wrap a socketpair parent end and spawn the reader thread.
    ///
    /// `sandbox` is cloned into the reader thread so escalate requests
    /// can be dispatched without blocking the main thread. `processor_id`
    /// is used for thread naming and tracing.
    pub fn new(
        stream: UnixStream,
        sandbox: GpuContextLimitedAccess,
        processor_id: String,
    ) -> Result<Self> {
        let read_half = stream.try_clone().map_err(|e| {
            Error::Runtime(format!(
                "failed to clone escalate socketpair for reader: {e}"
            ))
        })?;
        let writer: SharedWriter = Arc::new(Mutex::new(BufWriter::new(stream)));
        let registry = EscalateHandleRegistry::new();
        let (tx, rx) = mpsc::channel();
        let dead = Arc::new(Mutex::new(false));

        let thread_name = thread_name(&processor_id);
        let reader_writer = Arc::clone(&writer);
        let reader_registry = Arc::clone(&registry);
        let reader_dead = Arc::clone(&dead);
        let reader_processor_id = processor_id.clone();
        let teardown_sandbox = sandbox.clone();

        let reader = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                reader_loop(
                    BufReader::new(read_half),
                    reader_writer,
                    sandbox,
                    reader_registry,
                    tx,
                    reader_dead,
                    reader_processor_id,
                );
            })
            .expect("failed to spawn bridge reader thread");

        Ok(Self {
            processor_id,
            writer,
            lifecycle_rx: rx,
            registry,
            sandbox: teardown_sandbox,
            reader: Some(reader),
            dead,
        })
    }

    /// Write a length-prefixed JSON message to the subprocess.
    pub fn send(&self, msg: &serde_json::Value) -> Result<()> {
        if self.is_dead() {
            return Err(Error::Runtime(format!(
                "[{}] bridge marked dead, cannot send",
                self.processor_id
            )));
        }
        // The lifecycle command is the engine's only reading of which hook the
        // child is inside, and this is the one seam every command crosses.
        // Setup-phase-only escalate ops (minting a processor-owned window)
        // refuse on it, dispatched from the reader thread while the hook that
        // is allowed to ask is still running.
        if let Some(lifecycle_command) = msg.get("cmd").and_then(|c| c.as_str()) {
            self.registry
                .note_lifecycle_command_sent_to_the_helper_process(lifecycle_command);
        }
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| Error::Runtime("subprocess writer mutex poisoned".to_string()))?;
        write_frame(&mut *writer, msg).map_err(|e| {
            self.mark_dead();
            e
        })
    }

    /// Block until the next lifecycle-tagged message arrives.
    pub fn recv_lifecycle(&self) -> Result<serde_json::Value> {
        self.lifecycle_rx.recv().map_err(|_| {
            self.mark_dead();
            Error::Runtime(format!(
                "[{}] subprocess escalate socket closed before reply",
                self.processor_id
            ))
        })
    }

    /// Block up to `timeout` for the next lifecycle-tagged message.
    pub fn recv_lifecycle_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<serde_json::Value, RecvTimeoutError> {
        self.lifecycle_rx.recv_timeout(timeout)
    }

    /// Mark the bridge dead; subsequent sends return immediately.
    pub fn mark_dead(&self) {
        if let Ok(mut dead) = self.dead.lock() {
            *dead = true;
        }
    }

    pub fn is_dead(&self) -> bool {
        self.dead.lock().map(|g| *g).unwrap_or(true)
    }

    /// Count of escalate-acquired handles the host still holds. Used by
    /// teardown logging and tests.
    pub(crate) fn registry(&self) -> &Arc<EscalateHandleRegistry> {
        &self.registry
    }
}

impl Drop for SubprocessBridge {
    fn drop(&mut self) {
        self.mark_dead();
        // Shut the socket down before draining: until the reader thread sees
        // EOF it keeps dispatching escalate requests, and an acquire landing
        // after the drain would strand its cache entry — the very leak the
        // drain exists to close. A request already executing when the
        // shutdown lands can still slip through; closing that too would mean
        // joining the reader, which this path deliberately never blocks on.
        // The OS reaps the thread on process exit.
        if let Ok(writer) = self.writer.lock() {
            let _ = writer.get_ref().shutdown(std::net::Shutdown::Both);
        }
        // Windows first: each present thread resolves surface ids against the
        // same capability the handle release below evicts from, and dropping
        // one closes its window and joins its thread. A helper that never
        // called `close_processor_owned_window` — or crashed — releases its
        // windows here, which is what makes teardown the backstop the plan
        // says it is.
        for (window_id, present_loop) in self.registry.drain_processor_owned_windows() {
            tracing::debug!(
                "[{}] closing processor-owned window '{}' at teardown",
                self.processor_id,
                window_id
            );
            drop(present_loop);
        }
        // Run the release path's kind-specific cleanup for everything the
        // helper never released — a crashed child must not strand cache
        // entries in a GpuContext that outlives every respawn, nor
        // surface-share registrations and their fd dups: the host's own
        // connection registered those on the helper's behalf, so the
        // service's disconnect watchdog rightly never reclaims them.
        for (handle_id, removed_handle) in self.registry.drain_handles() {
            release_surface_share_and_texture_cache_for_handle(
                &self.sandbox,
                &handle_id,
                &removed_handle,
            );
        }
        if let Some(reader) = self.reader.take() {
            drop(reader);
        }
    }
}

/// Reader loop: drain the parent-side socket, dispatch escalate traffic,
/// forward lifecycle responses to `lifecycle_tx`.
fn reader_loop(
    mut reader: BufReader<UnixStream>,
    writer: SharedWriter,
    sandbox: GpuContextLimitedAccess,
    registry: Arc<EscalateHandleRegistry>,
    lifecycle_tx: mpsc::Sender<serde_json::Value>,
    dead: Arc<Mutex<bool>>,
    processor_id: String,
) {
    loop {
        let msg = match read_frame(&mut reader) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("[{}] bridge reader exiting: {}", processor_id, e);
                if let Ok(mut dead) = dead.lock() {
                    *dead = true;
                }
                break;
            }
        };

        // Classify the frame on the rpc tag, not the handler's reply
        // shape: fire-and-forget escalate ops (e.g. log) consume the
        // message but produce no response, so a `None` from
        // `process_bridge_message` cannot be used as the "this wasn't
        // an escalate request" signal — that would silently re-route
        // every log message to the lifecycle queue and trip the
        // setup/teardown waiters.
        let is_escalate_request = msg.get("rpc").and_then(|v| v.as_str())
            == Some(super::subprocess_escalate::ESCALATE_REQUEST_RPC);

        if is_escalate_request {
            if let Some(response) = process_bridge_message(&sandbox, &registry, &msg) {
                // Escalate request handled inline. Write response with the
                // shared writer lock.
                let send_result: Result<()> = {
                    let mut writer = match writer.lock() {
                        Ok(g) => g,
                        Err(_) => {
                            tracing::warn!(
                                "[{}] bridge reader saw poisoned writer mutex",
                                processor_id
                            );
                            break;
                        }
                    };
                    write_frame(&mut *writer, &response)
                };
                if let Err(e) = send_result {
                    tracing::warn!(
                        "[{}] bridge reader failed to write escalate response: {}",
                        processor_id,
                        e
                    );
                    if let Ok(mut dead) = dead.lock() {
                        *dead = true;
                    }
                    break;
                }
            }
            // Fire-and-forget ops (log) leave nothing to write. Either way,
            // never forward escalate traffic to the lifecycle channel.
            continue;
        }

        // Lifecycle response — forward to main thread. Send failure
        // means the receiver is gone (host dropped), exit cleanly.
        if lifecycle_tx.send(msg).is_err() {
            tracing::debug!(
                "[{}] bridge reader exiting: lifecycle channel dropped",
                processor_id
            );
            break;
        }
    }
}

/// Per-line reader that tags each non-empty line with
/// `intercepted=true, channel=<channel>, source=python|deno` and emits
/// it as a `tracing::warn!` event. Used by the Python and Deno spawn
/// paths on the subprocess's fd1 (stdout) and fd2 (stderr). `channel`
/// must be `"fd1"` or `"fd2"`; the source and tracing target are
/// inferred from `thread_prefix` (`"py-…"` → python, `"dn-…"` → deno).
///
/// Captures the caller's current [`tracing::Dispatch`] and installs it
/// as the reader thread's default, so events route through whatever
/// subscriber the owning runtime installed (global for production,
/// thread-local for `init_for_tests`).
pub fn spawn_fd_line_reader<R>(
    reader: R,
    thread_prefix: &str,
    channel: &'static str,
    processor_id: &str,
) -> Option<JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    let proc_id = processor_id.to_string();
    let short = &proc_id[..8.min(proc_id.len())];
    let name = format!("{}-{}", thread_prefix, short);
    let (source, target): (&'static str, &'static str) = if thread_prefix.starts_with("py") {
        ("python", "streamlib::polyglot::python")
    } else {
        ("deno", "streamlib::polyglot::deno")
    };
    let dispatch = tracing::dispatcher::get_default(|d| d.clone());

    thread::Builder::new()
        .name(name)
        .spawn(move || {
            use std::io::BufRead;
            tracing::dispatcher::with_default(&dispatch, || {
                let reader = BufReader::new(reader);
                for line in reader.lines() {
                    match line {
                        Ok(text) if !text.is_empty() => {
                            emit_intercepted_line(target, channel, source, &proc_id, &text);
                        }
                        Err(_) => break,
                        _ => {}
                    }
                }
            });
        })
        .ok()
}

fn emit_intercepted_line(
    target: &'static str,
    channel: &'static str,
    source: &'static str,
    processor_id: &str,
    text: &str,
) {
    // `tracing` macros require a literal target, so dispatch on the two
    // known targets here. Fields are identical across both call sites.
    match target {
        "streamlib::polyglot::python" => tracing::warn!(
            target: "streamlib::polyglot::python",
            intercepted = true,
            channel = channel,
            source = source,
            processor_id = %processor_id,
            "{}",
            text
        ),
        _ => tracing::warn!(
            target: "streamlib::polyglot::deno",
            intercepted = true,
            channel = channel,
            source = source,
            processor_id = %processor_id,
            "{}",
            text
        ),
    }
}

fn thread_name(processor_id: &str) -> String {
    // Thread names are limited to 15 chars on Linux; truncate the
    // processor id the same way the Python stderr-forwarder thread does.
    let short = &processor_id[..8.min(processor_id.len())];
    format!("br-{}", short)
}

fn write_frame<W: Write>(writer: &mut W, msg: &serde_json::Value) -> Result<()> {
    let bytes = serde_json::to_vec(msg)
        .map_err(|e| Error::Runtime(format!("failed to serialize bridge message: {e}")))?;
    let len = bytes.len() as u32;
    writer
        .write_all(&len.to_be_bytes())
        .map_err(|e| Error::Runtime(format!("failed to write bridge frame: {e}")))?;
    writer
        .write_all(&bytes)
        .map_err(|e| Error::Runtime(format!("failed to write bridge frame: {e}")))?;
    writer
        .flush()
        .map_err(|e| Error::Runtime(format!("failed to flush bridge frame: {e}")))?;
    Ok(())
}

fn read_frame<R: Read>(reader: &mut R) -> Result<serde_json::Value> {
    let mut len_buf = [0u8; 4];
    reader
        .read_exact(&mut len_buf)
        .map_err(|e| Error::Runtime(format!("bridge read failed: {e}")))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .map_err(|e| Error::Runtime(format!("bridge read failed: {e}")))?;
    serde_json::from_slice(&buf)
        .map_err(|e| Error::Runtime(format!("bridge frame decode failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use std::sync::mpsc::RecvTimeoutError;

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::init_for_platform_sync() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                println!("{test_name}: no GPU device ({e}) — skipping");
                None
            }
        }
    }

    fn gpu_sandbox_or_skip(test_name: &str) -> Option<GpuContextLimitedAccess> {
        gpu_or_skip(test_name).map(GpuContextLimitedAccess::new)
    }

    fn log_frame() -> serde_json::Value {
        serde_json::json!({
            "rpc": "escalate_request",
            "op": "log",
            "source": "python",
            "source_seq": "1",
            "source_ts": "1970-01-01T00:00:00Z",
            "level": "info",
            "message": "hello from subprocess",
            "intercepted": false,
            "channel": serde_json::Value::Null,
            "pipeline_id": serde_json::Value::Null,
            "processor_id": "p-bridge-test",
            "attrs": {},
        })
    }

    // Regression gate for the fire-and-forget classification in `reader_loop`
    // (`if is_escalate_request { … } else { lifecycle_tx.send(msg) }`).
    //
    // Before the fix, `process_bridge_message` returning `None` for log ops
    // was indistinguishable from "this frame isn't an escalate request", so
    // the reader forwarded every log frame to the lifecycle channel. The
    // first host-side `bridge_recv()` after `setup` then saw the log frame
    // in place of `{"rpc":"ready"}` and reported `setup failed: unknown`.
    //
    // This test drives a real reader_loop over a `UnixStream::pair()` and
    // asserts that a log frame arriving on the bridge does not leak to
    // `lifecycle_rx`. Reverting the reader_loop classification change will
    // turn this test red.
    #[test]
    fn log_frame_does_not_leak_to_lifecycle_channel() {
        const TEST: &str = "log_frame_does_not_leak_to_lifecycle_channel";
        let Some(sandbox) = gpu_sandbox_or_skip(TEST) else {
            return;
        };

        let (parent_end, child_end) = UnixStream::pair().expect("socketpair");
        let bridge = SubprocessBridge::new(parent_end, sandbox, "p-bridge-test".into())
            .expect("bridge construction");

        // Keep the child stream alive across the entire test so the reader
        // loop stays in its read → classify → continue cycle instead of
        // hitting EOF and dropping `lifecycle_tx` (which would mask a real
        // leak as `Disconnected`).
        let mut child_writer = BufWriter::new(child_end);
        write_frame(&mut child_writer, &log_frame()).expect("write log frame");
        child_writer.flush().expect("flush");

        // If reader_loop regresses, the log frame arrives on lifecycle_rx
        // within a few ms. With the fix, it's consumed by
        // `process_bridge_message` and the lifecycle channel stays empty.
        let result = bridge.recv_lifecycle_timeout(Duration::from_millis(250));
        match result {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(frame) => panic!(
                "log frame leaked to lifecycle channel \
                 — reader_loop classification has regressed: {frame}"
            ),
            Err(RecvTimeoutError::Disconnected) => panic!(
                "bridge reader exited before the test could assert — \
                 check for panics in the reader thread"
            ),
        }

        // Hold the child side open until the assertion completes.
        drop(child_writer);
    }

    // Positive control: a genuine lifecycle frame MUST arrive on
    // `lifecycle_rx`. Pairs with the negative test above to prove the
    // classification is a shift, not a blanket block.
    #[test]
    fn lifecycle_frame_still_routes_to_lifecycle_channel() {
        const TEST: &str = "lifecycle_frame_still_routes_to_lifecycle_channel";
        let Some(sandbox) = gpu_sandbox_or_skip(TEST) else {
            return;
        };

        let (parent_end, child_end) = UnixStream::pair().expect("socketpair");
        let bridge = SubprocessBridge::new(parent_end, sandbox, "p-bridge-test".into())
            .expect("bridge construction");

        let ready = serde_json::json!({"rpc": "ready"});
        let mut child_writer = BufWriter::new(child_end);
        write_frame(&mut child_writer, &ready).expect("write ready frame");
        drop(child_writer);

        let got = bridge
            .recv_lifecycle_timeout(Duration::from_millis(500))
            .expect("lifecycle frame must route through");
        assert_eq!(got.get("rpc").and_then(|v| v.as_str()), Some("ready"));
    }

    // SDK → engine handshake gate. The whole point of the version handshake is
    // that an incompatible installed SDK is refused at setup, not run. Mentally
    // revert `validate_subprocess_protocol` to `Ok(())` and every assertion
    // below that expects an `Err` goes green for the wrong reason — so these
    // lock the gate, not just exercise it.
    #[test]
    fn subprocess_protocol_gate_accepts_supported_and_rejects_others() {
        // Current engine version is in range → accepted.
        assert!(
            validate_subprocess_protocol(Some(STREAMLIB_SUBPROCESS_PROTOCOL_VERSION), "p",).is_ok()
        );
        // The minimum supported version is in range → accepted.
        assert!(validate_subprocess_protocol(Some(MIN_SUPPORTED_SUBPROCESS_PROTOCOL), "p").is_ok());

        // One past the engine's current version → refused (SDK too new).
        let too_new = validate_subprocess_protocol(
            Some(STREAMLIB_SUBPROCESS_PROTOCOL_VERSION + 1),
            "p-too-new",
        );
        assert!(
            too_new.is_err(),
            "an SDK newer than the engine must be refused"
        );
        assert!(
            too_new
                .unwrap_err()
                .to_string()
                .contains("protocol mismatch")
        );

        // Below the minimum supported version → refused (SDK too old).
        if MIN_SUPPORTED_SUBPROCESS_PROTOCOL > 0 {
            assert!(
                validate_subprocess_protocol(
                    Some(MIN_SUPPORTED_SUBPROCESS_PROTOCOL - 1),
                    "p-too-old",
                )
                .is_err(),
                "an SDK older than the engine's minimum must be refused"
            );
        }

        // No version reported at all (an SDK predating the handshake) → refused.
        let missing = validate_subprocess_protocol(None, "p-missing");
        assert!(
            missing.is_err(),
            "a missing SDK protocol version must be refused"
        );
        assert!(
            missing
                .unwrap_err()
                .to_string()
                .contains("did not report a protocol version")
        );
    }

    /// The crash-path mirror of the explicit `release_handle` op (#1901): a
    /// helper that dies without releasing its escalate acquires must not
    /// leave their surface-share registrations behind. The disconnect
    /// watchdog rightly never reclaims them — the host's own connection
    /// registered them on the helper's behalf, and the watchdog skips
    /// same-process peers — so bridge teardown is the only reclaimer.
    ///
    /// Each cycle acquires both kinds the drain distinguishes — a pixel
    /// buffer and a texture, the latter carrying the produce/consume
    /// timeline-fd pair the ticket names — and runs two crash-respawn
    /// cycles because the leak's bite is accumulation across respawns.
    /// Asserts the post-teardown table equals the post-acquire table minus
    /// exactly the helper's handles, so teardown is also shown to leave
    /// the pool's own long-lived registrations alone. Mental-revert: drop
    /// the surface-share half from the `SubprocessBridge::drop` drain and
    /// the post-drop assertion goes red on the first cycle.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn bridge_drop_releases_a_crashed_helpers_surface_share_registrations() {
        const TEST: &str = "bridge_drop_releases_a_crashed_helpers_surface_share_registrations";
        use std::collections::HashSet;

        use crate::core::context::SurfaceStore;
        use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};

        let Some(gpu) = gpu_or_skip(TEST) else {
            return;
        };

        let state = SurfaceShareState::new();
        let socket_dir = tempfile::TempDir::new().expect("socket dir");
        let socket_path = socket_dir.path().join("bridge-teardown.sock");
        let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
        service.start().expect("service start");

        let runtime_id = "bridge-teardown-test-runtime";
        let store = SurfaceStore::new(
            socket_path.to_string_lossy().into_owned(),
            runtime_id.to_string(),
        );
        store
            .connect()
            .expect("connect to the test surface-share service");
        gpu.set_surface_store(store);
        let sandbox = GpuContextLimitedAccess::new(gpu);
        let registered_surface_ids = || -> HashSet<String> {
            state
                .surface_ids_by_runtime(runtime_id)
                .into_iter()
                .collect()
        };

        for cycle in 0..2 {
            let (parent_end, child_end) = UnixStream::pair().expect("socketpair");
            let bridge =
                SubprocessBridge::new(parent_end, sandbox.clone(), format!("p-crash-{cycle}"))
                    .expect("bridge construction");

            let mut child_writer = BufWriter::new(child_end.try_clone().expect("clone child end"));
            let mut child_reader = BufReader::new(child_end);
            let mut acquire_via_bridge = |request: serde_json::Value| -> String {
                let op = request["op"]
                    .as_str()
                    .expect("request names an op")
                    .to_string();
                write_frame(&mut child_writer, &request).expect("write acquire frame");
                let response = read_frame(&mut child_reader).expect("acquire response");
                assert_eq!(
                    response.get("result").and_then(|v| v.as_str()),
                    Some("ok"),
                    "cycle {cycle}: {op} failed: {response}"
                );
                response
                    .get("handle_id")
                    .and_then(|v| v.as_str())
                    .expect("ok response carries a handle_id")
                    .to_string()
            };
            let pixel_buffer_handle_id = acquire_via_bridge(serde_json::json!({
                "rpc": "escalate_request",
                "op": "acquire_pixel_buffer",
                "request_id": format!("r-crash-buffer-{cycle}"),
                "width": 64,
                "height": 64,
                "format": "bgra",
            }));
            let texture_handle_id = acquire_via_bridge(serde_json::json!({
                "rpc": "escalate_request",
                "op": "acquire_texture",
                "request_id": format!("r-crash-texture-{cycle}"),
                "width": 64,
                "height": 64,
                "format": "rgba8_unorm",
                "usage": ["texture_binding", "copy_src"],
            }));

            // The acquires register their check-in ids, and (first cycle
            // only) create the pixel-buffer pool, whose pre-allocated slots
            // register themselves for cross-process lookup. Those pool
            // registrations live as long as the runtime — teardown must
            // release the helper's acquires and leave them alone.
            let registered_after_acquire = registered_surface_ids();
            for helper_handle_id in [&pixel_buffer_handle_id, &texture_handle_id] {
                assert!(
                    registered_after_acquire.contains(helper_handle_id),
                    "cycle {cycle}: the acquire must have registered '{helper_handle_id}' \
                     with the surface-share service"
                );
            }

            // The crash: the helper dies without a release_handle; bridge
            // teardown is everything that runs.
            drop(child_writer);
            drop(child_reader);
            drop(bridge);

            let mut expected_after_teardown = registered_after_acquire.clone();
            expected_after_teardown.remove(&pixel_buffer_handle_id);
            expected_after_teardown.remove(&texture_handle_id);
            let registered_after_teardown = registered_surface_ids();
            assert_eq!(
                registered_after_teardown, expected_after_teardown,
                "cycle {cycle}: bridge teardown must release the crashed helper's \
                 surface-share registrations and nothing else"
            );
        }

        service.stop();
    }
}
