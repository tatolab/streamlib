// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pinning a crash of the runtime on the stream whose thread crashed it.
//!
//! `docs/plan/changes/runtime-hosting.md`, decision 5 and "Pinning a crash": a
//! thread working for a stream carries the stream's name; handlers for the
//! fatal signals on the alternate stack, a hook on a panic that ends the
//! process, and the end past the abandoned-thread bound each append the
//! crashing thread's stream to the run-in-progress record — a file opened at
//! the runtime's start and removed at its clean end. A start that finds the
//! record reads it as the last run's crash.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::os::fd::IntoRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Once, OnceLock};

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

thread_local! {
    // `const`-initialised and never dropped, so a signal handler reads it
    // without the thread-local's lazy registration.
    static STREAM_THIS_THREAD_WORKS_FOR: Cell<StreamThisThreadWorksFor> =
        const { Cell::new(StreamThisThreadWorksFor::NONE) };
    static STREAM_THIS_THREAD_WORKED_FOR_AT_ITS_LAST_PANIC: Cell<StreamThisThreadWorksFor> =
        const { Cell::new(StreamThisThreadWorksFor::NONE) };
}

/// Mark the calling thread as working for `stream` until it is marked again,
/// returning what it worked for before.
pub(crate) fn mark_this_thread_as_working_for(
    stream: StreamThisThreadWorksFor,
) -> StreamThisThreadWorksFor {
    STREAM_THIS_THREAD_WORKS_FOR
        .try_with(|carried| carried.replace(stream))
        .unwrap_or(StreamThisThreadWorksFor::NONE)
}

fn the_stream_this_thread_works_for() -> StreamThisThreadWorksFor {
    STREAM_THIS_THREAD_WORKS_FOR
        .try_with(Cell::get)
        .unwrap_or(StreamThisThreadWorksFor::NONE)
}

/// The run-in-progress record's open file, `-1` while no run records.
static RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR: AtomicI32 = AtomicI32::new(-1);

/// The run-in-progress record's path, for an end that cannot return to remove it.
static RUN_IN_PROGRESS_RECORD_PATH: OnceLock<CString> = OnceLock::new();

/// The last panic on any thread, for a panic that escapes the main thread.
static THE_LAST_PANIC: parking_lot::Mutex<Option<PanicOfAThread>> = parking_lot::Mutex::new(None);

/// One panic as the hook saw it.
#[derive(Debug, Clone)]
struct PanicOfAThread {
    stream: StreamThisThreadWorksFor,
    what_it_said: String,
}

/// The run of the runtime in progress, recorded in a file the runtime removes
/// at its clean end; a crash leaves it behind, naming every stream it was
/// pinned on.
#[derive(Debug)]
pub struct RuntimeRunInProgressRecord {
    record_path: PathBuf,
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

/// What the previous run's crash was pinned on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrashOfThePreviousRuntimeRun {
    /// Each stream the crash was pinned on, by its cast name, with the first
    /// cause pinned on it.
    pub causes_by_implicated_stream: BTreeMap<String, String>,
    /// Each cause pinned on a thread no stream owns. With none here and no
    /// stream implicated, the crash left no trace: a `SIGKILL`, or an
    /// out-of-memory kill.
    pub causes_on_threads_no_stream_owns: Vec<String>,
}

