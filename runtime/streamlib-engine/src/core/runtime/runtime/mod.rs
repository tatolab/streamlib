// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod engine_resources_shared_by_every_stream;
mod runner_construction;
mod runner_identity_and_configuration;
#[cfg(target_os = "linux")]
mod runner_linux_surface_share;
#[cfg(target_os = "macos")]
mod runner_macos_surface_share;
mod runner_stream_loading;
mod runner_stream_shutdown;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::streams_loaded_in_this_runtime::loaded_stream::LoadedStreamInThisRuntime;
use super::streams_loaded_in_this_runtime::stream_actions_of_this_runtime::StreamActionsOfTheEngine;
use super::{RuntimeShutdownEscalation, StreamEnvironment, StreamLocation};
use crate::core::context::{CaptureDeviceAccessPrompting, GpuContext};
use crate::core::graph::cast_exposed_name_to_url_safe;
use crate::core::signals::ScopedShutdownSignalOwnership;
use crate::core::{Error, Result};
pub(crate) use engine_resources_shared_by_every_stream::EngineResourcesSharedByEveryStream;

/// The tokio runtime the engine runs on, as every stream reaches it.
///
/// Enables Runner to work both standalone (owning its runtime) and
/// integrated into existing tokio applications (using the current handle).
pub(crate) enum TokioRuntimeVariant {
    /// The runtime the [`Runner`] owns (created when NOT in tokio context),
    /// reached by its handle.
    TokioRuntimeTheRunnerOwns(tokio::runtime::Handle),
    /// Runner uses an external tokio Handle (auto-detected when called from tokio context).
    ExternalTokioHandle(tokio::runtime::Handle),
}

impl TokioRuntimeVariant {
    /// Get a tokio Handle from either variant.
    pub(crate) fn handle(&self) -> tokio::runtime::Handle {
        match self {
            TokioRuntimeVariant::TokioRuntimeTheRunnerOwns(handle)
            | TokioRuntimeVariant::ExternalTokioHandle(handle) => handle.clone(),
        }
    }
}

/// How long dropping an owned tokio runtime waits for its tasks to stop.
///
/// A plain drop waits for every `spawn_blocking` task to return, without
/// limit, so one blocking call that never returns hangs the engine's teardown.
const OWNED_TOKIO_RUNTIME_SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

/// A tokio runtime the engine owns, shut down within
/// [`OWNED_TOKIO_RUNTIME_SHUTDOWN_BUDGET`] when dropped.
pub(crate) struct TokioRuntimeShutDownWithinItsBudget(
    std::mem::ManuallyDrop<tokio::runtime::Runtime>,
);

impl TokioRuntimeShutDownWithinItsBudget {
    pub(crate) fn owning(runtime: tokio::runtime::Runtime) -> Self {
        Self(std::mem::ManuallyDrop::new(runtime))
    }

    pub(crate) fn handle(&self) -> &tokio::runtime::Handle {
        self.0.handle()
    }
}

impl Drop for TokioRuntimeShutDownWithinItsBudget {
    fn drop(&mut self) {
        crate::core::runtime::note_what_the_engine_teardown_is_waiting_on(
            "the engine's tokio runtime",
        );
        // SAFETY: taken once, here, as this value is dropped; nothing reads the
        // runtime afterwards.
        let runtime = unsafe { std::mem::ManuallyDrop::take(&mut self.0) };
        runtime.shutdown_timeout(OWNED_TOKIO_RUNTIME_SHUTDOWN_BUDGET);
    }
}

/// What commits the files of a project [`Runner::create_project`] wrote, in the directory it
/// is handed, as the first commit of the project's history; the error says why it did not.
pub type CreatedProjectFirstCommit =
    Arc<dyn Fn(&Path) -> std::result::Result<(), String> + Send + Sync>;

/// A hook run once with the engine's GPU context.
type SetupHookOfTheEngine = Box<dyn FnOnce(&GpuContext) -> Result<()> + Send>;

/// The engine's setup hooks: queued until the GPU context is created, then
/// run once.
enum SetupHooksOfTheEngine {
    WaitingForTheGpuContext(Vec<SetupHookOfTheEngine>),
    RanAsTheGpuContextWasCreated,
}

/// The engine: a table of loaded streams under one GPU context, iceoryx2
/// node, tokio runtime and surface service.
///
/// Every graph operation is a stream's: load a stream, then use the
/// [`LoadedStreamInThisRuntime`] it hands back.
pub struct Runner {
    // Declared first so it drops before the engine resources: a task spawned on this runtime may
    // hold them, and their last drop must never land on one of this runtime's workers.
    _tokio_runtime_the_runner_owns: Option<TokioRuntimeShutDownWithinItsBudget>,
    pub(super) engine_resources_shared_by_every_stream: Arc<EngineResourcesSharedByEveryStream>,
    /// Where this runtime keeps its streams, and the lock its stream actions
    /// take one at a time.
    pub(super) stream_actions: StreamActionsOfTheEngine,
}

