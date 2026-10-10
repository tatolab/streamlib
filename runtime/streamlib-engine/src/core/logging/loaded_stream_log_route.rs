// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where the records one loaded stream emits go: the stream's name and its
//! runtime's id, stamped on each record, and the stream's own JSONL file.
//!
//! A thread carries a route from the moment it is entered until the entry
//! drops; the logging layer reads the route of the thread an event is emitted
//! on. A record emitted where no route is entered reaches the pretty mirror,
//! and the runtime's own log when its runtime keeps one.

use std::cell::RefCell;
use std::future::Future;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Condvar, Mutex};
use streamlib_runtime_client_contract::runtime_log_file_paths::{
    active_runtime_log_segment_file_name, loaded_stream_log_path,
};

use crate::core::logging::layer::JsonlSinkLayer;
use crate::core::logging::loaded_stream_log_record_history::{
    LOADED_STREAM_LOG_RECORDS_HELD_IN_MEMORY, LoadedStreamLogRecordHistory,
    LoadedStreamLogRecordsPage,
};
use crate::core::logging::worker::DrainWorkerRecordQueue;
use crate::core::logging::writer::JsonlBatchedWriter;

/// How long closing a stream's JSONL file waits for the drain worker to write
/// the stream's records queued before the close.
const QUEUED_RECORDS_WRITTEN_BEFORE_A_STREAM_LOG_CLOSES_BUDGET: Duration = Duration::from_secs(2);

/// How long closing a stream's JSONL file waits for the stream's readers of a
/// helper's pipes to reach the end of what the helper wrote.
const HELPER_PIPE_READERS_FINISHED_BEFORE_A_STREAM_LOG_CLOSES_BUDGET: Duration =
    Duration::from_secs(2);

/// The instance name the runtime's own log files are named after:
/// `tatolabd-<started_at_millis>.jsonl`.
pub const RUNTIME_OWN_LOG_INSTANCE_NAME: &str = "tatolabd";

/// Whose records a log route carries.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LogRouteOwner {
    /// One loaded stream, by its URL-safe cast name.
    LoadedStream(String),
    /// The runtime itself: the records no stream emitted, which no reader
    /// pages through, so none is held in memory.
    TheRuntimeItself,
}

impl LogRouteOwner {
    /// What a log of this owner holds, as a warning names it.
    fn what_its_log_holds(&self) -> String {
        match self {
            Self::LoadedStream(stream_name) => format!("the stream `{stream_name}`"),
            Self::TheRuntimeItself => "the runtime's own log".to_string(),
        }
    }

    /// How many of this owner's most recent records its route holds in memory.
    fn records_held_in_memory(&self) -> usize {
        match self {
            Self::LoadedStream(_) => LOADED_STREAM_LOG_RECORDS_HELD_IN_MEMORY,
            Self::TheRuntimeItself => 0,
        }
    }
}

thread_local! {
    static LOADED_STREAM_LOG_ROUTE_OF_THIS_THREAD: RefCell<Option<Arc<LoadedStreamLogRoute>>> =
        const { RefCell::new(None) };
}

/// Where the records one loaded stream emits go: its runtime's id and its
/// name, stamped on every record, its JSONL file under its project, and the
/// most recent of them, numbered, in memory. The runtime's own log is a route
/// of no stream.
pub struct LoadedStreamLogRoute {
    runtime_id: String,
    owner: LogRouteOwner,
    jsonl_log_file: Option<LoadedStreamJsonlLogFile>,
    /// This stream's records a full queue dropped since the drain worker last
    /// reported them.
    records_dropped_from_a_full_queue: AtomicU64,
    helper_pipe_readers_still_reading: Mutex<usize>,
    a_helper_pipe_reader_finished: Condvar,
    numbered_record_history: Mutex<LoadedStreamLogRecordHistory>,
}

/// One loaded stream's JSONL file, and the queue whose drain worker writes it.
struct LoadedStreamJsonlLogFile {
    path: PathBuf,
    writer_while_open: Mutex<Option<JsonlBatchedWriter>>,
    record_queue_drained_into_it: DrainWorkerRecordQueue,
}

impl std::fmt::Debug for LoadedStreamLogRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedStreamLogRoute")
            .field("runtime_id", &self.runtime_id)
            .field("owner", &self.owner)
            .field("jsonl_log_path", &self.jsonl_log_path())
            .finish()
    }
}

