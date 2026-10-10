// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pinning a crash of the runtime on the stream whose thread crashed it.
//!
//! `docs/plan/changes/runtime-hosting.md`, decision 5 and "Pinning a crash": a
//! thread working for a stream carries the stream's name; handlers for the
//! fatal signals on the alternate stack, a hook on a panic that ends the
//! process, and the ends past a watchdog each append the crashing thread's
//! stream to the run-in-progress record — a file opened at the runtime's start
//! and removed at its clean end. A start that finds the record reads it as the
//! last run's crash.

use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::{CString, c_char};
use std::os::fd::IntoRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};
use std::sync::{Once, OnceLock};

use super::loaded_stream::what_a_panic_said;
use crate::core::{Error, Result};

/// The longest stream name a thread carries for its crash: a stream's cast
/// name is at most 63 bytes.
const LONGEST_STREAM_NAME_A_THREAD_CARRIES_FOR_ITS_CRASH: usize = 63;

/// The longest line one crash appends to the run-in-progress record.
const LONGEST_RUN_IN_PROGRESS_RECORD_LINE: usize = 512;

/// The mode the run-in-progress record is written at: its owner's alone.
const RUN_IN_PROGRESS_RECORD_FILE_MODE: u32 = 0o600;

/// The fatal signals a crash is pinned on, each with the cause a record line
/// names it by.
const FATAL_SIGNALS_PINNED_ON_A_STREAM: [(libc::c_int, &str); 5] = [
    (libc::SIGSEGV, "SIGSEGV"),
    (libc::SIGBUS, "SIGBUS"),
    (libc::SIGILL, "SIGILL"),
    (libc::SIGABRT, "SIGABRT"),
    (libc::SIGFPE, "SIGFPE"),
];

/// The lowest `si_code` a sender outside the kernel's fault path uses on
/// Apple's floors (`SI_USER` is `0x10001`); Linux's are zero or negative.
const LOWEST_SI_CODE_OF_A_SIGNAL_SENT_RATHER_THAN_RAISED_BY_A_FAULT: libc::c_int = 0x10000;

/// How many of the latest panics the hook keeps for a panic that escapes the
/// main thread to be matched against.
const LATEST_PANICS_KEPT_FOR_AN_ESCAPE_TO_MATCH: usize = 16;

/// The file descriptor slot's value while a run's record is being begun.
const RUN_IN_PROGRESS_RECORD_BEING_BEGUN: libc::c_int = -2;

/// The stream a thread works for, by its cast name, held where a fatal-signal
/// handler can read it without allocating or locking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamThisThreadWorksFor {
    name_bytes: [u8; LONGEST_STREAM_NAME_A_THREAD_CARRIES_FOR_ITS_CRASH],
    name_length: u8,
}

impl StreamThisThreadWorksFor {
    /// No stream: a thread a crash on implicates none.
    pub(crate) const NONE: Self = Self {
        name_bytes: [0; LONGEST_STREAM_NAME_A_THREAD_CARRIES_FOR_ITS_CRASH],
        name_length: 0,
    };

    /// The stream `stream_name` names, cut at the longest name a thread
    /// carries.
    pub(crate) fn named(stream_name: &str) -> Self {
        let mut stream = Self::NONE;
        let carried = stream_name
            .len()
            .min(LONGEST_STREAM_NAME_A_THREAD_CARRIES_FOR_ITS_CRASH);
        stream.name_bytes[..carried].copy_from_slice(&stream_name.as_bytes()[..carried]);
        stream.name_length = carried as u8;
        stream
    }

    fn name_bytes(&self) -> &[u8] {
        &self.name_bytes[..usize::from(self.name_length)]
    }

    fn is_none(&self) -> bool {
        self.name_length == 0
    }
}

/// The key whose per-thread value points at the stream the thread works for.
/// A key, not a thread-local: a handler reading a thread-local on a thread
/// that never touched it allocates on Apple's floors, and a key's read never
/// does.
static STREAM_THIS_THREAD_WORKS_FOR_KEY: OnceLock<Option<libc::pthread_key_t>> = OnceLock::new();

fn the_stream_this_thread_works_for_key() -> Option<libc::pthread_key_t> {
    *STREAM_THIS_THREAD_WORKS_FOR_KEY.get_or_init(|| {
        let mut key: libc::pthread_key_t = 0;
        // SAFETY: `key` is a valid out-parameter, and no destructor is named.
        match unsafe { libc::pthread_key_create(&mut key, None) } {
            0 => Some(key),
            _ => {
                tracing::warn!(
                    "no thread key for the stream a thread works for could be made, so a crash \
                     is pinned on no stream: {}",
                    std::io::Error::last_os_error()
                );
                None
            }
        }
    })
}

