// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::net::{SocketAddrV4, TcpListener};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use super::loopback_http_listener_pid_probe::{
    LOOPBACK_HTTP_LISTENER_PID_PROBE_BOUND, LoopbackHttpListenerPidAnswer,
    ask_the_loopback_http_listener_for_its_pid,
};
use super::{
    MachineRuntimeLock, MachineRuntimeLockHolder, MachineRuntimeLockRefusal, user_name_of_uid,
};

/// The lock's bound listener, kept until the lock drops, and a second descriptor of it for
/// the runtime to serve the loopback HTTP listener on.
#[derive(Debug)]
pub(super) struct LoopbackHttpListenerBoundAsTheLock {
    _listener_kept_by_the_lock: TcpListener,
    listener_handed_over_for_serving: Option<TcpListener>,
}

impl LoopbackHttpListenerBoundAsTheLock {
    pub(super) fn take_the_listener_handed_over_for_serving(&mut self) -> Option<TcpListener> {
        self.listener_handed_over_for_serving.take()
    }
}

/// How a refusal names the lock: the loopback port.
fn displayed_lock_name(address: SocketAddrV4) -> String {
    format!("the loopback port {address}")
}

fn could_not_be_taken(address: SocketAddrV4, source: std::io::Error) -> MachineRuntimeLockRefusal {
    MachineRuntimeLockRefusal::CouldNotBeTaken {
        lock: displayed_lock_name(address),
        source,
    }
}

pub(super) fn take(
    address: SocketAddrV4,
) -> Result<LoopbackHttpListenerBoundAsTheLock, MachineRuntimeLockRefusal> {
    // A second attempt covers a holder that exited between the refused bind and the probe.
    for attempt in 0..2 {
        match TcpListener::bind(address) {
            Ok(listener_kept_by_the_lock) => {
                let listener_handed_over_for_serving = listener_kept_by_the_lock
                    .try_clone()
                    .map_err(|source| could_not_be_taken(address, source))?;
                return Ok(LoopbackHttpListenerBoundAsTheLock {
                    _listener_kept_by_the_lock: listener_kept_by_the_lock,
                    listener_handed_over_for_serving: Some(listener_handed_over_for_serving),
                });
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(failure) => return Err(could_not_be_taken(address, failure)),
        }
        match holder(address)? {
            Some(holder) => {
                return Err(MachineRuntimeLockRefusal::HeldByAnotherRuntime { holder });
            }
            None if attempt == 0 => continue,
            None => break,
        }
    }
    Err(MachineRuntimeLockRefusal::HeldByAProcessThatDoesNotAnswer {
        lock: displayed_lock_name(address),
    })
}

pub(super) fn holder(
    address: SocketAddrV4,
) -> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal> {
    match ask_the_loopback_http_listener_for_its_pid(
        address,
        LOOPBACK_HTTP_LISTENER_PID_PROBE_BOUND,
    )
    .map_err(|source| could_not_be_taken(address, source))?
    {
        LoopbackHttpListenerPidAnswer::NothingListens => Ok(None),
        LoopbackHttpListenerPidAnswer::Answered(pid) => holder_named_by_pid(address, pid).map(Some),
        LoopbackHttpListenerPidAnswer::DoesNotAnswer => {
            Err(MachineRuntimeLockRefusal::HeldByAProcessThatDoesNotAnswer {
                lock: displayed_lock_name(address),
            })
        }
    }
}

/// Name a pid through libproc (`<libproc.h>`): its uid from its short BSD
/// info and its executable from its path, the two flavors xnu answers for
/// another user's process; the full BSD info is refused across users.
fn holder_named_by_pid(
    address: SocketAddrV4,
    pid: i32,
) -> Result<MachineRuntimeLockHolder, MachineRuntimeLockRefusal> {
    // SAFETY: `proc_bsdshortinfo` is plain data; all-zero is a valid value.
    let mut short_bsd_info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let short_bsd_info_size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
    // SAFETY: the buffer is a live `proc_bsdshortinfo` of exactly the size passed.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut short_bsd_info as *mut libc::proc_bsdshortinfo).cast::<libc::c_void>(),
            short_bsd_info_size,
        )
    };
    if written != short_bsd_info_size {
        let source = if written <= 0 {
            std::io::Error::last_os_error()
        } else {
            std::io::Error::other(format!(
                "libproc wrote {written} of the {short_bsd_info_size} bytes of its short BSD \
                 info"
            ))
        };
        return Err(MachineRuntimeLockRefusal::HeldByAProcessThatCannotBeNamed {
            lock: displayed_lock_name(address),
            pid,
            source,
        });
    }
    let mut executable_path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is live and exactly the length passed.
    let executable_path_length = unsafe {
        libc::proc_pidpath(
            pid,
            executable_path.as_mut_ptr().cast::<libc::c_void>(),
            executable_path.len() as u32,
        )
    };
    let executable = (executable_path_length > 0).then(|| {
        PathBuf::from(std::ffi::OsStr::from_bytes(
            &executable_path[..executable_path_length as usize],
        ))
    });
    Ok(MachineRuntimeLockHolder {
        user_name: user_name_of_uid(short_bsd_info.pbsi_uid),
        uid: short_bsd_info.pbsi_uid,
        pid,
        executable,
    })
}

/// Where this build's machine runtime lock lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MachineRuntimeLockLocation {
    pub(super) loopback_http_listener_address: SocketAddrV4,
}

impl MachineRuntimeLockLocation {
    #[cfg(not(feature = "machine-directories-under-a-test-root"))]
    pub(super) fn for_this_build() -> Result<Self, MachineRuntimeLockRefusal> {
        Ok(Self {
            loopback_http_listener_address: std::net::SocketAddrV4::new(
                std::net::Ipv4Addr::LOCALHOST,
                crate::local_api_wire_contract::LOOPBACK_HTTP_LISTENER_DEFAULT_PORT,
            ),
        })
    }

    /// A test build still needs its machine root, and binds the loopback HTTP port its test
    /// names, never the machine's own.
    #[cfg(feature = "machine-directories-under-a-test-root")]
    pub(super) fn for_this_build() -> Result<Self, MachineRuntimeLockRefusal> {
        crate::machine_directories_test_root::TestMachineRoot::from_the_environment()?;
        Ok(Self {
            loopback_http_listener_address: std::net::SocketAddrV4::new(
                std::net::Ipv4Addr::LOCALHOST,
                crate::machine_directories_test_root::test_loopback_http_port_from_the_environment(
                )?,
            ),
        })
    }
}

pub(super) fn take_the_machine_runtime_lock_at(
    location: &MachineRuntimeLockLocation,
) -> Result<MachineRuntimeLock, MachineRuntimeLockRefusal> {
    Ok(MachineRuntimeLock {
        bound_loopback_http_listener: take(location.loopback_http_listener_address)?,
    })
}

pub(super) fn holder_of_the_machine_runtime_lock_at(
    location: &MachineRuntimeLockLocation,
) -> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal> {
    holder(location.loopback_http_listener_address)
}
