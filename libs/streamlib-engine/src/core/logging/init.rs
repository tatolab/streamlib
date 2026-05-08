// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Install the unified logging pathway: tracing subscriber (env filter +
//! JSONL sink layer) + optional batched JSONL file writer + drain worker
//! thread. Returns a [`StreamlibLoggingGuard`] whose `Drop` flushes
//! buffered records and `fdatasync`s the JSONL file.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use tracing::Dispatch;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Registry};

use crate::core::logging::config::{ResolvedTunables, StreamlibLoggingConfig};
use crate::core::logging::event::Source;
use crate::core::logging::layer::JsonlSinkLayer;
use crate::core::logging::paths::runtime_log_path;
use crate::core::logging::polyglot_sink::{self, PolyglotLogSink};
#[cfg(unix)]
use crate::core::logging::stdio_interceptor::{self, StdioInterceptor};
use crate::core::logging::worker::{spawn as spawn_worker, WorkerConfig, WorkerHandle, WorkerSignal};
use crate::core::logging::writer::JsonlBatchedWriter;

/// Drops a little later than most — on `Drop`, flushes and `fdatasync`s
/// the JSONL writer, joins the drain worker thread, then releases
/// resources.
pub struct StreamlibLoggingGuard {
    worker: Option<WorkerHandle>,
    jsonl_path: Option<PathBuf>,
    /// Thread-local scope guard for test-mode installations. Dropped
    /// before the worker so no more events arrive during shutdown.
    default_scope: Option<tracing::dispatcher::DefaultGuard>,
    /// Fd-level stdio interceptor (if installed). Dropped BEFORE the
    /// worker so the reader threads' tail events drain into the
    /// worker queue before shutdown.
    #[cfg(unix)]
    interceptor: Option<StdioInterceptor>,
}

impl StreamlibLoggingGuard {
    fn noop() -> Self {
        Self {
            worker: None,
            jsonl_path: None,
            default_scope: None,
            #[cfg(unix)]
            interceptor: None,
        }
    }

    /// Path of the JSONL log file this runtime is writing to, if any.
    pub fn jsonl_path(&self) -> Option<&std::path::Path> {
        self.jsonl_path.as_deref()
    }

    /// Request a best-effort flush without shutting down the worker.
    /// Safe to call from panic hooks or signal handlers.
    pub fn request_flush(&self) {
        if let Some(w) = self.worker.as_ref() {
            w.request_flush();
        }
    }
}

impl Drop for StreamlibLoggingGuard {
    fn drop(&mut self) {
        // Restore the previous thread-local dispatcher first so no new
        // events reach our queue while we're draining.
        drop(self.default_scope.take());
        // Clear the polyglot sink before shutting the worker down so
        // late-arriving escalate-IPC log pushes can't race a dead queue.
        polyglot_sink::uninstall();
        // Drop the interceptor before the worker: restoring fds 1/2
        // unblocks the reader threads with EOF, so their final
        // intercepted events land in the worker queue while the
        // worker is still draining.
        #[cfg(unix)]
        drop(self.interceptor.take());
        if let Some(mut worker) = self.worker.take() {
            worker.shutdown_and_join();
        }
    }
}

static GLOBAL_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Install the logging pathway as the **global** tracing subscriber.
/// First-caller wins: subsequent calls return a no-op guard and the
/// original subscriber stays live. Used by production entrypoints
/// (`StreamRuntime::new`, `streamlib-cli`, `streamlib-runtime`).
pub fn init(config: StreamlibLoggingConfig) -> Result<StreamlibLoggingGuard> {
    if GLOBAL_INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(StreamlibLoggingGuard::noop());
    }

    let (dispatch, guard) = build_components(config)?;

    tracing::dispatcher::set_global_default(dispatch)
        .map_err(|e| anyhow::anyhow!("set_global_default failed: {}", e))?;

    if let Some(w) = guard.worker.as_ref() {
        install_panic_hook(w);
    }

    Ok(guard)
}

