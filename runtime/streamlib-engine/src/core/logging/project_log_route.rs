// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where the records a project's streams and stream actions emit go: the
//! project's one JSONL log, `<project>/.streamlib/logs/project.jsonl`, each
//! record stamped with its runtime's id, the stream that emitted it and the
//! stream action it belongs to.
//!
//! A thread carries an entry of a route from the moment it is entered until
//! the entry drops; the logging layer reads the entry of the thread an event
//! is emitted on. A record emitted where no entry is entered reaches the
//! pretty mirror, and the runtime's own log when its runtime keeps one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::{Condvar, Mutex};
use streamlib_runtime_client_contract::runtime_log_file_paths::{
    active_runtime_log_segment_file_name, project_log_path,
};

use crate::core::logging::layer::JsonlSinkLayer;
use crate::core::logging::worker::DrainWorkerRecordQueue;
use crate::core::logging::writer::JsonlBatchedWriter;
use crate::core::runtime::{StreamThisThreadWorksFor, mark_this_thread_as_working_for};

/// How long a flush of a project's log for one emitter waits for the drain
/// worker to write the records queued before it.
const QUEUED_RECORDS_WRITTEN_BEFORE_A_PROJECT_LOG_FLUSH_BUDGET: Duration = Duration::from_secs(2);

/// How long a flush of a project's log for one emitter waits for that
/// emitter's readers of a helper's pipes to reach the end of what the helper
/// wrote.
const HELPER_PIPE_READERS_FINISHED_BEFORE_A_PROJECT_LOG_FLUSH_BUDGET: Duration =
    Duration::from_secs(2);

/// The instance name the runtime's own log files are named after:
/// `tatolabd-<started_at_millis>.jsonl`.
pub const RUNTIME_OWN_LOG_INSTANCE_NAME: &str = "tatolabd";

/// Whose records a log route carries.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectLogRouteOwner {
    /// One project, by its directory: every stream loaded from it and every
    /// stream action of it.
    Project(PathBuf),
    /// The runtime itself: the records no project's stream or action emitted.
    TheRuntimeItself,
}

impl ProjectLogRouteOwner {
    /// What a log of this owner holds, as a warning names it.
    fn what_its_log_holds(&self) -> String {
        match self {
            Self::Project(project_directory) => {
                format!("the project in {}", project_directory.display())
            }
            Self::TheRuntimeItself => "the runtime's own log".to_string(),
        }
    }
}

thread_local! {
    static PROJECT_LOG_ROUTE_ENTRY_OF_THIS_THREAD: RefCell<Option<Arc<ProjectLogRouteEntry>>> =
        const { RefCell::new(None) };
}

/// The route of each project a route is open for in this process, so every
/// stream and action of one project shares one file.
fn project_log_routes_open_in_this_process()
-> &'static Mutex<HashMap<PathBuf, Weak<ProjectLogRoute>>> {
    static PROJECT_LOG_ROUTES: OnceLock<Mutex<HashMap<PathBuf, Weak<ProjectLogRoute>>>> =
        OnceLock::new();
    PROJECT_LOG_ROUTES.get_or_init(Mutex::default)
}

/// One project's JSONL log, shared by every stream and stream action of it;
/// the runtime's own log is a route of no project.
pub struct ProjectLogRoute {
    owner: ProjectLogRouteOwner,
    jsonl_log_file: Option<ProjectJsonlLogFile>,
}

/// A route's JSONL file, and the queue whose drain worker writes it.
struct ProjectJsonlLogFile {
    path: PathBuf,
    writer_while_open: Mutex<Option<JsonlBatchedWriter>>,
    record_queue_drained_into_it: DrainWorkerRecordQueue,
}

impl std::fmt::Debug for ProjectLogRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectLogRoute")
            .field("owner", &self.owner)
            .field("jsonl_log_path", &self.jsonl_log_path())
            .finish()
    }
}

