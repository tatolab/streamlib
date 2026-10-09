// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Install the unified logging pathway: one tracing subscriber per process
//! (env filter + the layer that routes each record to the stream that
//! emitted it) and the drain worker thread behind it, installed once and kept
//! for the process's life.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use parking_lot::Mutex;
use tracing::Dispatch;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::{EnvFilter, Registry};

use crate::core::logging::config::{ResolvedTunables, StreamlibLoggingConfig};
use crate::core::logging::layer::JsonlSinkLayer;
#[cfg(unix)]
use crate::core::logging::stdio_interceptor::{self, StdioInterceptor};
use crate::core::logging::worker::{
    DrainWorkerRecordQueue, WorkerConfig, WorkerHandle, WorkerSignal, spawn as spawn_worker,
};
use streamlib_runtime_client_contract::runtime_log_event::Source;

/// How long dropping the last hold on the process pathway waits for the drain
/// worker to write what is queued.
const QUEUED_RECORDS_WRITTEN_AS_THE_LAST_HOLD_DROPS_BUDGET: Duration = Duration::from_secs(2);

/// Env-var escape hatch. When set to `1` / `true`, streamlib skips
/// **all** logging initialization (worker, stream JSONL files, stdio
/// interceptor, polyglot sink, panic hook) and defers ownership of
/// the global `tracing` subscriber to whatever the host process has
/// already installed.
///
/// Use cases:
///
/// - **Throughput benchmarks** that need to install their own
///   subscriber to capture per-processor counter events.
/// - **Host applications** that own their own tracing setup
///   end-to-end and don't want streamlib's defaults.
///
/// **Demotions when set** — these features rely on the streamlib
/// worker existing, so opting in disables them:
///
/// - Each stream's JSONL log (`<project>/.streamlib/logs/`).
/// - Stdio interception that converges python/deno cdylib subprocess
///   stderr onto the same tracing dispatch as Rust events.
/// - The streamlib panic hook that funnels Rust panics through the
///   tracing worker.
///
/// The `DANGEROUSLY_` prefix is there to make the trade-off
/// unmistakable to anyone reading a runbook or `env | grep`.
pub const DEFER_LOGGING_TO_HOST_ENV: &str = "STREAMLIB_DANGEROUSLY_DEFER_LOGGING_TO_HOST";

fn host_owns_logging() -> bool {
    matches!(
        std::env::var(DEFER_LOGGING_TO_HOST_ENV).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE")
    )
}

/// The process's logging pathway: its drain worker, its dispatcher, and the
/// stdio interception its holds keep installed.
struct ProcessLoggingPathway {
    record_queue: DrainWorkerRecordQueue,
    /// Never joined: the worker drains for the process's life.
    _drain_worker: WorkerHandle,
    #[cfg_attr(not(unix), allow(dead_code))]
    dispatch: Dispatch,
    #[cfg_attr(not(unix), allow(dead_code))]
    intercepts_the_standard_streams: bool,
    holds: Mutex<HoldsOnTheProcessLoggingPathway>,
}

/// How many holds the pathway has, and the interception they keep installed.
#[derive(Default)]
struct HoldsOnTheProcessLoggingPathway {
    hold_count: usize,
    #[cfg(unix)]
    stdio_interceptor: Option<StdioInterceptor>,
}

/// Whether this process's pathway is installed.
enum ProcessLoggingPathwayInstallation {
    NotInstalled,
    DeferredToTheHost,
    Installed(Arc<ProcessLoggingPathway>),
}

static PROCESS_LOGGING_PATHWAY: Mutex<ProcessLoggingPathwayInstallation> =
    Mutex::new(ProcessLoggingPathwayInstallation::NotInstalled);

/// A hold on the process's logging pathway: while any hold lives, the
/// process's standard streams are intercepted (when the pathway was installed
/// to intercept them). Dropping the last hold restores them and waits for the
/// drain worker to write every queued record.
pub struct ProcessLoggingPathwayHold {
    pathway: Option<Arc<ProcessLoggingPathway>>,
}

