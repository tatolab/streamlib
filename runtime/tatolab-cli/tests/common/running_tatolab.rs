// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A `tatolab` left running in the background — `run` attached, `dev`, a followed `logs` — its
//! stdout and stderr read line by line as they land, signalled as a terminal signals it, and
//! killed if a test leaves it running. Integration tests only.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// How long a wait on a running `tatolab` lasts before failing rather than hanging.
pub const RUNNING_TATOLAB_WAIT_TIMEOUT: Duration = Duration::from_secs(20);

/// How often a wait on a condition looks again.
const CONDITION_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A `tatolab` running in the background.
pub struct RunningTatolab {
    tatolab_process: Child,
    standard_output_lines: Receiver<String>,
    standard_error_lines: Receiver<String>,
    standard_error_lines_read: Vec<String>,
}

impl Drop for RunningTatolab {
    fn drop(&mut self) {
        let _killed_or_already_gone = self.tatolab_process.kill();
        let _reaped = self.tatolab_process.wait();
    }
}

impl RunningTatolab {
    /// Start `tatolab_command` with its stdout and stderr piped.
    pub fn spawn(mut tatolab_command: Command) -> Self {
        let mut tatolab_process = tatolab_command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            standard_output_lines: lines_read_in_the_background(
                tatolab_process.stdout.take().unwrap(),
            ),
            standard_error_lines: lines_read_in_the_background(
                tatolab_process.stderr.take().unwrap(),
            ),
            tatolab_process,
            standard_error_lines_read: Vec::new(),
        }
    }

    /// The next line on stdout, `awaited` naming it when it never comes.
    pub fn next_standard_output_line(&self, awaited: &str) -> String {
        self.standard_output_lines
            .recv_timeout(RUNNING_TATOLAB_WAIT_TIMEOUT)
            .unwrap_or_else(|_| {
                panic!("expected {awaited} on stdout within {RUNNING_TATOLAB_WAIT_TIMEOUT:?}")
            })
    }

    /// Read stderr until a line containing `awaited_text`, answering that line.
    pub fn wait_for_standard_error_line_containing(&mut self, awaited_text: &str) -> String {
        let deadline = Instant::now() + RUNNING_TATOLAB_WAIT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.standard_error_lines.recv_timeout(remaining) {
                Ok(standard_error_line) => {
                    self.standard_error_lines_read
                        .push(standard_error_line.clone());
                    if standard_error_line.contains(awaited_text) {
                        return standard_error_line;
                    }
                }
                Err(_) => panic!(
                    "expected a stderr line containing {awaited_text:?} within \
                     {RUNNING_TATOLAB_WAIT_TIMEOUT:?}; stderr so far: {:?}",
                    self.standard_error_lines_read
                ),
            }
        }
    }

    /// Deliver `signal` to the process, as a terminal does.
    pub fn send_signal(&self, signal: libc::c_int) {
        // SAFETY: `kill` takes a pid and a signal number and touches no memory; the pid is this
        // test's unreaped child.
        let kill_result = unsafe { libc::kill(self.tatolab_process.id() as libc::pid_t, signal) };
        assert_eq!(kill_result, 0);
    }

    /// Wait for the process to exit, answering how, and every stderr line it wrote.
    pub fn wait_for_exit(mut self) -> (ExitStatus, Vec<String>) {
        let deadline = Instant::now() + RUNNING_TATOLAB_WAIT_TIMEOUT;
        let exit_status = loop {
            if let Some(exit_status) = self.tatolab_process.try_wait().unwrap() {
                break exit_status;
            }
            assert!(
                Instant::now() < deadline,
                "tatolab did not exit within {RUNNING_TATOLAB_WAIT_TIMEOUT:?}; stderr so far: \
                 {:?}",
                self.standard_error_lines_read
            );
            std::thread::sleep(CONDITION_POLL_INTERVAL);
        };
        while let Ok(standard_error_line) = self
            .standard_error_lines
            .recv_timeout(Duration::from_secs(2))
        {
            self.standard_error_lines_read.push(standard_error_line);
        }
        (
            exit_status,
            std::mem::take(&mut self.standard_error_lines_read),
        )
    }

    /// Whether the process is still running.
    pub fn is_still_running(&mut self) -> bool {
        self.tatolab_process.try_wait().unwrap().is_none()
    }
}

/// Wait until `condition` holds, `what_is_awaited` naming it when it never does.
pub fn wait_until(what_is_awaited: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + RUNNING_TATOLAB_WAIT_TIMEOUT;
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "{what_is_awaited} did not happen within {RUNNING_TATOLAB_WAIT_TIMEOUT:?}"
        );
        std::thread::sleep(CONDITION_POLL_INTERVAL);
    }
}

fn lines_read_in_the_background(stream: impl Read + Send + 'static) -> Receiver<String> {
    let (line_sender, line_receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            if line_sender.send(line).is_err() {
                return;
            }
        }
    });
    line_receiver
}