impl LoadedStreamLogRoute {
    /// The route of the stream `stream_name` of the runtime `runtime_id`, its
    /// JSONL file opened under `project_directory`'s `.streamlib/logs/`.
    ///
    /// Opened through the logging pathway the calling thread's dispatcher
    /// runs; with none of the engine's installed, or when the file cannot be
    /// opened (said once, as a warning), the route stamps records and writes
    /// no file.
    pub fn open_in_project_directory(
        runtime_id: &str,
        stream_name: &str,
        project_directory: &Path,
    ) -> Arc<Self> {
        let owner = LogRouteOwner::LoadedStream(stream_name.to_string());
        Self::open_writing_its_jsonl_file_at(runtime_id, owner, |started_at_millis| {
            loaded_stream_log_path(
                project_directory,
                runtime_id,
                stream_name,
                started_at_millis,
            )
        })
    }

    /// The route of the records of the runtime `runtime_id` that no stream
    /// emitted, its JSONL file opened as
    /// `<runtime_own_log_directory>/tatolabd-<started_at_millis>.jsonl`.
    ///
    /// Opened as [`Self::open_in_project_directory`] opens a stream's.
    pub(crate) fn open_the_runtimes_own_log_in(
        runtime_id: &str,
        runtime_own_log_directory: &Path,
    ) -> Arc<Self> {
        Self::open_writing_its_jsonl_file_at(
            runtime_id,
            LogRouteOwner::TheRuntimeItself,
            |started_at_millis| {
                runtime_own_log_directory.join(active_runtime_log_segment_file_name(
                    RUNTIME_OWN_LOG_INSTANCE_NAME,
                    started_at_millis,
                ))
            },
        )
    }

