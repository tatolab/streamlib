// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Fd-level interceptor for stdio. Redirects fds 1 and 2 through pipes
//! so raw `println!` / `printf` / `libc::write(1, …)` output surfaces
//! as intercepted `tracing::warn!` events in the unified JSONL
//! pathway. Defense-in-depth companion to the clippy `disallowed_macros`
//! lockout (#441): catches third-party dep chatter and transitive C
//! calls that the compile-time rule can't see.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::Dispatch;

/// How long dropping the interceptor waits for its reader threads to see end of
/// file.
///
/// Every process the app spawned inherits fds 1 and 2, which are the pipes'
/// write ends, so a reader cannot see end of file before the last of them
/// exits. Waiting on it without a bound let anything the app started hold the
/// app's own teardown open.
const INTERCEPT_READER_JOIN_BUDGET: Duration = Duration::from_secs(1);

const INTERCEPT_READER_JOIN_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) struct StdioInterceptor {
    saved_stdout: Option<OwnedFd>,
    saved_stderr: Option<OwnedFd>,
    fd1_reader: Option<JoinHandle<()>>,
    fd2_reader: Option<JoinHandle<()>>,
}

/// Fd-redirect installed without reader threads yet. Installed first so
/// the pretty-mirror sink (a [`File`] over the dup'd real stdout) can
/// be handed to the worker BEFORE the `tracing::Dispatch` — which
/// wraps the worker's queue — exists. Readers are then started with
/// [`StdioInterceptorPending::start_readers`] once the dispatch is
/// built.
pub(crate) struct StdioInterceptorPending {
    saved_stdout: OwnedFd,
    saved_stderr: OwnedFd,
    fd1_read: OwnedFd,
    fd2_read: OwnedFd,
}

/// Dup'd originals of fds 1/2 suitable for the pretty-mirror layer to
/// write to without re-entering the intercept pipe. The pretty-mirror
/// MUST write to these and not to fd 1 / fd 2 directly — otherwise
/// mirror output gets captured by the reader thread and re-emitted,
/// producing infinite recursion.
pub(crate) struct StdioInterceptorFiles {
    pub real_stdout: File,
    pub real_stderr: File,
}

/// Install the fd-level redirects. `dup` fds 1/2 for (a) the mirror
/// sink and (b) later restoration, create pipes, and `dup2` the pipe
/// write ends onto fds 1/2. Reader threads are NOT spawned yet —
/// call [`StdioInterceptorPending::start_readers`] once a
/// `tracing::Dispatch` is available.
pub(crate) fn install_redirects()
-> std::io::Result<(StdioInterceptorPending, StdioInterceptorFiles)> {
    // Dup fd 1 twice: one copy becomes the pretty-mirror sink, one is
    // stashed for restoration in Drop. Same for fd 2. MUST happen
    // BEFORE the dup2 redirects below — otherwise the "real" handles
    // would end up pointing at the pipe write ends, and the
    // pretty-mirror would recurse into the interceptor.
    let mirror_stdout = dup_fd(libc::STDOUT_FILENO)?;
    let saved_stdout = dup_fd(libc::STDOUT_FILENO)?;
    let mirror_stderr = dup_fd(libc::STDERR_FILENO)?;
    let saved_stderr = dup_fd(libc::STDERR_FILENO)?;

    let (fd1_read, fd1_write) = make_pipe()?;
    let (fd2_read, fd2_write) = make_pipe()?;

    dup2_fd(fd1_write.as_raw_fd(), libc::STDOUT_FILENO)?;
    dup2_fd(fd2_write.as_raw_fd(), libc::STDERR_FILENO)?;

    // After dup2, fds 1/2 hold the only reference to the pipe write
    // ends. Drop the explicit OwnedFds so restoring fds 1/2 on Drop
    // closes the last ref and the readers get EOF.
    drop(fd1_write);
    drop(fd2_write);

    let pending = StdioInterceptorPending {
        saved_stdout,
        saved_stderr,
        fd1_read,
        fd2_read,
    };
    let files = StdioInterceptorFiles {
        real_stdout: owned_fd_to_file(mirror_stdout),
        real_stderr: owned_fd_to_file(mirror_stderr),
    };
    Ok((pending, files))
}

impl StdioInterceptorPending {
    /// Start reader threads for the pipes. `dispatch` is cloned into
    /// each thread and installed as its thread-local subscriber so
    /// `tracing::warn!` events route through the owning logging
    /// pathway (works for both global `init` and thread-local
    /// `init_for_tests`).
    pub(crate) fn start_readers(self, dispatch: Dispatch) -> StdioInterceptor {
        let fd1_reader = spawn_reader(self.fd1_read, "fd1", dispatch.clone());
        let fd2_reader = spawn_reader(self.fd2_read, "fd2", dispatch);
        StdioInterceptor {
            saved_stdout: Some(self.saved_stdout),
            saved_stderr: Some(self.saved_stderr),
            fd1_reader: Some(fd1_reader),
            fd2_reader: Some(fd2_reader),
        }
    }
}