/// Mark the calling thread as working for the stream `stream` points at —
/// null for none — until it is marked again, returning what it carried
/// before. `stream` must outlive its mark.
pub(crate) fn mark_this_thread_as_working_for(
    stream: *const StreamThisThreadWorksFor,
) -> *const StreamThisThreadWorksFor {
    let Some(key) = the_stream_this_thread_works_for_key() else {
        return std::ptr::null();
    };
    // SAFETY: the key was made by `pthread_key_create`; the value is only
    // read back as a pointer to a `StreamThisThreadWorksFor` that outlives
    // its mark.
    unsafe {
        let carried_before = libc::pthread_getspecific(key) as *const StreamThisThreadWorksFor;
        libc::pthread_setspecific(key, stream.cast());
        carried_before
    }
}

/// The stream the calling thread works for. Async-signal-safe.
fn the_stream_this_thread_works_for() -> StreamThisThreadWorksFor {
    let Some(Some(key)) = STREAM_THIS_THREAD_WORKS_FOR_KEY.get() else {
        return StreamThisThreadWorksFor::NONE;
    };
    // SAFETY: a non-null value is a pointer the thread marked, live while
    // marked.
    unsafe {
        (libc::pthread_getspecific(*key) as *const StreamThisThreadWorksFor)
            .as_ref()
            .copied()
            .unwrap_or(StreamThisThreadWorksFor::NONE)
    }
}

thread_local! {
    // Read by a handler only while this thread unwinds, after the hook has
    // touched it on this thread.
    static STREAM_THIS_THREAD_WORKED_FOR_AT_ITS_LAST_PANIC: Cell<StreamThisThreadWorksFor> =
        const { Cell::new(StreamThisThreadWorksFor::NONE) };
}

/// The run-in-progress record's open file, `-1` while no run records.
static RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR: AtomicI32 = AtomicI32::new(-1);

/// The run-in-progress record's path while a run records, for an end that
/// cannot return to remove it; owned by the run that set it.
static RUN_IN_PROGRESS_RECORD_PATH: AtomicPtr<c_char> = AtomicPtr::new(std::ptr::null_mut());

/// The latest panics, newest last, for a panic that escapes the main thread.
static LATEST_PANICS: parking_lot::Mutex<VecDeque<PanicOfAThread>> =
    parking_lot::Mutex::new(VecDeque::new());

/// One panic as the hook saw it.
#[derive(Debug, Clone)]
struct PanicOfAThread {
    stream: StreamThisThreadWorksFor,
    what_it_said: String,
    location: String,
}

/// The run of the runtime in progress, recorded in a file the runtime removes
/// at its clean end; a crash leaves it behind, naming every stream it was
/// pinned on.
///
/// Dropped without an end while no panic unwinds — a start refused before it
/// counted the previous run's crash — it puts that crash back for the next
/// start to count, and otherwise ends cleanly.
#[derive(Debug)]
#[must_use = "a run's record is ended cleanly, or left by a crash"]
pub struct RuntimeRunInProgressRecord {
    record_path: PathBuf,
    previous_runs_record_until_it_is_counted: Option<Vec<u8>>,
    ended: bool,
}

/// How the runtime's previous run ended, as its start reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HowThePreviousRuntimeRunEnded {
    /// It reached its clean end — a signal stopped it, or its owner — or there
    /// was none.
    Cleanly,
    /// It crashed.
    Crashed(CrashOfThePreviousRuntimeRun),
}

impl HowThePreviousRuntimeRunEnded {
    /// The crash, `None` for a clean end.
    pub fn crash(&self) -> Option<&CrashOfThePreviousRuntimeRun> {
        match self {
            Self::Cleanly => None,
            Self::Crashed(crash) => Some(crash),
        }
    }
}

/// What the previous run's crash was pinned on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrashOfThePreviousRuntimeRun {
    /// Each stream the crash was pinned on, by its cast name, with the first
    /// cause pinned on it.
    pub causes_by_implicated_stream: BTreeMap<String, String>,
    /// Each cause pinned on a thread no stream owns. With none here and no
    /// stream implicated, the run ended with no line written: a `SIGKILL`, or
    /// an out-of-memory kill.
    pub causes_on_threads_no_stream_owns: Vec<String>,
}

impl RuntimeRunInProgressRecord {
    /// Read how the previous run ended from the record at `record_path`, then
    /// begin this run's record there and install the handlers that append to
    /// it. One run records at a time in a process.
    pub fn begin_this_run_reading_the_previous(
        record_path: &Path,
    ) -> Result<(Self, HowThePreviousRuntimeRunEnded)> {
        if RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR
            .compare_exchange(
                -1,
                RUN_IN_PROGRESS_RECORD_BEING_BEGUN,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            return Err(Error::Configuration(format!(
                "a run of the runtime is already recorded in this process; {} was not begun",
                record_path.display()
            )));
        }
        match Self::begin_with_the_slot_claimed(record_path) {
            Ok(begun) => Ok(begun),
            Err(refusal) => {
                RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR.store(-1, Ordering::SeqCst);
                Err(refusal)
            }
        }
    }

