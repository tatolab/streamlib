// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Asking whatever listens on a loopback port which pid serves it, as the
//! loopback HTTP listener answers on its holder route, within a bound so a
//! holder that never answers never hangs the caller.

use std::io::{Read, Write};
use std::net::{SocketAddr, SocketAddrV4, TcpStream};
use std::time::{Duration, Instant};

use crate::local_api_wire_contract::{
    MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH, MachineRuntimeLockHolderPidAnswer,
};

/// How long a probe waits, in all, to connect and read the answer.
pub(super) const LOOPBACK_HTTP_LISTENER_PID_PROBE_BOUND: Duration = Duration::from_secs(2);

/// The longest answer a probe reads before it judges the listener not to answer.
const LARGEST_PID_ANSWER_READ_BYTES: usize = 64 * 1024;

/// What asking the port for its holder's pid found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LoopbackHttpListenerPidAnswer {
    /// Nothing listens on the port.
    NothingListens,
    /// The listener answered this pid.
    Answered(i32),
    /// Something accepted the connection and gave no well-formed answer within the bound.
    DoesNotAnswer,
}

pub(super) fn ask_the_loopback_http_listener_for_its_pid(
    address: SocketAddrV4,
    probe_bound: Duration,
) -> std::io::Result<LoopbackHttpListenerPidAnswer> {
    let deadline = Instant::now() + probe_bound;
    let mut connection = match TcpStream::connect_timeout(&SocketAddr::V4(address), probe_bound) {
        Ok(connection) => connection,
        Err(failure) if failure.kind() == std::io::ErrorKind::ConnectionRefused => {
            return Ok(LoopbackHttpListenerPidAnswer::NothingListens);
        }
        Err(failure) if failure.kind() == std::io::ErrorKind::TimedOut => {
            return Ok(LoopbackHttpListenerPidAnswer::DoesNotAnswer);
        }
        Err(failure) => return Err(failure),
    };
    Ok(
        the_pid_answered_on_before(&mut connection, deadline).map_or(
            LoopbackHttpListenerPidAnswer::DoesNotAnswer,
            LoopbackHttpListenerPidAnswer::Answered,
        ),
    )
}

/// The pid a `200` answering a [`MachineRuntimeLockHolderPidAnswer`] names; `None` for any
/// other exchange, one still unfinished at `deadline` included.
fn the_pid_answered_on_before(connection: &mut TcpStream, deadline: Instant) -> Option<i32> {
    let time_left = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|time_left| !time_left.is_zero())
    };
    connection.set_write_timeout(Some(time_left()?)).ok()?;
    connection
        .write_all(
            format!(
                "GET {MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH} HTTP/1.1\r\nHost: 127.0.0.1\r\n\
                 Connection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .ok()?;
    let mut answer = Vec::new();
    let mut read_buffer = [0u8; 4096];
    loop {
        connection.set_read_timeout(Some(time_left()?)).ok()?;
        match connection.read(&mut read_buffer) {
            Ok(0) => break,
            Ok(read) => {
                answer.extend_from_slice(&read_buffer[..read]);
                if answer.len() > LARGEST_PID_ANSWER_READ_BYTES {
                    return None;
                }
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    let (head, body) = std::str::from_utf8(&answer).ok()?.split_once("\r\n\r\n")?;
    let mut status_line = head.lines().next()?.split_whitespace();
    if !status_line.next()?.starts_with("HTTP/1.") || status_line.next()? != "200" {
        return None;
    }
    let MachineRuntimeLockHolderPidAnswer { pid } = serde_json::from_str(body).ok()?;
    (pid > 0).then_some(pid)
}