impl Drop for StdioInterceptor {
    fn drop(&mut self) {
        // Restore fds 1/2 from saved dups. The dup2 overwrites fds
        // 1/2's prior (pipe write end) reference, dropping it; since
        // we explicitly closed the original write end OwnedFd on
        // install, this is the last reference and the reader thread
        // gets EOF.
        if let Some(fd) = self.saved_stdout.take() {
            let _ = dup2_fd(fd.as_raw_fd(), libc::STDOUT_FILENO);
        }
        if let Some(fd) = self.saved_stderr.take() {
            let _ = dup2_fd(fd.as_raw_fd(), libc::STDERR_FILENO);
        }
        let readers = [self.fd1_reader.take(), self.fd2_reader.take()];
        join_intercept_readers_within(readers.into_iter().flatten(), INTERCEPT_READER_JOIN_BUDGET);
    }
}

/// Join every reader that sees end of file within `budget`, and leave the rest
/// running detached. Returns how many were left.
fn join_intercept_readers_within(
    readers: impl IntoIterator<Item = JoinHandle<()>>,
    budget: Duration,
) -> usize {
    let deadline = Instant::now() + budget;
    let mut still_reading: Vec<JoinHandle<()>> = readers.into_iter().collect();
    loop {
        for returned in still_reading.extract_if(.., |reader| reader.is_finished()) {
            let _ = returned.join();
        }
        if still_reading.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(INTERCEPT_READER_JOIN_POLL_INTERVAL);
    }
    if !still_reading.is_empty() {
        tracing::warn!(
            "{} stdio intercept reader(s) left running: a process this app started still \
             holds the app's standard output or error",
            still_reading.len()
        );
    }
    still_reading.len()
}

fn spawn_reader(pipe_read: OwnedFd, channel: &'static str, dispatch: Dispatch) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name(format!("streamlib-logging-intercept-{channel}"))
        .spawn(move || {
            let _scope = tracing::dispatcher::set_default(&dispatch);
            let file = owned_fd_to_file(pipe_read);
            let mut reader = BufReader::new(file);
            let mut buf: Vec<u8> = Vec::with_capacity(256);
            loop {
                buf.clear();
                match reader.read_until(b'\n', &mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                        }
                        if buf.is_empty() {
                            continue;
                        }
                        let message = String::from_utf8_lossy(&buf);
                        tracing::warn!(
                            intercepted = true,
                            channel = channel,
                            source = "rust",
                            "{}",
                            message,
                        );
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("spawn stdio interceptor reader thread")
}

/// A close-on-exec copy of a standard stream. A copy anything spawned could
/// inherit is the app's own output held open by a grandchild after the app has
/// died.
fn dup_fd(fd: libc::c_int) -> std::io::Result<OwnedFd> {
    // SAFETY: fds 1 and 2 are open for the life of the process.
    unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()
}

fn dup2_fd(src: libc::c_int, dst: libc::c_int) -> std::io::Result<()> {
    let rc = unsafe { libc::dup2(src, dst) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// A close-on-exec pipe. The write end reaches fds 1 and 2 through `dup2`,
/// which is what clears the flag there and only there.
fn make_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let (read_end, write_end) = std::io::pipe()?;
    Ok((OwnedFd::from(read_end), OwnedFd::from(write_end)))
}

fn owned_fd_to_file(fd: OwnedFd) -> File {
    unsafe { File::from_raw_fd(fd.into_raw_fd()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_close_on_exec(descriptor: &OwnedFd) -> bool {
        // SAFETY: `F_GETFD` reads the flags of a descriptor this test owns.
        let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) };
        flags >= 0 && flags & libc::FD_CLOEXEC != 0
    }

    /// Fail-without-fix: a plain `dup` hands every process the app spawns a copy
    /// of the app's real stdout, and a grandchild holding it keeps anything
    /// reading that output waiting after the app has died.
    #[test]
    fn a_copy_of_the_apps_own_output_is_close_on_exec() {
        let stdout_copy = dup_fd(libc::STDOUT_FILENO).expect("fd 1 duplicates");
        let stderr_copy = dup_fd(libc::STDERR_FILENO).expect("fd 2 duplicates");
        assert!(
            is_close_on_exec(&stdout_copy),
            "a spawned process would inherit the copy of stdout"
        );
        assert!(
            is_close_on_exec(&stderr_copy),
            "a spawned process would inherit the copy of stderr"
        );
    }

    /// Fail-without-fix: joining without a bound waits for as long as the write
    /// end stays open, which is as long as whatever the app spawned lives.
    #[test]
    fn a_reader_whose_pipe_is_still_held_open_is_left_running_at_its_budget() {
        let (read_end, write_end_a_spawned_process_holds) = make_pipe().expect("a pipe opens");
        let reader = spawn_reader(read_end, "fd1", Dispatch::none());

        let started = Instant::now();
        let left_running = join_intercept_readers_within([reader], Duration::from_millis(200));

        assert_eq!(left_running, 1);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the join waited {:?} on a pipe that stays open",
            started.elapsed()
        );
        drop(write_end_a_spawned_process_holds);
    }

    #[test]
    fn a_reader_that_sees_end_of_file_is_joined() {
        let (read_end, write_end) = make_pipe().expect("a pipe opens");
        let reader = spawn_reader(read_end, "fd2", Dispatch::none());
        drop(write_end);

        assert_eq!(
            join_intercept_readers_within([reader], Duration::from_secs(5)),
            0
        );
    }

    #[test]
    fn both_ends_of_an_intercept_pipe_are_close_on_exec() {
        let (read_end, write_end) = make_pipe().expect("a pipe opens");
        assert!(
            is_close_on_exec(&read_end),
            "a spawned process would inherit the read end"
        );
        assert!(
            is_close_on_exec(&write_end),
            "a spawned process would inherit the write end"
        );
    }
}