impl ProcessLoggingPathwayHold {
    /// Hand fds 1 and 2 back to the process now, before this hold drops.
    ///
    /// For an engine that is never dropped, so whatever the process writes to
    /// its standard streams on its way out — an error naming what kept the
    /// engine alive — reaches them rather than a reader that may not forward
    /// it before the process ends. Idempotent.
    pub fn stop_intercepting_the_standard_streams(&self) {
        #[cfg(unix)]
        if let Some(pathway) = &self.pathway {
            let stdio_interceptor = pathway.holds.lock().stdio_interceptor.take();
            drop(stdio_interceptor);
        }
    }
}

impl Drop for ProcessLoggingPathwayHold {
    fn drop(&mut self) {
        let Some(pathway) = self.pathway.take() else {
            return;
        };
        #[cfg(unix)]
        let stdio_interceptor_of_the_last_hold = {
            let mut holds = pathway.holds.lock();
            holds.hold_count -= 1;
            if holds.hold_count == 0 {
                holds.stdio_interceptor.take()
            } else {
                None
            }
        };
        #[cfg(not(unix))]
        {
            pathway.holds.lock().hold_count -= 1;
        }
        // Restored before the wait: the readers' last lines reach the queue
        // as they see end of file.
        #[cfg(unix)]
        drop(stdio_interceptor_of_the_last_hold);
        pathway
            .record_queue
            .wait_until_every_queued_record_is_written(
                QUEUED_RECORDS_WRITTEN_AS_THE_LAST_HOLD_DROPS_BUDGET,
            );
    }
}

/// Install the process's logging pathway as the **global** tracing
/// subscriber the first time it is called, and hold it. The pathway is the
/// process's, kept for its life: a later call holds the one already
/// installed, `config` unread. Used by production entrypoints (`Runner::new`).
///
/// When [`DEFER_LOGGING_TO_HOST_ENV`] is set, nothing is installed and the
/// host process keeps full control of the global subscriber. See the
/// constant's docs for the precise behavioral demotions.
pub fn hold_the_process_logging_pathway(
    config: StreamlibLoggingConfig,
) -> Result<ProcessLoggingPathwayHold> {
    let pathway = {
        let mut installation = PROCESS_LOGGING_PATHWAY.lock();
        match &*installation {
            ProcessLoggingPathwayInstallation::Installed(pathway) => Arc::clone(pathway),
            ProcessLoggingPathwayInstallation::DeferredToTheHost => {
                return Ok(ProcessLoggingPathwayHold { pathway: None });
            }
            ProcessLoggingPathwayInstallation::NotInstalled if host_owns_logging() => {
                *installation = ProcessLoggingPathwayInstallation::DeferredToTheHost;
                // Raw stderr: the subscriber is the host's, not ours to use.
                #[allow(clippy::disallowed_macros)]
                {
                    eprintln!(
                        "streamlib::logging: {}=1 — deferring all logging initialization to the \
                         host process (stream JSONL logs, stdio interception, and panic hook are \
                         disabled)",
                        DEFER_LOGGING_TO_HOST_ENV,
                    );
                }
                return Ok(ProcessLoggingPathwayHold { pathway: None });
            }
            ProcessLoggingPathwayInstallation::NotInstalled => {
                let pathway = Arc::new(install_the_process_logging_pathway(config)?);
                *installation = ProcessLoggingPathwayInstallation::Installed(Arc::clone(&pathway));
                pathway
            }
        }
    };

    let mut holds = pathway.holds.lock();
    holds.hold_count += 1;
    #[cfg(unix)]
    if holds.hold_count == 1 && pathway.intercepts_the_standard_streams {
        holds.stdio_interceptor = match stdio_interceptor::install_redirects() {
            Ok(pending) => Some(pending.start_readers(pathway.dispatch.clone())),
            Err(e) => {
                tracing::warn!(
                    "the standard streams are not intercepted: the redirect failed to install: {e}"
                );
                None
            }
        };
    }
    drop(holds);
    Ok(ProcessLoggingPathwayHold {
        pathway: Some(pathway),
    })
}