/// Install the logging pathway as a **thread-local** default subscriber.
/// Used by tests that run with `#[serial]` to avoid global-subscriber
/// conflicts and by criterion benches measuring hot-path latency. The
/// returned guard holds a [`tracing::dispatcher::DefaultGuard`]; drop
/// order is: default guard first, drain worker after, so no tracing
/// events race the shutdown.
pub fn init_for_tests(config: StreamlibLoggingConfig) -> Result<StreamlibLoggingGuard> {
    let (dispatch, mut guard) = build_components(config)?;
    let default_scope = tracing::dispatcher::set_default(&dispatch);
    guard.default_scope = Some(default_scope);
    Ok(guard)
}

fn build_components(
    config: StreamlibLoggingConfig,
) -> Result<(Dispatch, StreamlibLoggingGuard)> {
    let tunables = ResolvedTunables::from_config(&config.tunables);

    let (writer, jsonl_path) = match (&config.runtime_id, config.jsonl) {
        (Some(rid), true) => {
            let started_at_millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let path = runtime_log_path(rid.as_str(), started_at_millis);
            match JsonlBatchedWriter::open(
                &path,
                tunables.batch_bytes,
                tunables.fsync_on_every_batch,
            ) {
                Ok(w) => (Some(w), Some(path)),
                Err(e) => {
                    // Pre-init error path: the tracing subscriber we're trying to
                    // build owns this error — emit via raw stderr before giving up.
                    #[allow(clippy::disallowed_macros)]
                    {
                        eprintln!(
                            "streamlib::logging: failed to open JSONL file {}: {} — continuing with stdout only",
                            path.display(),
                            e
                        );
                    }
                    (None, None)
                }
            }
        }
        _ => (None, None),
    };

    let stdout_enabled = config.effective_stdout();

    // Install fd redirects before spawning the worker so the dup'd
    // real-stdout handle can be handed to the worker as its pretty-
    // mirror sink. Reader threads are started AFTER the dispatch is
    // built so the intercepted events route through the right
    // subscriber.
    #[cfg(unix)]
    let (pending_interceptor, mut real_stdout_file) = if config.intercept_stdio {
        match stdio_interceptor::install_redirects() {
            Ok((pending, files)) => {
                // stderr mirror not wired today — dropping the file
                // closes the dup'd fd cleanly.
                drop(files.real_stderr);
                (Some(pending), Some(files.real_stdout))
            }
            Err(e) => {
                // Pre-init error path: interceptor failed before the subscriber exists.
                #[allow(clippy::disallowed_macros)]
                {
                    eprintln!(
                        "streamlib::logging: failed to install stdio interceptor: {} — continuing without interception",
                        e
                    );
                }
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    #[cfg(not(unix))]
    let mut real_stdout_file: Option<std::fs::File> = None;

    let stdout_sink: Option<Box<dyn std::io::Write + Send>> = if !stdout_enabled {
        None
    } else if let Some(file) = real_stdout_file.take() {
        Some(Box::new(file))
    } else {
        Some(Box::new(std::io::stdout()))
    };

    let worker = spawn_worker(WorkerConfig {
        runtime_id: config.runtime_id.clone(),
        source: Source::Rust,
        tunables,
        stdout_sink,
        writer,
    });

    // Install the process-wide polyglot sink so escalate-IPC log records
    // relayed from Python/Deno subprocesses converge on the same drain
    // worker as local tracing events. See [`polyglot_sink`] for why this
    // bypasses `tracing::*!()` rather than routing through it.
    polyglot_sink::install(Arc::new(PolyglotLogSink::from_worker(&worker)));

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let layer = JsonlSinkLayer::new(
        Arc::clone(&worker.queue),
        worker.doorbell.clone(),
        Arc::clone(&worker.dropped),
    );
    let subscriber = Registry::default().with(env_filter).with(layer);
    let dispatch = Dispatch::new(subscriber);

    #[cfg(unix)]
    let interceptor = pending_interceptor.map(|p| p.start_readers(dispatch.clone()));

    let guard = StreamlibLoggingGuard {
        worker: Some(worker),
        jsonl_path,
        default_scope: None,
        #[cfg(unix)]
        interceptor,
    };

    Ok((dispatch, guard))
}

/// Install a panic hook that requests a best-effort flush from the drain
/// worker before the default panic behavior runs. Composes with any
/// previously installed hook.
fn install_panic_hook(worker: &WorkerHandle) {
    let doorbell = worker.doorbell.clone();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = doorbell.try_send(WorkerSignal::Flush);
        std::thread::sleep(std::time::Duration::from_millis(50));
        previous(info);
    }));
}