impl ProjectLogRoute {
    /// The route of the project in `project_directory`, its JSONL file
    /// `<project>/.streamlib/logs/project.jsonl` appended to; the one already
    /// open in this process when there is one.
    ///
    /// Opened through the logging pathway the calling thread's dispatcher
    /// runs; with none of the engine's installed, or when the file cannot be
    /// opened (said once, as a warning), the route stamps records and writes
    /// no file.
    pub fn of_the_project_in(project_directory: &Path) -> Arc<Self> {
        let mut open_routes = project_log_routes_open_in_this_process().lock();
        if let Some(open_route) = open_routes
            .get(project_directory)
            .and_then(Weak::upgrade)
            .filter(|open_route| open_route.jsonl_log_file.is_some())
        {
            return open_route;
        }
        let route = Self::open_writing_its_jsonl_file_at(
            ProjectLogRouteOwner::Project(project_directory.to_path_buf()),
            project_log_path(project_directory),
        );
        open_routes.retain(|_, open_route| open_route.strong_count() > 0);
        open_routes.insert(project_directory.to_path_buf(), Arc::downgrade(&route));
        route
    }

    /// The route of the records of the runtime that no project's stream or
    /// action emitted, its JSONL file opened as
    /// `<runtime_own_log_directory>/tatolabd-<started_at_millis>.jsonl`.
    pub(crate) fn of_the_runtime_itself_in(runtime_own_log_directory: &Path) -> Arc<Self> {
        let started_at_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since_the_epoch| since_the_epoch.as_millis())
            .unwrap_or(0);
        Self::open_writing_its_jsonl_file_at(
            ProjectLogRouteOwner::TheRuntimeItself,
            runtime_own_log_directory.join(active_runtime_log_segment_file_name(
                RUNTIME_OWN_LOG_INSTANCE_NAME,
                started_at_millis,
            )),
        )
    }

    fn open_writing_its_jsonl_file_at(owner: ProjectLogRouteOwner, path: PathBuf) -> Arc<Self> {
        let jsonl_log_file = tracing::dispatcher::get_default(|dispatch| {
            dispatch
                .downcast_ref::<JsonlSinkLayer>()
                .and_then(JsonlSinkLayer::record_queue_that_writes_project_log_files)
        })
        .and_then(|(record_queue_drained_into_it, tunables)| {
            match JsonlBatchedWriter::open(
                &path,
                tunables.batch_bytes,
                tunables.fsync_on_every_batch,
                tunables.segment_rotation,
            ) {
                Ok(writer) => Some(ProjectJsonlLogFile {
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
            owner,
            jsonl_log_file,
        })
    }

    /// The project's directory; `None` for the runtime's own log.
    pub fn project_directory(&self) -> Option<&Path> {
        match &self.owner {
            ProjectLogRouteOwner::Project(project_directory) => Some(project_directory),
            ProjectLogRouteOwner::TheRuntimeItself => None,
        }
    }

    /// What this route's log holds, as a warning names it.
    fn what_this_log_holds(&self) -> String {
        self.owner.what_its_log_holds()
    }

    /// The log's active JSONL segment, `None` when it writes none.
    pub fn jsonl_log_path(&self) -> Option<&Path> {
        self.jsonl_log_file
            .as_ref()
            .map(|jsonl_log_file| jsonl_log_file.path.as_path())
    }

    /// Write every record queued before now, then flush and `fdatasync` the
    /// file; `false` when the drain worker did not write them within its
    /// budget.
    fn write_every_record_queued_before_now_and_sync(&self) -> bool {
        let Some(jsonl_log_file) = &self.jsonl_log_file else {
            return true;
        };
        let written = jsonl_log_file
            .record_queue_drained_into_it
            .wait_until_every_queued_record_is_written(
                QUEUED_RECORDS_WRITTEN_BEFORE_A_PROJECT_LOG_FLUSH_BUDGET,
            );
        self.flush_and_fsync_the_jsonl_log_file();
        written
    }

    /// Write every record queued before the close, then flush, `fdatasync`
    /// and close the file; a record routed here later reaches the pretty
    /// mirror only. Idempotent.
    pub(crate) fn close_the_jsonl_log_file(&self) {
        let Some(jsonl_log_file) = &self.jsonl_log_file else {
            return;
        };
        if jsonl_log_file.writer_while_open.lock().is_none() {
            return;
        }
        if !jsonl_log_file
            .record_queue_drained_into_it
            .wait_until_every_queued_record_is_written(
                QUEUED_RECORDS_WRITTEN_BEFORE_A_PROJECT_LOG_FLUSH_BUDGET,
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

    /// Append one serialized record to the open file; `false` when the route
    /// writes no file or it has closed.
    pub(crate) fn append_serialized_record(&self, serialized_record: &[u8]) -> bool {
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

impl Drop for ProjectLogRoute {
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

/// One emitter's way into a [`ProjectLogRoute`]: the runtime's id, the stream
/// that emits and the stream action it emits for, stamped on each record a
/// thread carrying the entry emits.
pub struct ProjectLogRouteEntry {
    route: Arc<ProjectLogRoute>,
    runtime_id: String,
    emitting_stream_id: Option<String>,
    emitting_action_id: Option<String>,
    /// The stream a thread carrying this entry works for, so a crash on it is
    /// pinned on the stream.
    stream_a_thread_carrying_it_works_for: StreamThisThreadWorksFor,
    /// This emitter's records a full queue dropped since the drain worker
    /// last reported them.
    records_dropped_from_a_full_queue: AtomicU64,
    helper_pipe_readers_still_reading: Mutex<usize>,
    a_helper_pipe_reader_finished: Condvar,
}

impl std::fmt::Debug for ProjectLogRouteEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectLogRouteEntry")
            .field("route", &self.route)
            .field("runtime_id", &self.runtime_id)
            .field("emitting_stream_id", &self.emitting_stream_id)
            .field("emitting_action_id", &self.emitting_action_id)
            .finish()
    }
}

impl ProjectLogRouteEntry {
    /// The entry the stream `stream_id` of the runtime `runtime_id` emits
    /// through into its project's log, for the stream action `action_id`
    /// that loaded it, when one did.
    pub fn for_a_stream_of_the_project_in(
        project_directory: &Path,
        runtime_id: &str,
        stream_id: &str,
        action_id: Option<&str>,
    ) -> Arc<Self> {
        Self::through(
            ProjectLogRoute::of_the_project_in(project_directory),
            runtime_id,
            Some(stream_id),
            action_id,
        )
    }

    /// The entry the stream action `action_id` of the runtime `runtime_id`
    /// emits through into its project's log, under no stream.
    pub fn for_a_stream_action_of_the_project_in(
        project_directory: &Path,
        runtime_id: &str,
        action_id: &str,
    ) -> Arc<Self> {
        Self::through(
            ProjectLogRoute::of_the_project_in(project_directory),
            runtime_id,
            None,
            Some(action_id),
        )
    }

    /// The entry the runtime `runtime_id` emits through into its own log.
    pub(crate) fn of_the_runtime_itself(
        route: Arc<ProjectLogRoute>,
        runtime_id: &str,
    ) -> Arc<Self> {
        Self::through(route, runtime_id, None, None)
    }

    fn through(
        route: Arc<ProjectLogRoute>,
        runtime_id: &str,
        emitting_stream_id: Option<&str>,
        emitting_action_id: Option<&str>,
    ) -> Arc<Self> {
        Arc::new(Self {
            route,
            runtime_id: runtime_id.to_string(),
            stream_a_thread_carrying_it_works_for: match emitting_stream_id {
                Some(stream_id) => StreamThisThreadWorksFor::of_the_stream_id(stream_id),
                None => StreamThisThreadWorksFor::NONE,
            },
            emitting_stream_id: emitting_stream_id.map(str::to_string),
            emitting_action_id: emitting_action_id.map(str::to_string),
            records_dropped_from_a_full_queue: AtomicU64::new(0),
            helper_pipe_readers_still_reading: Mutex::new(0),
            a_helper_pipe_reader_finished: Condvar::new(),
        })
    }

    /// The route this entry leads into.
    pub fn route(&self) -> &Arc<ProjectLogRoute> {
        &self.route
    }

    /// The id of the runtime the emitter runs in.
    pub fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    /// The emitting stream's id; `None` for a stream action's own records
    /// and the runtime's own log.
    pub fn stream_id(&self) -> Option<&str> {
        self.emitting_stream_id.as_deref()
    }

    /// The id of the stream action the emitter's records belong to; `None`
    /// for a stream its host loaded and the runtime's own log.
    pub fn action_id(&self) -> Option<&str> {
        self.emitting_action_id.as_deref()
    }

    /// The project log's active JSONL segment, `None` when it writes none.
    pub fn jsonl_log_path(&self) -> Option<&Path> {
        self.route.jsonl_log_path()
    }

    /// Route every record this thread emits through this entry, and pin a
    /// crash on it on the emitting stream, until the returned entry drops,
    /// when the entry the thread carried before is back.
    pub fn enter_on_this_thread(self: &Arc<Self>) -> ProjectLogRouteEntryEnteredOnThisThread {
        let entry_carried_before = PROJECT_LOG_ROUTE_ENTRY_OF_THIS_THREAD
            .try_with(|entry| entry.borrow_mut().replace(Arc::clone(self)))
            .ok()
            .flatten();
        let stream_worked_for_before =
            mark_this_thread_as_working_for(self.stream_a_thread_carrying_it_works_for);
        ProjectLogRouteEntryEnteredOnThisThread {
            entry_carried_before,
            stream_worked_for_before,
            entered_on_this_thread_only: PhantomData,
        }
    }

    /// Run `work` with this entry entered on the calling thread.
    pub fn run_entered<R>(self: &Arc<Self>, work: impl FnOnce() -> R) -> R {
        let _entered = self.enter_on_this_thread();
        work()
    }

    /// Let every reader of this emitter's helper pipes reach the end of what
    /// the helper wrote, write every record queued before then, then flush
    /// and `fdatasync` the project's log, so a reader of it finds the
    /// emitter's records whole.
    pub fn write_every_record_it_emitted_and_sync(&self) {
        let mut helper_pipe_readers_still_reading = self.helper_pipe_readers_still_reading.lock();
        let readers_outlasted_the_budget = self
            .a_helper_pipe_reader_finished
            .wait_while_for(
                &mut helper_pipe_readers_still_reading,
                |still_reading| *still_reading > 0,
                HELPER_PIPE_READERS_FINISHED_BEFORE_A_PROJECT_LOG_FLUSH_BUDGET,
            )
            .timed_out();
        let helper_pipe_readers_left_reading = *helper_pipe_readers_still_reading;
        drop(helper_pipe_readers_still_reading);
        if readers_outlasted_the_budget {
            tracing::warn!(
                "{} was synced while {helper_pipe_readers_left_reading} reader(s) of its \
                 helpers' pipes had not reached their end; what they read later lands after",
                self.what_this_entry_writes()
            );
        }
        if !self.route.write_every_record_queued_before_now_and_sync() {
            tracing::warn!(
                "{} was synced before the drain worker wrote every record queued for it",
                self.what_this_entry_writes()
            );
        }
    }

    /// What this entry writes, as a warning names it.
    fn what_this_entry_writes(&self) -> String {
        match &self.emitting_stream_id {
            Some(stream_id) => format!(
                "the stream `{stream_id}`'s part of the log of {}",
                self.route.what_this_log_holds()
            ),
            None => format!("the log of {}", self.route.what_this_log_holds()),
        }
    }

    /// Count one of this emitter's records dropped from a full queue; `true`
    /// for the first since the count was last taken.
    pub(crate) fn note_a_record_dropped_from_a_full_queue(&self) -> bool {
        self.records_dropped_from_a_full_queue
            .fetch_add(1, Ordering::Relaxed)
            == 0
    }

    /// The records of this emitter dropped from a full queue since the last
    /// take, reset to none.
    pub(crate) fn take_the_count_of_records_dropped(&self) -> u64 {
        self.records_dropped_from_a_full_queue
            .swap(0, Ordering::Relaxed)
    }
}

/// The runtime's own log, written every record no project's stream or action
/// emits while the engine that opened it lives.
pub(crate) struct TheRuntimesOwnLogWhileItsEngineLives {
    entry: Arc<ProjectLogRouteEntry>,
    runtime_own_log_entry_of_the_queue: Arc<Mutex<Option<Arc<ProjectLogRouteEntry>>>>,
}

impl TheRuntimesOwnLogWhileItsEngineLives {
    /// Open the runtime's own log under `runtime_own_log_directory` and have
    /// the drain worker of the calling thread's pathway write the records no
    /// project emits to it; `None` when no pathway of the engine's runs or the
    /// file cannot be opened, which the open says as a warning.
    pub(crate) fn open(runtime_id: &str, runtime_own_log_directory: &Path) -> Option<Self> {
        let route = ProjectLogRoute::of_the_runtime_itself_in(runtime_own_log_directory);
        let runtime_own_log_entry_of_the_queue = route
            .jsonl_log_file
            .as_ref()?
            .record_queue_drained_into_it
            .runtime_own_log_entry
            .clone();
        let entry = ProjectLogRouteEntry::of_the_runtime_itself(route, runtime_id);
        *runtime_own_log_entry_of_the_queue.lock() = Some(Arc::clone(&entry));
        Some(Self {
            entry,
            runtime_own_log_entry_of_the_queue,
        })
    }

    /// The log's active JSONL segment.
    pub(crate) fn jsonl_log_path(&self) -> Option<&Path> {
        self.entry.jsonl_log_path()
    }
}

impl Drop for TheRuntimesOwnLogWhileItsEngineLives {
    /// Closed while the drain worker still writes to it, so every record
    /// queued before the close lands; later ones reach the pretty mirror only.
    fn drop(&mut self) {
        self.entry.route.close_the_jsonl_log_file();
        let mut runtime_own_log_entry = self.runtime_own_log_entry_of_the_queue.lock();
        if runtime_own_log_entry
            .as_ref()
            .is_some_and(|installed| Arc::ptr_eq(installed, &self.entry))
        {
            *runtime_own_log_entry = None;
        }
    }
}

/// A [`ProjectLogRouteEntry`] entered on one thread; dropping it restores the
/// entry the thread carried before.
pub struct ProjectLogRouteEntryEnteredOnThisThread {
    entry_carried_before: Option<Arc<ProjectLogRouteEntry>>,
    stream_worked_for_before: StreamThisThreadWorksFor,
    entered_on_this_thread_only: PhantomData<*const ()>,
}

impl Drop for ProjectLogRouteEntryEnteredOnThisThread {
    fn drop(&mut self) {
        mark_this_thread_as_working_for(self.stream_worked_for_before);
        let entry_carried_before = self.entry_carried_before.take();
        let _ = PROJECT_LOG_ROUTE_ENTRY_OF_THIS_THREAD
            .try_with(|entry| *entry.borrow_mut() = entry_carried_before);
    }
}

/// The entry the calling thread carries, if any; none once the thread's
/// locals are being torn down, so a record a destructor emits never panics.
pub fn the_project_log_route_entry_of_this_thread() -> Option<Arc<ProjectLogRouteEntry>> {
    PROJECT_LOG_ROUTE_ENTRY_OF_THIS_THREAD
        .try_with(|entry| entry.borrow().clone())
        .ok()
        .flatten()
}

/// `work`, made to run in the entry the calling thread carries now — for a
/// thread or a blocking task this thread spawns on a stream's behalf.
pub fn carrying_this_threads_project_log_route_entry<R>(
    work: impl FnOnce() -> R,
) -> impl FnOnce() -> R {
    let entry_of_the_spawning_thread = the_project_log_route_entry_of_this_thread();
    move || {
        run_in_the_project_log_route_entry_when_there_is_one(
            entry_of_the_spawning_thread.as_ref(),
            work,
        )
    }
}

/// `work`, made to run in the entry the calling thread carries now, a sync of
/// that entry's records waiting for `work` until it returns or its budget runs
/// out — for a thread reading a helper's pipe to its end.
pub(crate) fn carrying_this_threads_project_log_route_entry_and_reading_a_helper_pipe<R>(
    work: impl FnOnce() -> R,
) -> impl FnOnce() -> R {
    let entry_of_the_spawning_thread = the_project_log_route_entry_of_this_thread();
    let reading_into_the_entry = entry_of_the_spawning_thread
        .as_ref()
        .map(HelperPipeReaderOfOneEmitterStillReading::counted_against);
    move || {
        let _reading_into_the_entry = reading_into_the_entry;
        run_in_the_project_log_route_entry_when_there_is_one(
            entry_of_the_spawning_thread.as_ref(),
            work,
        )
    }
}

/// One reader of an emitter's helper pipe a sync of its records waits for
/// until it drops.
struct HelperPipeReaderOfOneEmitterStillReading {
    entry: Arc<ProjectLogRouteEntry>,
}

impl HelperPipeReaderOfOneEmitterStillReading {
    fn counted_against(entry: &Arc<ProjectLogRouteEntry>) -> Self {
        *entry.helper_pipe_readers_still_reading.lock() += 1;
        Self {
            entry: Arc::clone(entry),
        }
    }
}

impl Drop for HelperPipeReaderOfOneEmitterStillReading {
    fn drop(&mut self) {
        let mut helper_pipe_readers_still_reading =
            self.entry.helper_pipe_readers_still_reading.lock();
        *helper_pipe_readers_still_reading -= 1;
        if *helper_pipe_readers_still_reading == 0 {
            self.entry.a_helper_pipe_reader_finished.notify_all();
        }
    }
}

/// Run `work` with `entry` entered when there is one — for a callback an OS
/// queue runs on a thread the stream did not spawn.
pub fn run_in_the_project_log_route_entry_when_there_is_one<R>(
    entry: Option<&Arc<ProjectLogRouteEntry>>,
    work: impl FnOnce() -> R,
) -> R {
    match entry {
        Some(entry) => entry.run_entered(work),
        None => work(),
    }
}

/// `future`, polled in `entry` on whichever thread polls it.
pub(crate) fn polled_in_a_project_log_route_entry<F: Future>(
    entry: Arc<ProjectLogRouteEntry>,
    future: F,
) -> FuturePolledInAProjectLogRouteEntry<F> {
    FuturePolledInAProjectLogRouteEntry { entry, future }
}

pin_project_lite::pin_project! {
    /// A future every poll of which runs with its emitter's entry entered.
    pub(crate) struct FuturePolledInAProjectLogRouteEntry<F> {
        entry: Arc<ProjectLogRouteEntry>,
        #[pin]
        future: F,
    }
}

impl<F: Future> Future for FuturePolledInAProjectLogRouteEntry<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        let _entered = this.entry.enter_on_this_thread();
        this.future.poll(context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_emitter_of_one_project_shares_one_route_and_another_project_has_its_own() {
        let first_project = tempfile::tempdir().unwrap();
        let second_project = tempfile::tempdir().unwrap();

        let camera = ProjectLogRouteEntry::for_a_stream_of_the_project_in(
            first_project.path(),
            "R-test",
            "camera",
            Some("R-test-1"),
        );
        let the_load = ProjectLogRouteEntry::for_a_stream_action_of_the_project_in(
            first_project.path(),
            "R-test",
            "R-test-1",
        );
        let elsewhere = ProjectLogRouteEntry::for_a_stream_of_the_project_in(
            second_project.path(),
            "R-test",
            "camera",
            None,
        );

        assert_eq!(camera.stream_id(), Some("camera"));
        assert_eq!(camera.action_id(), Some("R-test-1"));
        assert_eq!(the_load.stream_id(), None);
        assert_eq!(
            camera.route().project_directory(),
            Some(first_project.path())
        );
        assert_eq!(
            elsewhere.route().project_directory(),
            Some(second_project.path())
        );
        assert!(!Arc::ptr_eq(camera.route(), elsewhere.route()));
    }
}
