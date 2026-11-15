// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The lock that makes one runtime the machine's runtime, held for that
//! process's life, and the probe a client uses to name whoever holds it.
//!
//! On Linux it is the abstract unix socket `@tatolab-runtime`: the name lives in
//! the network namespace, so a container sharing the host's network is this
//! machine to it, and the kernel frees it when the holder dies. A probe's
//! connection stays in the listen queue until it is accepted, closed or not, so
//! the holder accepts and drops every one; a full queue would leave the holder
//! unnameable. On macOS it is the loopback HTTP listener's own socket,
//! `127.0.0.1:4446`, which any process may bind first and no other may bind
//! beside it. macOS names no other user's port holder, so the holder answers
//! its pid on the listener and the prober reads who runs that pid from libproc.

#[cfg(target_os = "linux")]
mod linux_abstract_socket_lock;
#[cfg(any(target_os = "macos", test))]
mod loopback_http_listener_pid_probe;
#[cfg(target_os = "macos")]
mod macos_loopback_port_lock;
#[cfg(test)]
mod tests;

use std::fmt;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use linux_abstract_socket_lock::{
    MachineRuntimeLockLocation, holder_of_the_machine_runtime_lock_at,
    take_the_machine_runtime_lock_at,
};
#[cfg(target_os = "macos")]
use macos_loopback_port_lock::{
    MachineRuntimeLockLocation, holder_of_the_machine_runtime_lock_at,
    take_the_machine_runtime_lock_at,
};

/// The abstract unix socket name the Linux machine runtime lock binds, without its leading NUL.
pub const MACHINE_RUNTIME_LOCK_ABSTRACT_SOCKET_NAME: &str = "tatolab-runtime";

/// The process holding the machine runtime lock: on Linux as the kernel names it, on macOS the
/// pid its listener answers, with the user and executable libproc reads for that pid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRuntimeLockHolder {
    /// The holder's user name, when its uid has a passwd entry.
    pub user_name: Option<String>,
    /// The holder's uid.
    pub uid: u32,
    /// The holder's pid; 0 when it runs in a pid namespace this process cannot see.
    pub pid: i32,
    /// The holder's executable, when this process may read it.
    pub executable: Option<PathBuf>,
}

impl fmt::Display for MachineRuntimeLockHolder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.user_name {
            Some(user_name) => write!(formatter, "user {user_name} (uid {})", self.uid)?,
            None => write!(formatter, "uid {}", self.uid)?,
        }
        write!(formatter, ", pid {}", self.pid)?;
        match &self.executable {
            Some(executable) => write!(formatter, ", {}", executable.display()),
            None => write!(formatter, ", executable not readable"),
        }
    }
}

/// Why the machine runtime lock was not taken, or its holder not named.
#[derive(Debug, thiserror::Error)]
pub enum MachineRuntimeLockRefusal {
    /// Another process holds the lock, and is named.
    #[error("another runtime holds this machine: {holder}")]
    HeldByAnotherRuntime { holder: MachineRuntimeLockHolder },
    /// Something holds the lock's name but answers nothing that names it.
    #[error("another runtime holds this machine: {lock} is held by a process that does not answer")]
    HeldByAProcessThatDoesNotAnswer { lock: String },
    /// A process holds the lock, and who it runs as could not be read.
    #[error(
        "another runtime holds this machine: pid {pid} holds {lock}, and its uid could not be read: {source}"
    )]
    HeldByAProcessThatCannotBeNamed {
        lock: String,
        pid: i32,
        source: std::io::Error,
    },
    /// The operating system refused an operation the lock needs.
    #[error("the machine runtime lock {lock} could not be taken: {source}")]
    CouldNotBeTaken {
        lock: String,
        source: std::io::Error,
    },
    /// This test build's machine root, or on macOS its loopback HTTP port, was refused.
    #[cfg(feature = "machine-directories-under-a-test-root")]
    #[error(transparent)]
    TestMachineRoot(#[from] crate::machine_directories_test_root::TestMachineRootRefusal),
}

/// The machine runtime lock, held until this value drops or the process exits.
#[derive(Debug)]
pub struct MachineRuntimeLock {
    #[cfg(target_os = "linux")]
    _listening_abstract_socket:
        linux_abstract_socket_lock::ListeningAbstractSocketWithItsQueueDrained,
    #[cfg(target_os = "macos")]
    bound_loopback_http_listener: macos_loopback_port_lock::LoopbackHttpListenerBoundAsTheLock,
}

impl MachineRuntimeLock {
    /// Take the machine runtime lock, refusing with its holder named when another process has it.
    pub fn take() -> Result<Self, MachineRuntimeLockRefusal> {
        let location = MachineRuntimeLockLocation::for_this_build()?;
        take_the_machine_runtime_lock_at(&location)
    }

    /// The bound loopback HTTP listener for the runtime to serve on, the first time it is asked
    /// for. The lock keeps a descriptor of its own, so the port stays bound until the lock drops.
    #[cfg(target_os = "macos")]
    pub fn take_the_bound_loopback_http_listener(&mut self) -> Option<std::net::TcpListener> {
        self.bound_loopback_http_listener
            .take_the_listener_handed_over_for_serving()
    }
}

/// Name whoever holds the machine runtime lock, never taking it; `None` when nothing holds it.
pub fn holder_of_the_machine_runtime_lock()
-> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal> {
    let location = MachineRuntimeLockLocation::for_this_build()?;
    holder_of_the_machine_runtime_lock_at(&location)
}

/// The passwd user name of `uid`, when it has one.
fn user_name_of_uid(uid: u32) -> Option<String> {
    const LARGEST_PASSWD_BUFFER_TRIED: usize = 1 << 20;
    let mut buffer: Vec<libc::c_char> = vec![0; 4096];
    loop {
        // SAFETY: `passwd` is plain data the call fills; all-zero is a valid value.
        let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut entry: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: every pointer is to a live local of the size passed, and the
        // strings the call writes into `passwd` point into `buffer`, which
        // outlives every read of them below.
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                &mut passwd,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut entry,
            )
        };
        if status == libc::ERANGE && buffer.len() < LARGEST_PASSWD_BUFFER_TRIED {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || entry.is_null() || passwd.pw_name.is_null() {
            return None;
        }
        // SAFETY: on success `pw_name` is a NUL-terminated string inside `buffer`.
        let user_name = unsafe { std::ffi::CStr::from_ptr(passwd.pw_name) };
        return user_name.to_str().ok().map(str::to_string);
    }
}
