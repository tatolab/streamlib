// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Length-prefixed JSON stdio bridge shared by the Python and Deno
//! subprocess host processors.
//!
//! Two roles travel over the same pair of pipes:
//! 1. Lifecycle RPC (`setup`, `run`, `stop`, `teardown`, `on_pause`,
//!    `on_resume`, …) — initiated by the host, the subprocess replies with
//!    `rpc: "ready" | "stopped" | "ok" | "done" | "error"`.
//! 2. Escalate-on-behalf (`rpc: "escalate_request"`) — initiated by the
//!    subprocess, the host replies with `rpc: "escalate_response"`.
//!
//! A dedicated reader thread (`bridge-reader-…`) owns the subprocess stdout
//! and demultiplexes incoming messages: escalate requests are dispatched
//! inline through [`subprocess_escalate::process_bridge_message`], and
//! anything else is forwarded to the main thread over an mpsc channel for
//! the lifecycle RPC to consume. Writes in both directions serialize through
//! a shared `Arc<Mutex<BufWriter<ChildStdin>>>` so the main thread and the
//! reader thread can't interleave halves of a length-prefixed frame.

use std::io::{BufReader, BufWriter, Read, Write};
use std::process::{ChildStdin, ChildStdout};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::core::context::GpuContextLimitedAccess;
use crate::core::error::{Result, StreamError};

use super::subprocess_escalate::{process_bridge_message, EscalateHandleRegistry};

/// Shared writer handle. The host's lifecycle path and the reader thread's
/// escalate-response path both write through this mutex.
type SharedWriter = Arc<Mutex<BufWriter<ChildStdin>>>;

/// Bridge for one subprocess. Drop the value to tear the reader thread down
/// cleanly (stdin close propagates EOF; reader thread exits).
pub(crate) struct SubprocessBridge {
    processor_id: String,
    stdin: SharedWriter,
    lifecycle_rx: Receiver<serde_json::Value>,
    registry: Arc<EscalateHandleRegistry>,
    reader: Option<JoinHandle<()>>,
    dead: Arc<Mutex<bool>>,
}

impl SubprocessBridge {
    /// Wrap the subprocess pipes and spawn the reader thread.
    ///
    /// `sandbox` is cloned into the reader thread so escalate requests can
    /// be dispatched without blocking the main thread. `processor_id` is
    /// used for thread naming and tracing.
    pub(crate) fn new(
        stdin: ChildStdin,
        stdout: ChildStdout,
        sandbox: GpuContextLimitedAccess,
        processor_id: String,
    ) -> Self {
        let stdin: SharedWriter = Arc::new(Mutex::new(BufWriter::new(stdin)));
        let registry = EscalateHandleRegistry::new();
        let (tx, rx) = mpsc::channel();
        let dead = Arc::new(Mutex::new(false));

        let thread_name = thread_name(&processor_id);
        let reader_stdin = Arc::clone(&stdin);
        let reader_registry = Arc::clone(&registry);
        let reader_dead = Arc::clone(&dead);
        let reader_processor_id = processor_id.clone();

        let reader = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                reader_loop(
                    BufReader::new(stdout),
                    reader_stdin,
                    sandbox,
                    reader_registry,
                    tx,
                    reader_dead,
                    reader_processor_id,
                );
            })
            .expect("failed to spawn bridge reader thread");

        Self {
            processor_id,
            stdin,
            lifecycle_rx: rx,
            registry,
            reader: Some(reader),
            dead,
        }
    }

    /// Write a length-prefixed JSON message to the subprocess stdin.
    pub(crate) fn send(&self, msg: &serde_json::Value) -> Result<()> {
        if self.is_dead() {
            return Err(StreamError::Runtime(format!(
                "[{}] bridge marked dead, cannot send",
                self.processor_id
            )));
        }
        let mut writer = self
            .stdin
            .lock()
            .map_err(|_| StreamError::Runtime("subprocess stdin mutex poisoned".to_string()))?;
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
                "[{}] subprocess stdout closed before reply",
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
        // Dropping stdin (and its Arc) isn't enough on its own because the
        // reader thread holds a clone. We can't force the subprocess to
        // close its stdout from this side, but the host processor's
        // teardown() has already sent the teardown command and waited for
        // the reply — at this point the subprocess has exited and the
        // reader thread will see EOF. Join with a short timeout via
        // `join()` on the handle we stashed.
        if let Some(reader) = self.reader.take() {
            // Detach rather than join; the OS reaps the thread when the
            // process exits. A blocking join here would deadlock if the
            // subprocess is stuck and stdout hasn't closed.
            drop(reader);
        }
    }
}

/// Reader loop: drain stdout, dispatch escalate traffic, forward lifecycle
/// responses to `lifecycle_tx`.
fn reader_loop(
    mut stdout: BufReader<ChildStdout>,
    stdin: SharedWriter,
    sandbox: GpuContextLimitedAccess,
    registry: Arc<EscalateHandleRegistry>,
    lifecycle_tx: mpsc::Sender<serde_json::Value>,
    dead: Arc<Mutex<bool>>,
    processor_id: String,
) {
    loop {
        let msg = match read_frame(&mut stdout) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("[{}] bridge reader exiting: {}", processor_id, e);
                if let Ok(mut dead) = dead.lock() {
                    *dead = true;
                }
                break;
            }
        };

        if let Some(response) = process_bridge_message(&sandbox, &registry, &msg) {
            // Escalate request handled inline. Write response with the
            // shared stdin lock.
            let send_result: Result<()> = {
                let mut writer = match stdin.lock() {
                    Ok(g) => g,
                    Err(_) => {
                        tracing::warn!("[{}] bridge reader saw poisoned stdin mutex", processor_id);
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
            continue;
        }

        // Lifecycle response — forward to main thread. Send failure means
        // the receiver is gone (host dropped), exit cleanly.
        if lifecycle_tx.send(msg).is_err() {
            tracing::debug!(
                "[{}] bridge reader exiting: lifecycle channel dropped",
                processor_id
            );
            break;
        }
    }
}

fn thread_name(processor_id: &str) -> String {
    // Thread names are limited to 15 chars on Linux; truncate the processor
    // id the same way the Python stderr-forwarder thread does.
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