/// What a host chooses as it constructs a [`Runner`].
#[derive(Debug, Clone, Default)]
pub struct RunnerConstructionOptions {
    /// The standard stream the pretty log mirror writes to, when this runtime
    /// is the first in its process to install the process's logging pathway.
    pub pretty_log_mirror_stream: crate::core::logging::PrettyLogMirrorStandardStream,
    /// Where the records no stream emits are also written as JSONL, to
    /// `tatolabd-<started_at_millis>.jsonl`, rotated as a stream's log is.
    pub runtime_own_log_directory: Option<PathBuf>,
}

/// Where a loaded stream's id comes from.
/// What a host chooses as it loads one stream of a project into a [`Runner`].
#[derive(Debug, Clone)]
pub struct OptionsForLoadingOneStream {
    /// The id the runtime records the stream under, cast.
    pub stream_id: String,
    /// The project and stream asset the stream is.
    pub stream_location: StreamLocation,
    /// The label the stream is shown under; the stream id when none is given.
    pub display_name: Option<String>,
    pub stream_environment: Option<StreamEnvironment>,
    /// Whether the stream's capture devices may ask the user for access nobody
    /// has answered yet.
    pub capture_device_access_prompting: CaptureDeviceAccessPrompting,
    /// Why none of the stream's node code is read: every node is held as a placeholder of its
    /// type, drawn from the graph as written; `None` reads its node code as a load does.
    pub why_none_of_its_node_code_is_read: Option<String>,
}

impl OptionsForLoadingOneStream {
    pub fn at_stream_location(
        stream_id: impl Into<String>,
        stream_location: StreamLocation,
    ) -> Self {
        Self {
            stream_id: stream_id.into(),
            stream_location,
            display_name: None,
            stream_environment: None,
            capture_device_access_prompting: CaptureDeviceAccessPrompting::default(),
            why_none_of_its_node_code_is_read: None,
        }
    }

    /// These options, reading none of the stream's node code for `why_none_of_it_is_read`.
    pub fn reading_none_of_its_node_code(self, why_none_of_it_is_read: String) -> Self {
        Self {
            why_none_of_its_node_code_is_read: Some(why_none_of_it_is_read),
            ..self
        }
    }

    pub fn in_stream_environment(self, stream_environment: StreamEnvironment) -> Self {
        Self {
            stream_environment: Some(stream_environment),
            ..self
        }
    }

    /// These options, letting the stream's capture devices ask the user as
    /// `capture_device_access_prompting` says.
    pub fn with_capture_device_access_prompting(
        self,
        capture_device_access_prompting: CaptureDeviceAccessPrompting,
    ) -> Self {
        Self {
            capture_device_access_prompting,
            ..self
        }
    }

    /// These options, showing the stream as `display_name`.
    pub fn with_display_name(self, display_name: impl Into<String>) -> Self {
        Self {
            display_name: Some(display_name.into()),
            ..self
        }
    }

    /// The stream's project directory, its location's, refused when its stream environment
    /// lives in another.
    fn resolved_project_directory(&self, stream_id: &str) -> Result<PathBuf> {
        let project_directory = &self.stream_location.project_directory;
        match &self.stream_environment {
            Some(stream_environment)
                if stream_environment.project_directory != *project_directory =>
            {
                Err(Error::Configuration(format!(
                    "the stream `{stream_id}` is of the project in `{}` and was given a \
                     stream environment in `{}`; a stream's environment lives in its project \
                     directory",
                    project_directory.display(),
                    stream_environment.project_directory.display()
                )))
            }
            _ => Ok(project_directory.clone()),
        }
    }
}

/// How a load watched for an interrupt — a machine shutdown request, or the
/// cancel of the stream action loading it — ended.
pub enum StreamLoadObservingInterrupts {
    /// The stream loaded and nothing interrupted it.
    Loaded(Arc<LoadedStreamInThisRuntime>),
    /// An interrupt came before the load or while it ran; the stream was never
    /// loaded.
    AbandonedForAnInterrupt,
}

/// What [`Runner::wait_until_every_stream_has_ended`] saw once every stream had
/// ended.
pub struct EveryStreamEndedDuringTheWait {
    /// Every stream loaded, or out of the table with its end not yet recorded,
    /// at any point of the wait, in the order it was first seen.
    pub streams_seen_during_the_wait: Vec<Arc<LoadedStreamInThisRuntime>>,
    /// How the first of them to end with a failure ended, or `Ok` when none did.
    pub how_the_first_failed_stream_ended: Result<()>,
}

impl Drop for Runner {
    fn drop(&mut self) {
        if let Err(shut_down_failure) = self.shut_down() {
            tracing::error!("the engine shut down with a failure: {shut_down_failure}");
        }
    }
}