    fn open_writing_its_jsonl_file_at(
        runtime_id: &str,
        owner: LogRouteOwner,
        log_path_started_at: impl FnOnce(u128) -> PathBuf,
    ) -> Arc<Self> {
        let jsonl_log_file = tracing::dispatcher::get_default(|dispatch| {
            dispatch
                .downcast_ref::<JsonlSinkLayer>()
                .and_then(JsonlSinkLayer::record_queue_that_writes_stream_log_files)
        })
        .and_then(|(record_queue_drained_into_it, tunables)| {
            let started_at_millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since_the_epoch| since_the_epoch.as_millis())
                .unwrap_or(0);
            let path = log_path_started_at(started_at_millis);
            match JsonlBatchedWriter::open(
                &path,
                tunables.batch_bytes,
                tunables.fsync_on_every_batch,
                tunables.segment_rotation,
            ) {
                Ok(writer) => Some(LoadedStreamJsonlLogFile {
                    path,
                    writer_while_open: Mutex::new(Some(writer)),
                    record_queue_drained_into_it,
                }),
                Err(open_failure) => {
                    tracing::warn!(
                        "{} writes no JSONL log: {} could not be opened: {open_failure}",
                        owner.what_its_log_holds(),
                        path.display()
                    );
                    None
                }
            }
        });
        Arc::new(Self {
            runtime_id: runtime_id.to_string(),
            numbered_record_history: Mutex::new(LoadedStreamLogRecordHistory::holding_at_most(
                owner.records_held_in_memory(),
            )),
            owner,
            jsonl_log_file,
            records_dropped_from_a_full_queue: AtomicU64::new(0),
            helper_pipe_readers_still_reading: Mutex::new(0),
            a_helper_pipe_reader_finished: Condvar::new(),
        })
    }

    /// The id of the runtime the stream is loaded in.
    pub fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    /// The stream's URL-safe cast name; `None` for the runtime's own log.
    pub fn stream_name(&self) -> Option<&str> {
        match &self.owner {
            LogRouteOwner::LoadedStream(stream_name) => Some(stream_name),
            LogRouteOwner::TheRuntimeItself => None,
        }
    }

    /// What this route's log holds, as a warning names it.
    fn what_this_log_holds(&self) -> String {
        self.owner.what_its_log_holds()
    }

    /// The stream's active JSONL segment, `None` when it writes none.
    pub fn jsonl_log_path(&self) -> Option<&Path> {
        self.jsonl_log_file
            .as_ref()
            .map(|jsonl_log_file| jsonl_log_file.path.as_path())
    }

    /// Route every record this thread emits to this stream until the returned
    /// entry drops, when the route the thread carried before is back.
    pub fn enter_on_this_thread(self: &Arc<Self>) -> LoadedStreamLogRouteEnteredOnThisThread {
        let route_carried_before = LOADED_STREAM_LOG_ROUTE_OF_THIS_THREAD
            .try_with(|route| route.borrow_mut().replace(Arc::clone(self)))
            .ok()
            .flatten();
        LoadedStreamLogRouteEnteredOnThisThread {
            route_carried_before,
            entered_on_this_thread_only: PhantomData,
        }
    }

    /// Run `work` with this route entered on the calling thread.
    pub fn run_entered<R>(self: &Arc<Self>, work: impl FnOnce() -> R) -> R {
        let _entered = self.enter_on_this_thread();
        work()
    }

    /// Let every reader of this stream's helper pipes reach the end of what
    /// the helper wrote, write every record of this stream queued before then,
    /// then flush, `fdatasync` and close its JSONL file; a record this stream
    /// emits later reaches the pretty mirror only. Idempotent.
    pub fn close_the_jsonl_log_file(&self) {
        let Some(jsonl_log_file) = &self.jsonl_log_file else {
            return;
        };
        if jsonl_log_file.writer_while_open.lock().is_none() {
            return;
        }
        let mut helper_pipe_readers_still_reading = self.helper_pipe_readers_still_reading.lock();
        let readers_outlasted_the_budget = self
            .a_helper_pipe_reader_finished
            .wait_while_for(
                &mut helper_pipe_readers_still_reading,
                |still_reading| *still_reading > 0,
                HELPER_PIPE_READERS_FINISHED_BEFORE_A_STREAM_LOG_CLOSES_BUDGET,
            )
            .timed_out();
        let helper_pipe_readers_left_reading = *helper_pipe_readers_still_reading;
        drop(helper_pipe_readers_still_reading);
        if readers_outlasted_the_budget {
            tracing::warn!(
                "{} closed its JSONL log while {helper_pipe_readers_left_reading} reader(s) of \
                 its helpers' pipes had not reached their end; what they read later reaches the \
                 pretty mirror only",
                self.what_this_log_holds()
            );
        }
        if !jsonl_log_file
            .record_queue_drained_into_it
            .wait_until_every_queued_record_is_written(
                QUEUED_RECORDS_WRITTEN_BEFORE_A_STREAM_LOG_CLOSES_BUDGET,
            )
        {
            tracing::warn!(
                "{} closed its JSONL log before the drain worker wrote every record queued for \
                 it; the rest reach the pretty mirror only",
                self.what_this_log_holds()
            );
        }
        let writer = jsonl_log_file.writer_while_open.lock().take();
        if let Some(mut writer) = writer
            && let Err(sync_failure) = writer.flush_and_fsync()
        {
            tracing::warn!(
                "the JSONL log of {} at {} did not reach the disk whole: {sync_failure}",
                self.what_this_log_holds(),
                jsonl_log_file.path.display()
            );
        }
    }

    /// Count one of this stream's records dropped from a full queue; `true`
    /// for the first since the count was last taken.
    pub(crate) fn note_a_record_dropped_from_a_full_queue(&self) -> bool {
        self.records_dropped_from_a_full_queue
            .fetch_add(1, Ordering::Relaxed)
            == 0
    }

    /// The records of this stream dropped from a full queue since the last
    /// take, reset to none.
    pub(crate) fn take_the_count_of_records_dropped(&self) -> u64 {
        self.records_dropped_from_a_full_queue
            .swap(0, Ordering::Relaxed)
    }

    /// The records this route numbered after `after`, at most `max_count` of
    /// them, from the most recent it holds in memory; the runtime's own log
    /// holds none.
    pub fn log_records_after(&self, after: u64, max_count: usize) -> LoadedStreamLogRecordsPage {
        self.numbered_record_history
            .lock()
            .records_after(after, max_count)
    }

    /// Number one serialized record into the in-memory history and append it
    /// to the open file; `false` when the route writes no file or it has
    /// closed.
    pub(crate) fn append_serialized_record(&self, serialized_record: &[u8]) -> bool {
        self.numbered_record_history
            .lock()
            .append(serialized_record);
        let Some(jsonl_log_file) = &self.jsonl_log_file else {
            return false;
        };
        match jsonl_log_file.writer_while_open.lock().as_mut() {
            Some(writer) => {
                let _ = writer.append_record(serialized_record);
                true
            }
            None => false,
        }
    }

    /// Hand the records the open file buffers to the OS.
    pub(crate) fn flush_the_records_pending_in_the_jsonl_log_file(&self) {
        if let Some(jsonl_log_file) = &self.jsonl_log_file
            && let Some(writer) = jsonl_log_file.writer_while_open.lock().as_mut()
        {
            let _ = writer.flush_if_pending();
        }
    }

    /// Hand the records the open file buffers to the OS and `fdatasync` it.
    pub(crate) fn flush_and_fsync_the_jsonl_log_file(&self) {
        if let Some(jsonl_log_file) = &self.jsonl_log_file
            && let Some(writer) = jsonl_log_file.writer_while_open.lock().as_mut()
        {
            let _ = writer.flush_and_fsync();
        }
    }
}