fn install_the_process_logging_pathway(
    config: StreamlibLoggingConfig,
) -> Result<ProcessLoggingPathway> {
    let tunables = ResolvedTunables::from_config(&config.tunables);
    let (mut drain_worker, dispatch) = build_components(&config, tunables, config.intercept_stdio);
    if let Err(already_installed) = tracing::dispatcher::set_global_default(dispatch.clone()) {
        drain_worker.shutdown_and_join();
        return Err(anyhow::anyhow!(
            "set_global_default failed: {already_installed}"
        ));
    }
    install_panic_hook(&drain_worker.record_queue);
    Ok(ProcessLoggingPathway {
        record_queue: drain_worker.record_queue.clone(),
        _drain_worker: drain_worker,
        dispatch,
        intercepts_the_standard_streams: config.intercept_stdio,
        holds: Mutex::new(HoldsOnTheProcessLoggingPathway::default()),
    })
}

/// Ask the process pathway's drain worker to flush what it holds, without
/// waiting for it.
///
/// For a path that is about to `_exit`, where no hold will ever drop.
///
/// Never blocks and never panics: its callers are the paths that must not.
pub(crate) fn request_a_best_effort_flush() {
    let Some(installation) = PROCESS_LOGGING_PATHWAY.try_lock() else {
        return;
    };
    if let ProcessLoggingPathwayInstallation::Installed(pathway) = &*installation {
        pathway.record_queue.request_flush();
    }
}

/// A logging pathway installed as one thread's default subscriber, for
/// tests and benches. On `Drop`, restores the thread's previous dispatcher,
/// then writes every queued record, `fdatasync`s every stream file it wrote
/// to, and joins its drain worker.
pub struct StreamlibLoggingGuard {
    worker: Option<WorkerHandle>,
    default_scope: Option<tracing::dispatcher::DefaultGuard>,
    #[cfg_attr(not(unix), allow(dead_code))]
    dispatch: Dispatch,
    /// Dropped BEFORE the worker so the reader threads' tail events drain
    /// into the worker queue before shutdown.
    #[cfg(unix)]
    interceptor: Option<StdioInterceptor>,
}

impl StreamlibLoggingGuard {
    /// Request a best-effort flush without shutting down the worker.
    pub fn request_flush(&self) {
        if let Some(w) = self.worker.as_ref() {
            w.record_queue.request_flush();
        }
    }

    /// Intercept the standard streams until this guard drops, the readers
    /// carrying the stream route of the calling thread.
    #[cfg(unix)]
    pub(crate) fn intercept_the_standard_streams_carrying_this_threads_route(
        &mut self,
    ) -> Result<()> {
        self.interceptor =
            Some(stdio_interceptor::install_redirects()?.start_readers(self.dispatch.clone()));
        Ok(())
    }
}

impl Drop for StreamlibLoggingGuard {
    fn drop(&mut self) {
        drop(self.default_scope.take());
        #[cfg(unix)]
        drop(self.interceptor.take());
        if let Some(mut worker) = self.worker.take() {
            worker.shutdown_and_join();
        }
    }
}

/// Install a logging pathway as the calling thread's default subscriber.
/// Used by tests that run with `#[serial]` to stay off the process pathway
/// and by criterion benches measuring hot-path latency. A stream route opened
/// on this thread while the guard lives writes through this pathway.
pub fn init_for_tests(config: StreamlibLoggingConfig) -> Result<StreamlibLoggingGuard> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut guard = init_for_tests_intercepting_nothing_yet(&config)?;
    #[cfg(unix)]
    if config.intercept_stdio {
        guard.intercept_the_standard_streams_carrying_this_threads_route()?;
    }
    Ok(guard)
}

