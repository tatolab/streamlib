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
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::thread::JoinHandle;

use tracing::Dispatch;

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
pub(crate) fn install_redirects(
) -> std::io::Result<(StdioInterceptorPending, StdioInterceptorFiles)> {
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
        if let Some(j) = self.fd1_reader.take() {
            let _ = j.join();
        }
        if let Some(j) = self.fd2_reader.take() {
            let _ = j.join();
        }
    }
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

fn dup_fd(fd: libc::c_int) -> std::io::Result<OwnedFd> {
    let dup = unsafe { libc::dup(fd) };
    if dup < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(dup) })
}

fn dup2_fd(src: libc::c_int, dst: libc::c_int) -> std::io::Result<()> {
    let rc = unsafe { libc::dup2(src, dst) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn make_pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds: [libc::c_int; 2] = [-1, -1];
    let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((
        unsafe { OwnedFd::from_raw_fd(fds[0]) },
        unsafe { OwnedFd::from_raw_fd(fds[1]) },
    ))
}

fn owned_fd_to_file(fd: OwnedFd) -> File {
    unsafe { File::from_raw_fd(fd.into_raw_fd()) }
}