/// The runtime's own log, written every record no stream emits while the
/// engine that opened it lives.
pub(crate) struct TheRuntimesOwnLogWhileItsEngineLives {
    route: Arc<LoadedStreamLogRoute>,
    runtime_own_log_route_of_the_queue: Arc<Mutex<Option<Arc<LoadedStreamLogRoute>>>>,
}

impl TheRuntimesOwnLogWhileItsEngineLives {
    /// Open the runtime's own log under `runtime_own_log_directory` and have
    /// the drain worker of the calling thread's pathway write the records no
    /// stream emits to it; `None` when no pathway of the engine's runs or the
    /// file cannot be opened, which the open says as a warning.
    pub(crate) fn open(runtime_id: &str, runtime_own_log_directory: &Path) -> Option<Self> {
        let route = LoadedStreamLogRoute::open_the_runtimes_own_log_in(
            runtime_id,
            runtime_own_log_directory,
        );
        let runtime_own_log_route_of_the_queue = route
            .jsonl_log_file
            .as_ref()?
            .record_queue_drained_into_it
            .runtime_own_log_route
            .clone();
        *runtime_own_log_route_of_the_queue.lock() = Some(Arc::clone(&route));
        Some(Self {
            route,
            runtime_own_log_route_of_the_queue,
        })
    }

    /// The log's active JSONL segment.
    pub(crate) fn jsonl_log_path(&self) -> Option<&Path> {
        self.route.jsonl_log_path()
    }
}

impl Drop for TheRuntimesOwnLogWhileItsEngineLives {
    /// Closed while the drain worker still writes to it, so every record
    /// queued before the close lands; later ones reach the pretty mirror only.
    fn drop(&mut self) {
        self.route.close_the_jsonl_log_file();
        let mut runtime_own_log_route = self.runtime_own_log_route_of_the_queue.lock();
        if runtime_own_log_route
            .as_ref()
            .is_some_and(|installed| Arc::ptr_eq(installed, &self.route))
        {
            *runtime_own_log_route = None;
        }
    }
}

impl Drop for LoadedStreamLogRoute {
    /// Every queued record holds its route, so none is left to write once
    /// the last route drops.
    fn drop(&mut self) {
        if let Some(jsonl_log_file) = &self.jsonl_log_file
            && let Some(mut writer) = jsonl_log_file.writer_while_open.lock().take()
        {
            let _ = writer.flush_and_fsync();
        }
    }
}

/// A [`LoadedStreamLogRoute`] entered on one thread; dropping it restores the
/// route the thread carried before.
pub struct LoadedStreamLogRouteEnteredOnThisThread {
    route_carried_before: Option<Arc<LoadedStreamLogRoute>>,
    entered_on_this_thread_only: PhantomData<*const ()>,
}

impl Drop for LoadedStreamLogRouteEnteredOnThisThread {
    fn drop(&mut self) {
        let route_carried_before = self.route_carried_before.take();
        let _ = LOADED_STREAM_LOG_ROUTE_OF_THIS_THREAD
            .try_with(|route| *route.borrow_mut() = route_carried_before);
    }
}

/// The route the calling thread carries, if any; none once the thread's
/// locals are being torn down, so a record a destructor emits never panics.
pub fn the_loaded_stream_log_route_of_this_thread() -> Option<Arc<LoadedStreamLogRoute>> {
    LOADED_STREAM_LOG_ROUTE_OF_THIS_THREAD
        .try_with(|route| route.borrow().clone())
        .ok()
        .flatten()
}

/// `work`, made to run in the route the calling thread carries now — for a
/// thread or a blocking task this thread spawns on a stream's behalf.
pub fn carrying_this_threads_loaded_stream_log_route<R>(
    work: impl FnOnce() -> R,
) -> impl FnOnce() -> R {
    let route_of_the_spawning_thread = the_loaded_stream_log_route_of_this_thread();
    move || {
        run_in_the_loaded_stream_log_route_when_there_is_one(
            route_of_the_spawning_thread.as_ref(),
            work,
        )
    }
}

