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
use crate::core::error::{Result, StreamError};

use super::subprocess_escalate::{process_bridge_message, EscalateHandleRegistry};

/// Env var advertising the inherited child-end fd number of the escalate
/// socketpair. The subprocess opens this fd as a duplex UNIX socket and
/// uses it as the framed-IPC transport.
pub(crate) const ESCALATE_FD_ENV: &str = "STREAMLIB_ESCALATE_FD";

/// Socketpair-backed escalate IPC transport. The parent holds one half
/// and the subprocess inherits the other via [`ESCALATE_FD_ENV`].
pub(crate) struct EscalateTransport {
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
    pub(crate) fn attach(command: &mut Command) -> Result<Self> {
        let (parent_end, child_end) = UnixStream::pair().map_err(|e| {
            StreamError::Runtime(format!("failed to create escalate socketpair: {e}"))
        })?;

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
    pub(crate) fn release_child_end(&mut self) {
        self.child_end.take();
    }

    /// Consume the transport and return the parent-side [`UnixStream`].
    pub(crate) fn into_parent_stream(mut self) -> UnixStream {
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
pub(crate) struct SubprocessBridge {
    processor_id: String,
    writer: SharedWriter,
    lifecycle_rx: Receiver<serde_json::Value>,
    registry: Arc<EscalateHandleRegistry>,
    reader: Option<JoinHandle<()>>,
    dead: Arc<Mutex<bool>>,
}

impl SubprocessBridge {
    /// Wrap a socketpair parent end and spawn the reader thread.
    ///
    /// `sandbox` is cloned into the reader thread so escalate requests
    /// can be dispatched without blocking the main thread. `processor_id`
    /// is used for thread naming and tracing.
    pub(crate) fn new(
        stream: UnixStream,
        sandbox: GpuContextLimitedAccess,
        processor_id: String,
    ) -> Result<Self> {
        let read_half = stream.try_clone().map_err(|e| {
            StreamError::Runtime(format!(
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
            reader: Some(reader),
            dead,
        })
    }

    /// Write a length-prefixed JSON message to the subprocess.
    pub(crate) fn send(&self, msg: &serde_json::Value) -> Result<()> {
        if self.is_dead() {
            return Err(StreamError::Runtime(format!(
                "[{}] bridge marked dead, cannot send",
                self.processor_id
            )));
        }
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| StreamError::Runtime("subprocess writer mutex poisoned".to_string()))?;
        write_frame(&mut *writer, msg).map_err(|e| {
            self.mark_dead();
            e
        })
    }

    /// Block until the next lifecycle-tagged message arrives.
    pub(crate) fn recv_lifecycle(&self) -> Result<serde_json::Value> {
        self.lifecycle_rx.recv().map_err(|_| {
            self.mark_dead();
            StreamError::Runtime(format!(
                "[{}] subprocess escalate socket closed before reply",
                self.processor_id
            ))
        })
    }

    /// Block up to `timeout` for the next lifecycle-tagged message.
    pub(crate) fn recv_lifecycle_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<serde_json::Value, RecvTimeoutError> {
        self.lifecycle_rx.recv_timeout(timeout)
    }

    /// Mark the bridge dead; subsequent sends return immediately.
    pub(crate) fn mark_dead(&self) {
        if let Ok(mut dead) = self.dead.lock() {
            *dead = true;
        }
    }

    pub(crate) fn is_dead(&self) -> bool {
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
        self.registry.clear();
        // Shut down the write half so the reader thread sees EOF on its
        // clone if the subprocess is still alive. The OS reaps the
        // thread on process exit; we avoid blocking on join.
        if let Ok(writer) = self.writer.lock() {
            let _ = writer.get_ref().shutdown(std::net::Shutdown::Both);
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
        let is_escalate_request = msg
            .get("rpc")
            .and_then(|v| v.as_str())
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
pub(crate) fn spawn_fd_line_reader<R>(
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
    let (source, target): (&'static str, &'static str) =
        if thread_prefix.starts_with("py") {
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
                            emit_intercepted_line(
                                target, channel, source, &proc_id, &text,
                            );
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
        .map_err(|e| StreamError::Runtime(format!("failed to serialize bridge message: {e}")))?;
    let len = bytes.len() as u32;
    writer
        .write_all(&len.to_be_bytes())
        .map_err(|e| StreamError::Runtime(format!("failed to write bridge frame: {e}")))?;
    writer
        .write_all(&bytes)
        .map_err(|e| StreamError::Runtime(format!("failed to write bridge frame: {e}")))?;
    writer
        .flush()
        .map_err(|e| StreamError::Runtime(format!("failed to flush bridge frame: {e}")))?;
    Ok(())
}

fn read_frame<R: Read>(reader: &mut R) -> Result<serde_json::Value> {
    let mut len_buf = [0u8; 4];
    reader
        .read_exact(&mut len_buf)
        .map_err(|e| StreamError::Runtime(format!("bridge read failed: {e}")))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .map_err(|e| StreamError::Runtime(format!("bridge read failed: {e}")))?;
    serde_json::from_slice(&buf)
        .map_err(|e| StreamError::Runtime(format!("bridge frame decode failed: {e}")))
}