    fn begin_with_the_slot_claimed(
        record_path: &Path,
    ) -> Result<(Self, HowThePreviousRuntimeRunEnded)> {
        let refuse = |what_failed: String| {
            Error::Runtime(format!(
                "the runtime's run-in-progress record {} {what_failed}",
                record_path.display()
            ))
        };
        let record_path_for_an_end_that_cannot_return =
            CString::new(record_path.as_os_str().as_bytes())
                .map_err(|holds_a_nul| refuse(format!("cannot be named: {holds_a_nul}")))?;
        let previous_runs_record = match std::fs::read(record_path) {
            Ok(record_bytes) => Some(record_bytes),
            Err(not_read) if not_read.kind() == std::io::ErrorKind::NotFound => None,
            Err(not_read) => return Err(refuse(format!("cannot be read: {not_read}"))),
        };
        if previous_runs_record.is_some() {
            std::fs::remove_file(record_path)
                .map_err(|not_removed| refuse(format!("cannot be replaced: {not_removed}")))?;
        }
        let record_file = std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(RUN_IN_PROGRESS_RECORD_FILE_MODE)
            .open(record_path)
            .map_err(|not_created| {
                if let Some(previous_runs_record) = &previous_runs_record {
                    let _ = std::fs::write(record_path, previous_runs_record);
                }
                refuse(format!("cannot be created: {not_created}"))
            })?;
        sync_the_directory_of(record_path);
        RUN_IN_PROGRESS_RECORD_PATH.store(
            record_path_for_an_end_that_cannot_return.into_raw(),
            Ordering::SeqCst,
        );
        RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR.store(record_file.into_raw_fd(), Ordering::SeqCst);
        install_the_crash_handlers_once();
        let how_the_previous_run_ended = match &previous_runs_record {
            Some(record_bytes) => HowThePreviousRuntimeRunEnded::Crashed(
                CrashOfThePreviousRuntimeRun::read_from(record_bytes),
            ),
            None => HowThePreviousRuntimeRunEnded::Cleanly,
        };
        Ok((
            Self {
                record_path: record_path.to_path_buf(),
                previous_runs_record_until_it_is_counted: previous_runs_record,
                ended: false,
            },
            how_the_previous_run_ended,
        ))
    }

    /// Say the previous run's end is counted against the kept streams, so
    /// this run's end no longer puts it back.
    pub fn the_previous_runs_end_is_counted(&mut self) {
        self.previous_runs_record_until_it_is_counted = None;
    }

    /// End this run cleanly: its record is removed, so the next start reads
    /// no crash.
    pub fn end_this_run_cleanly(mut self) {
        self.end_cleanly();
    }

    fn end_cleanly(&mut self) {
        self.ended = true;
        stop_recording_this_run();
        if let Err(not_removed) = std::fs::remove_file(&self.record_path) {
            tracing::error!(
                "the runtime's run-in-progress record {} was not removed at its clean end, so \
                 its next start reads a crash: {not_removed}",
                self.record_path.display()
            );
        }
    }

    /// Put the previous run's record back in place of this run's, for the
    /// next start to count.
    fn end_putting_back_the_previous_runs_crash(&mut self, previous_runs_record: &[u8]) {
        self.ended = true;
        stop_recording_this_run();
        if let Err(not_written) = std::fs::write(&self.record_path, previous_runs_record) {
            tracing::error!(
                "the runtime's previous crash was not put back in {}, so its next start does not \
                 count it: {not_written}",
                self.record_path.display()
            );
        }
    }
}

impl Drop for RuntimeRunInProgressRecord {
    fn drop(&mut self) {
        if self.ended || std::thread::panicking() {
            return;
        }
        match self.previous_runs_record_until_it_is_counted.take() {
            Some(previous_runs_record) => {
                self.end_putting_back_the_previous_runs_crash(&previous_runs_record)
            }
            None => self.end_cleanly(),
        }
    }
}

impl CrashOfThePreviousRuntimeRun {
    /// The crash a record's lines describe: `<stream>\t<cause>`, the stream
    /// empty for a thread no stream owns. A line of any other shape is skipped.
    fn read_from(record_bytes: &[u8]) -> Self {
        let mut crash = Self::default();
        for line in String::from_utf8_lossy(record_bytes).lines() {
            let Some((stream_name, cause)) = line.split_once('\t') else {
                continue;
            };
            if stream_name.is_empty() {
                crash
                    .causes_on_threads_no_stream_owns
                    .push(cause.to_string());
            } else {
                crash
                    .causes_by_implicated_stream
                    .entry(stream_name.to_string())
                    .or_insert_with(|| cause.to_string());
            }
        }
        crash
    }