/// `work`, made to run in the route the calling thread carries now, that
/// route's JSONL file kept open until `work` returns or its close's budget runs
/// out — for a thread reading a helper's pipe to its end.
pub(crate) fn carrying_this_threads_loaded_stream_log_route_and_reading_a_helper_pipe<R>(
    work: impl FnOnce() -> R,
) -> impl FnOnce() -> R {
    let route_of_the_spawning_thread = the_loaded_stream_log_route_of_this_thread();
    let reading_into_the_route = route_of_the_spawning_thread
        .as_ref()
        .map(LoadedStreamHelperPipeReaderStillReading::counted_against);
    move || {
        let _reading_into_the_route = reading_into_the_route;
        run_in_the_loaded_stream_log_route_when_there_is_one(
            route_of_the_spawning_thread.as_ref(),
            work,
        )
    }
}

/// One reader of a stream's helper pipe the stream's log close waits for
/// until it drops.
struct LoadedStreamHelperPipeReaderStillReading {
    route: Arc<LoadedStreamLogRoute>,
}

impl LoadedStreamHelperPipeReaderStillReading {
    fn counted_against(route: &Arc<LoadedStreamLogRoute>) -> Self {
        *route.helper_pipe_readers_still_reading.lock() += 1;
        Self {
            route: Arc::clone(route),
        }
    }
}

impl Drop for LoadedStreamHelperPipeReaderStillReading {
    fn drop(&mut self) {
        let mut helper_pipe_readers_still_reading =
            self.route.helper_pipe_readers_still_reading.lock();
        *helper_pipe_readers_still_reading -= 1;
        if *helper_pipe_readers_still_reading == 0 {
            self.route.a_helper_pipe_reader_finished.notify_all();
        }
    }
}

/// Run `work` with `route` entered when there is one — for a callback an OS
/// queue runs on a thread the stream did not spawn.
pub fn run_in_the_loaded_stream_log_route_when_there_is_one<R>(
    route: Option<&Arc<LoadedStreamLogRoute>>,
    work: impl FnOnce() -> R,
) -> R {
    match route {
        Some(route) => route.run_entered(work),
        None => work(),
    }
}

/// `future`, polled in `route` on whichever thread polls it.
pub(crate) fn polled_in_a_loaded_stream_log_route<F: Future>(
    route: Arc<LoadedStreamLogRoute>,
    future: F,
) -> FuturePolledInALoadedStreamLogRoute<F> {
    FuturePolledInALoadedStreamLogRoute { route, future }
}

pin_project_lite::pin_project! {
    /// A future every poll of which runs with its stream's route entered.
    pub(crate) struct FuturePolledInALoadedStreamLogRoute<F> {
        route: Arc<LoadedStreamLogRoute>,
        #[pin]
        future: F,
    }
}

impl<F: Future> Future for FuturePolledInALoadedStreamLogRoute<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _entered = this.route.enter_on_this_thread();
        this.future.poll(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtimes_own_log_holds_no_record_in_memory_and_a_streams_does() {
        let runtime_own_log_directory = tempfile::tempdir().unwrap();
        let project_directory = tempfile::tempdir().unwrap();
        let the_runtimes_own_route = LoadedStreamLogRoute::open_the_runtimes_own_log_in(
            "R-test",
            runtime_own_log_directory.path(),
        );
        let a_streams_route = LoadedStreamLogRoute::open_in_project_directory(
            "R-test",
            "camera",
            project_directory.path(),
        );

        for route in [&the_runtimes_own_route, &a_streams_route] {
            route.append_serialized_record(br#"{"message":"first"}"#);
            route.append_serialized_record(br#"{"message":"second"}"#);
        }

        let the_runtimes_own_page = the_runtimes_own_route.log_records_after(0, 256);
        assert!(
            the_runtimes_own_page.records.is_empty(),
            "{the_runtimes_own_page:?}"
        );
        assert_eq!(the_runtimes_own_route.stream_name(), None);
        let a_streams_page = a_streams_route.log_records_after(0, 256);
        assert_eq!(
            a_streams_page
                .records
                .iter()
                .map(|numbered| numbered.sequence)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(a_streams_route.stream_name(), Some("camera"));
    }
}