/// The URL-safe cast of the stream id a load names, refused when it casts to
/// nothing and when there is none.
pub(super) fn the_cast_stream_id_a_load_names(requested_stream_id: &str) -> Result<String> {
    cast_exposed_name_to_url_safe(requested_stream_id)
        .map(|cast| cast.into_owned())
        .map_err(|casts_to_nothing| {
            Error::GraphError(format!(
                "cannot load the stream `{requested_stream_id}`: {casts_to_nothing}"
            ))
        })
}

/// How a load refused for an id already loaded is told to go on.
const LOAD_UNDER_ANOTHER_STREAM_ID_IN_CODE: &str =
    "load this one under another id with `OptionsForLoadingOneStream::at_stream_location`";

/// The refusal of a load under the id `first` is loaded as, naming where it
/// was loaded from and ending with `how_to_load_this_one`.
pub(super) fn a_stream_id_already_loaded_refusal(
    first: &LoadedStreamInThisRuntime,
    how_to_load_this_one: &str,
) -> Error {
    Error::GraphError(format!(
        "a stream with the id `{}` is already loaded in this runtime, from `{}`; \
         {how_to_load_this_one}",
        first.stream_id(),
        first.stream_location()
    ))
}

/// What interrupted a load, as its log lines name it: a machine shutdown
/// request, else the cancel of the stream action loading it.
pub(crate) fn what_interrupted_a_load() -> &'static str {
    if crate::core::runtime::is_the_machines_shutdown_requested() {
        "a machine shutdown was requested"
    } else {
        "its stream action was cancelled"
    }
}

/// Walk a stream a load is still building — in no table the machine's walk
/// reaches — to the machine's shutdown level, and interrupt its describes.
fn walk_a_stream_being_loaded_to_the_machines_shutdown_level(stream: &LoadedStreamInThisRuntime) {
    let machines_level = crate::core::runtime::the_machines_shutdown_escalation();
    stream
        .this_streams_shutdown_escalation()
        .raise_to_graceful();
    if machines_level >= RuntimeShutdownEscalation::Forced {
        stream.this_streams_shutdown_escalation().raise_to_forced();
    }
    stream.interrupt_every_processor_interpreter_describe();
}

/// Ask `stream` for its shutdown and block until it has ended. Once its
/// shutdown thread has started, the stream's watchdog bounds the wait; until
/// then each poll asks again, retrying the thread's spawn. A machine shutdown
/// forced meanwhile forces this stream too, whichever thread is waiting on the
/// machine.
pub(super) fn request_a_streams_shutdown_and_wait_until_it_has_ended(
    stream: &LoadedStreamInThisRuntime,
    reason: &str,
) {
    loop {
        stream.ask_for_this_streams_shutdown(reason);
        if crate::core::runtime::the_machines_shutdown_escalation()
            >= RuntimeShutdownEscalation::Forced
        {
            stream.force_this_streams_shutdown("the machine's shutdown was forced");
        }
        if stream.wait_for_this_streams_end_within(
            crate::core::runtime::RUNTIME_SHUTDOWN_REQUEST_OBSERVATION_POLL_INTERVAL,
        ) {
            return;
        }
    }
}

/// Marks an engine the owner of the machine's shutdown signals until dropped.
struct TheMachineShutdownSignalsOwnedByOneEngine<'engine>(
    &'engine EngineResourcesSharedByEveryStream,
);

impl<'engine> TheMachineShutdownSignalsOwnedByOneEngine<'engine> {
    fn mark(engine: &'engine EngineResourcesSharedByEveryStream) -> Self {
        engine
            .owns_the_machine_shutdown_signals
            .store(true, Ordering::SeqCst);
        Self(engine)
    }
}

impl Drop for TheMachineShutdownSignalsOwnedByOneEngine<'_> {
    fn drop(&mut self) {
        self.0
            .owns_the_machine_shutdown_signals
            .store(false, Ordering::SeqCst);
    }
}

/// Stop a stream a refused load built, which never entered the table, and
/// close its JSONL log.
fn tear_down_a_stream_that_never_entered_the_table(stream: &Arc<LoadedStreamInThisRuntime>) {
    if let Err(teardown_failure) = stream.stop() {
        stream.log_route().run_entered(|| {
            tracing::error!(
                "the stream `{}`, refused before it was loaded, failed to tear down: \
                 {teardown_failure}",
                stream.stream_id()
            )
        });
    }
    stream.log_route().write_every_record_it_emitted_and_sync();
}

/// Own SIGINT, SIGTERM and SIGHUP until the returned value drops.
fn take_shutdown_signal_ownership() -> Result<ScopedShutdownSignalOwnership> {
    ScopedShutdownSignalOwnership::take_until_dropped().map_err(|ownership_failure| {
        Error::Configuration(format!(
            "Failed to own shutdown signals: {}",
            ownership_failure
        ))
    })
}

#[cfg(test)]
mod tests;