    /// Whether the run ended with no line written.
    pub fn left_no_trace(&self) -> bool {
        self.causes_by_implicated_stream.is_empty()
            && self.causes_on_threads_no_stream_owns.is_empty()
    }
}

/// Append a line pinning the runtime's crash on each of `stream_names`.
pub(crate) fn pin_the_runtimes_crash_on_each_stream(stream_names: &[String], cause: &str) {
    let cause = a_cause_fit_for_one_record_line(cause);
    for stream_name in stream_names {
        append_a_crash_line(
            StreamThisThreadWorksFor::named(stream_name).name_bytes(),
            &[cause.as_bytes()],
        );
    }
}

/// Append a line pinning the runtime's crash on no stream.
pub(crate) fn pin_the_runtimes_crash_on_no_stream(cause: &str) {
    append_a_crash_line(b"", &[a_cause_fit_for_one_record_line(cause).as_bytes()]);
}

/// Pin the runtime's crash on the stream whose thread raised the panic that
/// escaped the main thread with `escaped_panic_payload` — the latest panic
/// the hook saw that said the same — or on no stream when the hook saw none.
pub fn pin_the_runtimes_crash_on_the_panic_that_escaped_the_main_thread(
    escaped_panic_payload: &(dyn std::any::Any + Send),
) {
    let what_it_said = what_a_panic_said(escaped_panic_payload);
    let the_hooks_panic = LATEST_PANICS
        .lock()
        .iter()
        .rev()
        .find(|panic| panic.what_it_said == what_it_said)
        .cloned();
    let (stream, location) = the_hooks_panic
        .map_or((StreamThisThreadWorksFor::NONE, String::new()), |panic| {
            (panic.stream, panic.location)
        });
    let cause = a_cause_fit_for_one_record_line(&format!(
        "a panic that escaped the main thread: {what_it_said}{location}"
    ));
    append_a_crash_line(stream.name_bytes(), &[cause.as_bytes()]);
}

/// Remove the run-in-progress record as the owner ends the process at once —
/// the third interrupt — which is a stop, not a crash. Async-signal-safe.
pub(crate) fn end_the_run_in_progress_record_as_the_owner_ends_the_process() {
    let record_file_descriptor = RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR.swap(-1, Ordering::SeqCst);
    if record_file_descriptor < 0 {
        return;
    }
    let record_path = RUN_IN_PROGRESS_RECORD_PATH.swap(std::ptr::null_mut(), Ordering::SeqCst);
    // SAFETY: `close` and `unlink` are async-signal-safe; the descriptor was
    // this run's, and the path a NUL-terminated string it set, left to the
    // process's end.
    unsafe {
        libc::close(record_file_descriptor);
        if !record_path.is_null() {
            libc::unlink(record_path);
        }
    }
}

/// Close this run's record and let go of its path, so no handler appends to
/// it again.
fn stop_recording_this_run() {
    let record_file_descriptor = RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR.swap(-1, Ordering::SeqCst);
    if record_file_descriptor >= 0 {
        // SAFETY: the descriptor was this run's alone, and is closed once.
        unsafe { libc::close(record_file_descriptor) };
    }
    let record_path = RUN_IN_PROGRESS_RECORD_PATH.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !record_path.is_null() {
        // SAFETY: the pointer came from `CString::into_raw` and is reclaimed once.
        drop(unsafe { CString::from_raw(record_path) });
    }
}

fn sync_the_directory_of(record_path: &Path) {
    if let Some(record_directory) = record_path.parent()
        && let Ok(record_directory) = std::fs::File::open(record_directory)
    {
        let _ = record_directory.sync_all();
    }
}

/// `cause` with every tab and line break a space, so it stays one field of
/// one line.
fn a_cause_fit_for_one_record_line(cause: &str) -> String {
    cause
        .chars()
        .map(|character| match character {
            '\t' | '\n' | '\r' => ' ',
            other => other,
        })
        .collect()
}

/// Append `<stream>\t<cause parts>\n` to the run-in-progress record, cut to
/// fit one line. Async-signal-safe: no allocation, no lock, one `write`.
fn append_a_crash_line(stream_name: &[u8], cause_parts: &[&[u8]]) {
    let record_file_descriptor = RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR.load(Ordering::SeqCst);
    if record_file_descriptor < 0 {
        return;
    }
    let mut line = [0u8; LONGEST_RUN_IN_PROGRESS_RECORD_LINE];
    let mut line_length = 0;
    for part in [stream_name, b"\t".as_slice()].iter().chain(cause_parts) {
        let room_before_the_line_break = LONGEST_RUN_IN_PROGRESS_RECORD_LINE - 1 - line_length;
        let copied = part.len().min(room_before_the_line_break);
        line[line_length..line_length + copied].copy_from_slice(&part[..copied]);
        line_length += copied;
    }
    line[line_length] = b'\n';
    line_length += 1;
    // SAFETY: `write` is async-signal-safe and reads `line_length` bytes this
    // frame owns; a short or failed write leaves a line the reader skips.
    unsafe { libc::write(record_file_descriptor, line.as_ptr().cast(), line_length) };
}