impl RuntimeRunInProgressRecord {
    /// Read how the previous run ended from the record at `record_path`, then
    /// begin this run's record there and install the handlers that append to
    /// it. One run records at a time in a process.
    pub fn begin_this_run_reading_the_previous(
        record_path: &Path,
    ) -> Result<(Self, HowThePreviousRuntimeRunEnded)> {
        if RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR.load(Ordering::SeqCst) >= 0 {
            return Err(Error::Configuration(format!(
                "a run of the runtime is already recorded in this process; {} was not begun",
                record_path.display()
            )));
        }
        let refuse = |what_failed: String| {
            Error::Runtime(format!(
                "the runtime's run-in-progress record {} {what_failed}",
                record_path.display()
            ))
        };
        let how_the_previous_run_ended = match std::fs::read(record_path) {
            Ok(record_bytes) => HowThePreviousRuntimeRunEnded::Crashed(
                CrashOfThePreviousRuntimeRun::read_from(&record_bytes),
            ),
            Err(not_read) if not_read.kind() == std::io::ErrorKind::NotFound => {
                HowThePreviousRuntimeRunEnded::Cleanly
            }
            Err(not_read) => return Err(refuse(format!("cannot be read: {not_read}"))),
        };
        match std::fs::remove_file(record_path) {
            Ok(()) => {}
            Err(not_removed) if not_removed.kind() == std::io::ErrorKind::NotFound => {}
            Err(not_removed) => return Err(refuse(format!("cannot be replaced: {not_removed}"))),
        }
        let record_file = std::fs::OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(RUN_IN_PROGRESS_RECORD_FILE_MODE)
            .open(record_path)
            .map_err(|not_created| refuse(format!("cannot be created: {not_created}")))?;
        let record_path_for_an_end_that_cannot_return =
            CString::new(record_path.as_os_str().as_bytes())
                .map_err(|holds_a_nul| refuse(format!("cannot be named: {holds_a_nul}")))?;
        if let Some(record_directory) = record_path.parent()
            && let Ok(record_directory) = std::fs::File::open(record_directory)
        {
            let _ = record_directory.sync_all();
        }
        let _ = RUN_IN_PROGRESS_RECORD_PATH.set(record_path_for_an_end_that_cannot_return);
        RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR.store(record_file.into_raw_fd(), Ordering::SeqCst);
        install_the_crash_handlers_once();
        Ok((
            Self {
                record_path: record_path.to_path_buf(),
            },
            how_the_previous_run_ended,
        ))
    }

    /// End this run cleanly: its record is removed, so the next start reads
    /// no crash.
    pub fn end_this_run_cleanly(self) {
        close_the_run_in_progress_record();
        if let Err(not_removed) = std::fs::remove_file(&self.record_path) {
            tracing::error!(
                "the runtime's run-in-progress record {} was not removed at its clean end, so \
                 its next start reads a crash: {not_removed}",
                self.record_path.display()
            );
        }
    }

    /// End this run as a crash pinned on each of `stream_names`, `cause`
    /// naming why: its record stays for the next start to read.
    pub fn end_this_run_as_a_crash_pinned_on(self, stream_names: &[String], cause: &str) {
        pin_the_runtimes_crash_on_each_stream(stream_names, cause);
        close_the_run_in_progress_record();
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

    /// Whether the crash was pinned on nothing at all.
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
            cause.as_bytes(),
        );
    }
}

/// Pin the runtime's crash on the stream the last panic's thread worked for,
/// a panic that escaped the main thread and so ends the process.
pub fn pin_the_runtimes_crash_on_the_panic_that_escaped_the_main_thread() {
    let Some(last_panic) = THE_LAST_PANIC.lock().clone() else {
        append_a_crash_line(b"", b"a panic escaped the main thread");
        return;
    };
    let cause = a_cause_fit_for_one_record_line(&format!(
        "a panic that escaped the main thread: {}",
        last_panic.what_it_said
    ));
    append_a_crash_line(last_panic.stream.name_bytes(), cause.as_bytes());
}

/// Remove the run-in-progress record as the owner ends the process at once —
/// the third interrupt — which is a stop, not a crash. Async-signal-safe.
pub(crate) fn end_the_run_in_progress_record_as_the_owner_ends_the_process() {
    let record_file_descriptor = RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR.swap(-1, Ordering::SeqCst);
    if record_file_descriptor < 0 {
        return;
    }
    // SAFETY: `close` and `unlink` are async-signal-safe; the descriptor was
    // this module's, and the path a NUL-terminated string set once.
    unsafe {
        libc::close(record_file_descriptor);
        if let Some(record_path) = RUN_IN_PROGRESS_RECORD_PATH.get() {
            libc::unlink(record_path.as_ptr());
        }
    }
}