/// [`init_for_tests`] with the interception `config` asks for left to the
/// caller; the pretty mirror already writes past it.
pub(crate) fn init_for_tests_intercepting_nothing_yet(
    config: &StreamlibLoggingConfig,
) -> Result<StreamlibLoggingGuard> {
    let tunables = ResolvedTunables::from_config(&config.tunables);
    let (worker, dispatch) = build_components(config, tunables, config.intercept_stdio);
    let default_scope = tracing::dispatcher::set_default(&dispatch);
    Ok(StreamlibLoggingGuard {
        worker: Some(worker),
        default_scope: Some(default_scope),
        dispatch,
        #[cfg(unix)]
        interceptor: None,
    })
}

/// The filter every engine process runs at when `RUST_LOG` says nothing.
///
/// `rmcp` records each MCP request's service start and finish at `info`; held
/// to `warn` here, a host polling a node's control plane stays out of the app's
/// own log. `RUST_LOG=info,rmcp=info` brings them back.
pub const ENGINE_DEFAULT_TRACING_FILTER_DIRECTIVES: &str = "info,rmcp=warn";

/// The level and target filtering every engine process runs at: `RUST_LOG`,
/// or [`ENGINE_DEFAULT_TRACING_FILTER_DIRECTIVES`] where it says nothing.
///
/// One spelling for the app process and for a helper, so a record captured in
/// a child is the record the same call site would make in the parent.
pub(crate) fn the_engines_configured_tracing_filter() -> EnvFilter {
    EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(ENGINE_DEFAULT_TRACING_FILTER_DIRECTIVES))
}

/// The drain worker and the dispatcher feeding it. With `mirror_past_an_interceptor`,
/// the pretty mirror writes to copies of the real fds 1/2 taken now, so it
/// writes past any interception installed later instead of feeding it.
fn build_components(
    config: &StreamlibLoggingConfig,
    tunables: ResolvedTunables,
    mirror_past_an_interceptor: bool,
) -> (WorkerHandle, Dispatch) {
    #[cfg(not(unix))]
    let _ = mirror_past_an_interceptor;
    let pretty_log_mirror_sink: Option<Box<dyn std::io::Write + Send>> = config
        .effective_pretty_log_mirror_stream()
        .map(|pretty_log_mirror_stream| {
            #[cfg(unix)]
            if mirror_past_an_interceptor {
                match stdio_interceptor::duplicate_the_real_standard_streams_for_the_pretty_mirror()
                {
                    Ok(files) => {
                        return Box::new(
                            pretty_log_mirror_stream.pick(files.real_stdout, files.real_stderr),
                        ) as Box<dyn std::io::Write + Send>;
                    }
                    Err(e) => {
                        // Pre-init error path: the subscriber does not exist yet.
                        #[allow(clippy::disallowed_macros)]
                        {
                            eprintln!(
                                "streamlib::logging: failed to copy the real standard streams \
                                 for the pretty mirror: {e} — mirroring through fds 1/2"
                            );
                        }
                    }
                }
            }
            pretty_log_mirror_stream.pick(
                Box::new(std::io::stdout()) as Box<dyn std::io::Write + Send>,
                Box::new(std::io::stderr()),
            )
        });

    let worker = spawn_worker(WorkerConfig {
        source: Source::Rust,
        tunables,
        pretty_log_mirror_sink,
    });

    let layer = JsonlSinkLayer::feeding_a_drain_worker_that_writes_stream_log_files(
        worker.record_queue.clone(),
        tunables,
    );
    let subscriber = Registry::default()
        .with(the_engines_configured_tracing_filter())
        .with(layer);
    (worker, Dispatch::new(subscriber))
}

/// Install a panic hook that requests a best-effort flush from the drain
/// worker before the default panic behavior runs. Composes with any
/// previously installed hook.
fn install_panic_hook(record_queue: &DrainWorkerRecordQueue) {
    let doorbell = record_queue.doorbell.clone();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = doorbell.try_send(WorkerSignal::Flush);
        std::thread::sleep(std::time::Duration::from_millis(50));
        previous(info);
    }));
}