/// The dispositions the crash handlers displaced, in the order of
/// [`FATAL_SIGNALS_PINNED_ON_A_STREAM`].
static DISPOSITIONS_THE_CRASH_HANDLERS_DISPLACED: OnceLock<
    [libc::sigaction; FATAL_SIGNALS_PINNED_ON_A_STREAM.len()],
> = OnceLock::new();

/// Install the fatal-signal handlers and the panic hook, once per process.
fn install_the_crash_handlers_once() {
    static CRASH_HANDLERS_INSTALLED_ONCE_PER_PROCESS: Once = Once::new();
    CRASH_HANDLERS_INSTALLED_ONCE_PER_PROCESS.call_once(|| {
        the_stream_this_thread_works_for_key();
        install_the_panic_hook();
        install_the_fatal_signal_handlers();
    });
}

fn install_the_fatal_signal_handlers() {
    // SAFETY: a zeroed `sigaction` is a valid out-parameter.
    let displaced = unsafe {
        let mut displaced: [libc::sigaction; FATAL_SIGNALS_PINNED_ON_A_STREAM.len()] =
            std::mem::zeroed();
        for (slot, (signal, _)) in displaced.iter_mut().zip(FATAL_SIGNALS_PINNED_ON_A_STREAM) {
            libc::sigaction(signal, std::ptr::null(), slot);
        }
        displaced
    };
    let _ = DISPOSITIONS_THE_CRASH_HANDLERS_DISPLACED.set(displaced);
    for (signal, signal_name) in FATAL_SIGNALS_PINNED_ON_A_STREAM {
        // SAFETY: a zeroed `sigaction` with its handler, flags and an empty
        // mask set is a valid disposition; the handler is an `extern "C"`
        // function of the three-argument shape `SA_SIGINFO` calls.
        let installed = unsafe {
            let mut crash_handler: libc::sigaction = std::mem::zeroed();
            crash_handler.sa_sigaction =
                pin_the_crash_on_the_threads_stream_then_hand_the_signal_on as *const () as usize;
            crash_handler.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigemptyset(&mut crash_handler.sa_mask);
            libc::sigaction(signal, &crash_handler, std::ptr::null_mut())
        };
        if installed != 0 {
            tracing::warn!(
                "the runtime's crash handler for {signal_name} was not installed, so a crash on \
                 it is pinned on no stream: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

extern "C" fn pin_the_crash_on_the_threads_stream_then_hand_the_signal_on(
    signal: libc::c_int,
    signal_information: *mut libc::siginfo_t,
    signal_context: *mut libc::c_void,
) {
    let a_panic_is_unwinding = std::thread::panicking();
    let mut stream = the_stream_this_thread_works_for();
    if stream.is_none() && a_panic_is_unwinding {
        stream = STREAM_THIS_THREAD_WORKED_FOR_AT_ITS_LAST_PANIC
            .try_with(Cell::get)
            .unwrap_or(StreamThisThreadWorksFor::NONE);
    }
    let signal_name = FATAL_SIGNALS_PINNED_ON_A_STREAM
        .iter()
        .find(|(pinned, _)| *pinned == signal)
        .map_or("a fatal signal", |(_, signal_name)| signal_name);
    let while_a_panic_unwound: &[u8] = if a_panic_is_unwinding {
        b" while a panic unwound"
    } else {
        b""
    };
    append_a_crash_line(
        stream.name_bytes(),
        &[signal_name.as_bytes(), while_a_panic_unwound],
    );
    hand_the_signal_on(signal, signal_information, signal_context);
}

/// Hand a fault the kernel raised to the disposition displaced — Rust's own
/// handler reports a stack overflow — and anything else to the default, so
/// the process ends on the signal it took.
fn hand_the_signal_on(
    signal: libc::c_int,
    signal_information: *mut libc::siginfo_t,
    signal_context: *mut libc::c_void,
) {
    let displaced = DISPOSITIONS_THE_CRASH_HANDLERS_DISPLACED
        .get()
        .and_then(|displaced| {
            FATAL_SIGNALS_PINNED_ON_A_STREAM
                .iter()
                .position(|(pinned, _)| *pinned == signal)
                .map(|position| displaced[position])
        });
    // SAFETY: the kernel hands a valid `siginfo_t` to an `SA_SIGINFO` handler.
    let si_code =
        unsafe { signal_information.as_ref() }.map_or(0, |information| information.si_code);
    let raised_by_a_fault = signal != libc::SIGABRT
        && si_code > 0
        && si_code < LOWEST_SI_CODE_OF_A_SIGNAL_SENT_RATHER_THAN_RAISED_BY_A_FAULT;
    // SAFETY: `sigaction` and `raise` are async-signal-safe; a displaced
    // handler is called with the arguments its flags say it takes.
    unsafe {
        if let Some(displaced) = displaced
            && raised_by_a_fault
            && displaced.sa_sigaction != libc::SIG_DFL
            && displaced.sa_sigaction != libc::SIG_IGN
        {
            libc::sigaction(signal, &displaced, std::ptr::null_mut());
            if displaced.sa_flags & libc::SA_SIGINFO != 0 {
                let displaced_handler: extern "C" fn(
                    libc::c_int,
                    *mut libc::siginfo_t,
                    *mut libc::c_void,
                ) = std::mem::transmute(displaced.sa_sigaction);
                displaced_handler(signal, signal_information, signal_context);
            } else {
                let displaced_handler: extern "C" fn(libc::c_int) =
                    std::mem::transmute(displaced.sa_sigaction);
                displaced_handler(signal);
            }
            return;
        }
        let mut default_disposition: libc::sigaction = std::mem::zeroed();
        default_disposition.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut default_disposition.sa_mask);
        libc::sigaction(signal, &default_disposition, std::ptr::null_mut());
        // Blocked while this handler runs, so delivered on its return; a
        // fault that returns instead faults again under the default.
        libc::raise(signal);
    }
}

/// A hook noting each panic's thread, stream and message, composed with the
/// hook before it. It writes nothing: a caught panic records nothing, and one
/// that aborts or escapes the main thread is pinned where the process ends.
fn install_the_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_information| {
        let stream = the_stream_this_thread_works_for();
        let _ = STREAM_THIS_THREAD_WORKED_FOR_AT_ITS_LAST_PANIC
            .try_with(|at_the_last_panic| at_the_last_panic.set(stream));
        let panic_of_this_thread = PanicOfAThread {
            stream,
            what_it_said: what_a_panic_said(panic_information.payload()).to_string(),
            location: panic_information
                .location()
                .map(|location| format!(" at {}:{}", location.file(), location.line()))
                .unwrap_or_default(),
        };
        {
            let mut latest_panics = LATEST_PANICS.lock();
            if latest_panics.len() == LATEST_PANICS_KEPT_FOR_AN_ESCAPE_TO_MATCH {
                latest_panics.pop_front();
            }
            latest_panics.push_back(panic_of_this_thread);
        }
        previous_hook(panic_information);
    }));
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Arc;

    use super::*;
    use crate::core::logging::LoadedStreamLogRoute;
    use crate::core::test_support::{
        a_temporary_directory_at_owner_only_mode, rerun_this_test_in_a_child_process,
    };

    /// Set only in the child process a test re-runs itself in, to the
    /// directory its run-in-progress record sits in.
    const CRASH_RECORD_CHILD_DIRECTORY_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_RUNTIME_CRASH_RECORD_CHILD_DIRECTORY";

    const RECORD_FILE_NAME: &str = "runtime-run-in-progress";

    /// The route a thread of the stream `stream_name` carries, writing no file.
    fn the_log_route_of_the_stream(
        stream_name: &str,
        project_directory: &Path,
    ) -> Arc<LoadedStreamLogRoute> {
        LoadedStreamLogRoute::open_in_project_directory("R-test", stream_name, project_directory)
    }

    /// Re-run `test_path` in a child process holding a fresh directory for its
    /// record, and hand back how it ended and what its record holds.
    fn the_childs_end_and_its_record(test_path: &str) -> (std::process::Output, Option<String>) {
        let directory = a_temporary_directory_at_owner_only_mode().expect("a record directory");
        let child = rerun_this_test_in_a_child_process(
            test_path,
            CRASH_RECORD_CHILD_DIRECTORY_ENVIRONMENT_VARIABLE,
            directory.path().as_os_str(),
        );
        let record = std::fs::read_to_string(directory.path().join(RECORD_FILE_NAME)).ok();
        (child, record)
    }

    /// End `run` as the process's death does: nothing more is written, and
    /// the record stays as it is.
    fn end_as_the_process_does_on_a_crash(mut run: RuntimeRunInProgressRecord) {
        run.ended = true;
        stop_recording_this_run();
    }

    fn the_child_directory() -> Option<PathBuf> {
        std::env::var_os(CRASH_RECORD_CHILD_DIRECTORY_ENVIRONMENT_VARIABLE).map(PathBuf::from)
    }

    #[test]
    fn a_record_names_each_stream_by_its_first_cause_and_keeps_each_cause_no_stream_owns() {
        let crash = CrashOfThePreviousRuntimeRun::read_from(
            b"crasher\tSIGSEGV\n\tSIGBUS\nnot a crash line\ncrasher\tSIGABRT\nother\texit 124\n",
        );

        assert_eq!(
            crash.causes_by_implicated_stream,
            BTreeMap::from([
                ("crasher".to_string(), "SIGSEGV".to_string()),
                ("other".to_string(), "exit 124".to_string()),
            ])
        );
        assert_eq!(crash.causes_on_threads_no_stream_owns, ["SIGBUS"]);
        assert!(!crash.left_no_trace());
        assert!(CrashOfThePreviousRuntimeRun::read_from(b"").left_no_trace());
    }

    #[test]
    fn a_stream_name_past_the_longest_a_thread_carries_is_cut() {
        let long_name = "s".repeat(80);

        let carried = StreamThisThreadWorksFor::named(&long_name);

        assert_eq!(
            carried.name_bytes(),
            &long_name.as_bytes()[..LONGEST_STREAM_NAME_A_THREAD_CARRIES_FOR_ITS_CRASH]
        );
        assert!(StreamThisThreadWorksFor::NONE.is_none());
    }

    /// A `SIGSEGV` on a thread carrying a stream's log route is pinned on that
    /// stream, and the process still ends on the signal.
    #[test]
    fn a_fatal_signal_on_a_thread_working_for_a_stream_is_pinned_on_it_and_ends_the_process() {
        if let Some(directory) = the_child_directory() {
            let (_this_run_until_the_process_ends, _) =
                RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(
                    &directory.join(RECORD_FILE_NAME),
                )
                .expect("the run's record begins");
            let route = the_log_route_of_the_stream("crasher", &directory);
            std::thread::spawn(move || {
                let _entered = route.enter_on_this_thread();
                // SAFETY: `raise` takes a signal number.
                unsafe { libc::raise(libc::SIGSEGV) };
            })
            .join()
            .expect("the thread runs");
            panic!("a SIGSEGV did not end the process");
        }

        let (child, record) = the_childs_end_and_its_record(
            "core::runtime::runtime_crash_pinned_on_a_stream::tests::a_fatal_signal_on_a_thread_working_for_a_stream_is_pinned_on_it_and_ends_the_process",
        );

        assert_eq!(child.status.signal(), Some(libc::SIGSEGV), "{child:?}");
        assert_eq!(record.as_deref(), Some("crasher\tSIGSEGV\n"));
    }

    /// An abort on a thread no stream's route is entered on — including one
    /// whose route has since been left — implicates no stream.
    #[test]
    fn a_fatal_signal_on_a_thread_working_for_no_stream_is_pinned_on_none() {
        if let Some(directory) = the_child_directory() {
            let (_this_run_until_the_process_ends, _) =
                RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(
                    &directory.join(RECORD_FILE_NAME),
                )
                .expect("the run's record begins");
            the_log_route_of_the_stream("left-before-the-crash", &directory).run_entered(|| {});
            std::process::abort();
        }

        let (child, record) = the_childs_end_and_its_record(
            "core::runtime::runtime_crash_pinned_on_a_stream::tests::a_fatal_signal_on_a_thread_working_for_no_stream_is_pinned_on_none",
        );

        assert_eq!(child.status.signal(), Some(libc::SIGABRT), "{child:?}");
        assert_eq!(record.as_deref(), Some("\tSIGABRT\n"));
    }

    /// The hook writes nothing for a panic a thread catches; a panic that
    /// escapes the main thread is pinned on the stream its thread worked for,
    /// never on a bystander that panicked after it.
    #[test]
    fn a_caught_panic_records_nothing_and_one_escaping_the_main_thread_is_pinned_on_its_stream() {
        if let Some(directory) = the_child_directory() {
            let record_path = directory.join(RECORD_FILE_NAME);
            let (_this_run_until_the_process_ends, _) =
                RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(&record_path)
                    .expect("the run's record begins");
            let caught = the_log_route_of_the_stream("caught", &directory)
                .run_entered(|| std::panic::catch_unwind(|| panic!("a panic its thread catches")));
            assert!(caught.is_err());
            assert_eq!(
                std::fs::read_to_string(&record_path).expect("the record is there"),
                "",
                "a caught panic recorded something"
            );
            let escaped_panic_payload = std::panic::catch_unwind(|| {
                the_log_route_of_the_stream("escaping", &directory)
                    .run_entered(|| panic!("a panic that ends the process"))
            })
            .expect_err("the panic escaped");
            let bystander_route = the_log_route_of_the_stream("bystander", &directory);
            std::thread::spawn(move || {
                bystander_route.run_entered(|| {
                    std::panic::catch_unwind(|| panic!("a bystander's caught panic"))
                })
            })
            .join()
            .expect("the bystander's thread runs")
            .expect_err("the bystander panicked");
            pin_the_runtimes_crash_on_the_panic_that_escaped_the_main_thread(
                &*escaped_panic_payload,
            );
            std::process::exit(0);
        }

        let (child, record) = the_childs_end_and_its_record(
            "core::runtime::runtime_crash_pinned_on_a_stream::tests::a_caught_panic_records_nothing_and_one_escaping_the_main_thread_is_pinned_on_its_stream",
        );

        assert!(child.status.success(), "{child:?}");
        let record = record.expect("the record is left behind");
        assert!(
            record.starts_with(
                "escaping\ta panic that escaped the main thread: a panic that ends the process at "
            ),
            "{record}"
        );
        assert_eq!(record.lines().count(), 1, "{record}");
    }

    /// A clean end removes the record, so the next start reads none; an end as
    /// a crash leaves the streams it names; the owner's end at once removes it.
    #[test]
    fn a_clean_end_leaves_no_crash_and_a_crash_end_leaves_the_streams_it_was_pinned_on() {
        if let Some(directory) = the_child_directory() {
            let record_path = directory.join(RECORD_FILE_NAME);
            let begin = || {
                RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(&record_path)
                    .expect("the run's record begins")
            };

            let (first_run, how_the_first_previous_run_ended) = begin();
            assert_eq!(
                how_the_first_previous_run_ended,
                HowThePreviousRuntimeRunEnded::Cleanly
            );
            assert!(
                RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(&record_path)
                    .is_err(),
                "a second run began while one was recorded"
            );
            first_run.end_this_run_cleanly();
            assert!(!record_path.exists());

            let (second_run, how_the_second_previous_run_ended) = begin();
            assert_eq!(
                how_the_second_previous_run_ended,
                HowThePreviousRuntimeRunEnded::Cleanly
            );
            pin_the_runtimes_crash_on_each_stream(
                &["first".to_string(), "second".to_string()],
                "exit 124:\tabandoned",
            );
            end_as_the_process_does_on_a_crash(second_run);

            let (mut third_run, how_the_third_previous_run_ended) = begin();
            assert_eq!(
                how_the_third_previous_run_ended,
                HowThePreviousRuntimeRunEnded::Crashed(CrashOfThePreviousRuntimeRun {
                    causes_by_implicated_stream: BTreeMap::from([
                        ("first".to_string(), "exit 124: abandoned".to_string()),
                        ("second".to_string(), "exit 124: abandoned".to_string()),
                    ]),
                    causes_on_threads_no_stream_owns: Vec::new(),
                })
            );
            third_run.the_previous_runs_end_is_counted();
            end_the_run_in_progress_record_as_the_owner_ends_the_process();
            assert!(!record_path.exists());
            end_as_the_process_does_on_a_crash(third_run);

            let (_fourth_run, how_the_fourth_previous_run_ended) = begin();
            assert_eq!(
                how_the_fourth_previous_run_ended,
                HowThePreviousRuntimeRunEnded::Cleanly
            );
            assert_eq!(
                std::fs::metadata(&record_path)
                    .expect("the fourth run's record is there")
                    .permissions()
                    .mode()
                    & 0o777,
                RUN_IN_PROGRESS_RECORD_FILE_MODE
            );
            std::process::exit(0);
        }

        let (child, record) = the_childs_end_and_its_record(
            "core::runtime::runtime_crash_pinned_on_a_stream::tests::a_clean_end_leaves_no_crash_and_a_crash_end_leaves_the_streams_it_was_pinned_on",
        );

        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert_eq!(
            record.as_deref(),
            Some(""),
            "a run never ended reads as a crash pinned on nothing"
        );
    }

    /// A run dropped before it counted the previous run's crash — a start
    /// refused early — puts that crash back for the next start; one dropped
    /// after counting ends cleanly.
    #[test]
    fn a_run_dropped_before_counting_puts_the_previous_crash_back_and_after_counting_ends_cleanly()
    {
        if let Some(directory) = the_child_directory() {
            let record_path = directory.join(RECORD_FILE_NAME);
            std::fs::write(&record_path, "crasher\tSIGSEGV\n").unwrap();
            let begin = || {
                RuntimeRunInProgressRecord::begin_this_run_reading_the_previous(&record_path)
                    .expect("the run's record begins")
            };

            let (refused_start, _) = begin();
            drop(refused_start);
            let (mut counted_start, how_the_previous_run_ended) = begin();
            assert_eq!(
                how_the_previous_run_ended
                    .crash()
                    .map(|crash| crash.causes_by_implicated_stream.clone()),
                Some(BTreeMap::from([(
                    "crasher".to_string(),
                    "SIGSEGV".to_string()
                )])),
                "the refused start did not put the crash back"
            );
            counted_start.the_previous_runs_end_is_counted();
            drop(counted_start);
            assert!(!record_path.exists());
            std::process::exit(0);
        }

        let (child, record) = the_childs_end_and_its_record(
            "core::runtime::runtime_crash_pinned_on_a_stream::tests::a_run_dropped_before_counting_puts_the_previous_crash_back_and_after_counting_ends_cleanly",
        );

        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert_eq!(record, None);
    }
}