fn close_the_run_in_progress_record() {
    let record_file_descriptor = RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR.swap(-1, Ordering::SeqCst);
    if record_file_descriptor >= 0 {
        // SAFETY: the descriptor was this module's alone, and is closed once.
        unsafe { libc::close(record_file_descriptor) };
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

/// Append `<stream>\t<cause>\n` to the run-in-progress record, each part cut
/// to fit one line. Async-signal-safe: no allocation, no lock, one `write`.
fn append_a_crash_line(stream_name: &[u8], cause: &[u8]) {
    let record_file_descriptor = RUN_IN_PROGRESS_RECORD_FILE_DESCRIPTOR.load(Ordering::SeqCst);
    if record_file_descriptor < 0 {
        return;
    }
    let mut line = [0u8; LONGEST_RUN_IN_PROGRESS_RECORD_LINE];
    let mut line_length = 0;
    for part in [stream_name, b"\t", cause] {
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
struct DispositionsTheCrashHandlersDisplaced([libc::sigaction; 5]);

// SAFETY: written once before any handler can read it, and only read after.
unsafe impl Sync for DispositionsTheCrashHandlersDisplaced {}
// SAFETY: plain data the kernel filled in.
unsafe impl Send for DispositionsTheCrashHandlersDisplaced {}

static DISPOSITIONS_THE_CRASH_HANDLERS_DISPLACED: OnceLock<DispositionsTheCrashHandlersDisplaced> =
    OnceLock::new();

/// Install the fatal-signal handlers and the panic hook, once per process.
fn install_the_crash_handlers_once() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        install_the_panic_hook();
        install_the_fatal_signal_handlers();
    });
}

fn install_the_fatal_signal_handlers() {
    // SAFETY: a zeroed `sigaction` is a valid out-parameter and, with its
    // handler, flags and an empty mask set, a valid disposition to install.
    let displaced = unsafe {
        let mut displaced: [libc::sigaction; 5] = std::mem::zeroed();
        for (slot, (signal, _)) in displaced.iter_mut().zip(FATAL_SIGNALS_PINNED_ON_A_STREAM) {
            libc::sigaction(signal, std::ptr::null(), slot);
        }
        displaced
    };
    let _ = DISPOSITIONS_THE_CRASH_HANDLERS_DISPLACED
        .set(DispositionsTheCrashHandlersDisplaced(displaced));
    for (signal, signal_name) in FATAL_SIGNALS_PINNED_ON_A_STREAM {
        // SAFETY: as above; the handler is an `extern "C"` function with the
        // three-argument shape `SA_SIGINFO` calls.
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
    if a_panic_is_unwinding {
        let mut cause = [0u8; 64];
        let mut cause_length = 0;
        for part in [signal_name.as_bytes(), b" while a panic unwound"] {
            cause[cause_length..cause_length + part.len()].copy_from_slice(part);
            cause_length += part.len();
        }
        append_a_crash_line(stream.name_bytes(), &cause[..cause_length]);
    } else {
        append_a_crash_line(stream.name_bytes(), signal_name.as_bytes());
    }
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
                .map(|position| displaced.0[position])
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

/// A hook noting each panic's thread and stream, composed with the hook
/// before it. It writes nothing: a caught panic records nothing, and one that
/// aborts or escapes the main thread is pinned where the process ends.
fn install_the_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_information| {
        let stream = the_stream_this_thread_works_for();
        let _ = STREAM_THIS_THREAD_WORKED_FOR_AT_ITS_LAST_PANIC
            .try_with(|at_the_last_panic| at_the_last_panic.set(stream));
        let what_it_said = panic_information
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| {
                panic_information
                    .payload()
                    .downcast_ref::<String>()
                    .map(String::as_str)
            })
            .unwrap_or("a panic that carried no message");
        let location = panic_information
            .location()
            .map(|location| format!(" at {}:{}", location.file(), location.line()))
            .unwrap_or_default();
        *THE_LAST_PANIC.lock() = Some(PanicOfAThread {
            stream,
            what_it_said: format!("{what_it_said}{location}"),
        });
        previous_hook(panic_information);
    }));
}
