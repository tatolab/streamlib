// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener};

use super::{
    MACHINE_RUNTIME_LOCK_ABSTRACT_SOCKET_NAME, MachineRuntimeLock, MachineRuntimeLockHolder,
    MachineRuntimeLockRefusal, user_name_of_uid,
};

/// How long the queue-draining thread waits before accepting again after
/// the process ran out of descriptors.
const QUEUE_DRAIN_BACKOFF_AFTER_RUNNING_OUT_OF_DESCRIPTORS_MILLISECONDS: libc::c_int = 100;

/// The lock's listening socket, with a thread that accepts and drops every
/// connection a probe or a refused take leaves in its queue.
#[derive(Debug)]
pub(super) struct ListeningAbstractSocketWithItsQueueDrained {
    _listening_abstract_socket: UnixListener,
    stop_draining_eventfd: OwnedFd,
    queue_draining_thread: Option<std::thread::JoinHandle<()>>,
}

impl ListeningAbstractSocketWithItsQueueDrained {
    fn start_draining(listening_abstract_socket: UnixListener) -> std::io::Result<Self> {
        listening_abstract_socket.set_nonblocking(true)?;
        // SAFETY: plain eventfd creation; the descriptor is owned below.
        let eventfd_descriptor = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        if eventfd_descriptor < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `eventfd_descriptor` is a fresh eventfd nothing else owns.
        let stop_draining_eventfd = unsafe { OwnedFd::from_raw_fd(eventfd_descriptor) };
        // The thread owns its own descriptors, so the socket stays bound until it exits.
        let listening_abstract_socket_for_the_drain = listening_abstract_socket.try_clone()?;
        let stop_draining_eventfd_for_the_drain = stop_draining_eventfd.try_clone()?;
        let queue_draining_thread = std::thread::Builder::new()
            .name("tatolab-machine-runtime-lock-queue-drain".to_string())
            .spawn(move || {
                drain_the_listen_queue_until_stopped(
                    &listening_abstract_socket_for_the_drain,
                    &stop_draining_eventfd_for_the_drain,
                )
            })?;
        Ok(Self {
            _listening_abstract_socket: listening_abstract_socket,
            stop_draining_eventfd,
            queue_draining_thread: Some(queue_draining_thread),
        })
    }

    /// The listening socket's descriptor, for a test that shrinks its backlog.
    #[cfg(test)]
    pub(super) fn listening_abstract_socket_descriptor(&self) -> std::os::fd::RawFd {
        self._listening_abstract_socket.as_raw_fd()
    }
}

impl Drop for ListeningAbstractSocketWithItsQueueDrained {
    fn drop(&mut self) {
        let one: u64 = 1;
        // SAFETY: the eventfd is open, and the buffer is a live 8-byte value.
        let written = unsafe {
            libc::write(
                self.stop_draining_eventfd.as_raw_fd(),
                (&one as *const u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            )
        };
        if written < 0 {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "the machine runtime lock's queue-draining thread could not be told to stop; \
                 the lock stays held until this process exits"
            );
            return;
        }
        if let Some(queue_draining_thread) = self.queue_draining_thread.take()
            && queue_draining_thread.join().is_err()
        {
            tracing::warn!("the machine runtime lock's queue-draining thread panicked");
        }
    }
}

/// Accept and drop every queued connection until the stop eventfd fires.
fn drain_the_listen_queue_until_stopped(
    listening_abstract_socket: &UnixListener,
    stop_draining_eventfd: &OwnedFd,
) {
    let listening_descriptor = listening_abstract_socket.as_raw_fd();
    let stop_draining_descriptor = stop_draining_eventfd.as_raw_fd();
    loop {
        let mut watched = [
            libc::pollfd {
                fd: listening_descriptor,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stop_draining_descriptor,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `watched` is a live array of exactly the length passed.
        let ready = unsafe { libc::poll(watched.as_mut_ptr(), watched.len() as libc::nfds_t, -1) };
        if ready < 0 {
            let failure = std::io::Error::last_os_error();
            if failure.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            tracing::warn!(error = %failure, "the machine runtime lock stopped draining its listen queue");
            return;
        }
        if watched[1].revents != 0 {
            return;
        }
        if watched[0].revents & libc::POLLIN == 0 {
            if watched[0].revents != 0 {
                tracing::warn!(
                    revents = watched[0].revents,
                    "the machine runtime lock's listening socket reported an error; it stopped \
                     draining its listen queue"
                );
                return;
            }
            continue;
        }
        // SAFETY: the listening descriptor is open; null address pointers ask for no peer address.
        let accepted = unsafe {
            libc::accept4(
                listening_descriptor,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            )
        };
        if accepted >= 0 {
            // SAFETY: `accepted` is a fresh descriptor nothing else owns; dropping it closes it.
            drop(unsafe { OwnedFd::from_raw_fd(accepted) });
            continue;
        }
        let failure = std::io::Error::last_os_error();
        match failure.raw_os_error() {
            Some(libc::EAGAIN | libc::EINTR | libc::ECONNABORTED) => {}
            Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM) => {
                tracing::warn!(
                    error = %failure,
                    "the machine runtime lock could not accept a queued connection; retrying"
                );
                let mut stop_watched = [watched[1]];
                // SAFETY: `stop_watched` is a live array of exactly the length passed.
                let stopped = unsafe {
                    libc::poll(
                        stop_watched.as_mut_ptr(),
                        1,
                        QUEUE_DRAIN_BACKOFF_AFTER_RUNNING_OUT_OF_DESCRIPTORS_MILLISECONDS,
                    )
                };
                if stopped > 0 {
                    return;
                }
            }
            _ => {
                tracing::warn!(
                    error = %failure,
                    "the machine runtime lock stopped draining its listen queue"
                );
                return;
            }
        }
    }
}

/// What connecting to the abstract name found.
pub(super) enum AbstractSocketAnswer {
    /// A listener accepted the connection into its queue; the kernel names who listened.
    Answered(libc::ucred),
    /// Nothing listens: the name is free, or bound by a socket that never listened.
    NothingListens,
    /// A listener's queue is full, so it cannot be asked who it is.
    ListenerQueueIsFull,
}

/// How a refusal names the lock: the abstract socket with its leading `@`.
pub(super) fn displayed_lock_name(abstract_socket_name: &str) -> String {
    format!("the abstract socket @{abstract_socket_name}")
}

fn could_not_be_taken(
    abstract_socket_name: &str,
    source: std::io::Error,
) -> MachineRuntimeLockRefusal {
    MachineRuntimeLockRefusal::CouldNotBeTaken {
        lock: displayed_lock_name(abstract_socket_name),
        source,
    }
}

pub(super) fn take(
    abstract_socket_name: &str,
) -> Result<ListeningAbstractSocketWithItsQueueDrained, MachineRuntimeLockRefusal> {
    let address = SocketAddr::from_abstract_name(abstract_socket_name.as_bytes())
        .map_err(|source| could_not_be_taken(abstract_socket_name, source))?;
    // A second attempt covers a holder that exited between the refused bind and the connect.
    for attempt in 0..2 {
        match UnixListener::bind_addr(&address) {
            Ok(listening_abstract_socket) => {
                return ListeningAbstractSocketWithItsQueueDrained::start_draining(
                    listening_abstract_socket,
                )
                .map_err(|source| could_not_be_taken(abstract_socket_name, source));
            }
            Err(failure) if failure.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(failure) => return Err(could_not_be_taken(abstract_socket_name, failure)),
        }
        match ask_whoever_listens_on(abstract_socket_name)
            .map_err(|source| could_not_be_taken(abstract_socket_name, source))?
        {
            AbstractSocketAnswer::Answered(peer_credentials) => {
                return Err(MachineRuntimeLockRefusal::HeldByAnotherRuntime {
                    holder: holder_from_peer_credentials(peer_credentials),
                });
            }
            AbstractSocketAnswer::NothingListens if attempt == 0 => continue,
            AbstractSocketAnswer::NothingListens | AbstractSocketAnswer::ListenerQueueIsFull => {
                break;
            }
        }
    }
    Err(MachineRuntimeLockRefusal::HeldByAProcessThatDoesNotAnswer {
        lock: displayed_lock_name(abstract_socket_name),
    })
}

pub(super) fn holder(
    abstract_socket_name: &str,
) -> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal> {
    match ask_whoever_listens_on(abstract_socket_name)
        .map_err(|source| could_not_be_taken(abstract_socket_name, source))?
    {
        AbstractSocketAnswer::Answered(peer_credentials) => {
            Ok(Some(holder_from_peer_credentials(peer_credentials)))
        }
        AbstractSocketAnswer::NothingListens => Ok(None),
        AbstractSocketAnswer::ListenerQueueIsFull => {
            Err(MachineRuntimeLockRefusal::HeldByAProcessThatDoesNotAnswer {
                lock: displayed_lock_name(abstract_socket_name),
            })
        }
    }
}

fn holder_from_peer_credentials(peer_credentials: libc::ucred) -> MachineRuntimeLockHolder {
    MachineRuntimeLockHolder {
        user_name: user_name_of_uid(peer_credentials.uid),
        uid: peer_credentials.uid,
        pid: peer_credentials.pid,
        executable: (peer_credentials.pid > 0)
            .then(|| std::fs::read_link(format!("/proc/{}/exe", peer_credentials.pid)).ok())
            .flatten(),
    }
}

/// The `sockaddr_un` of an abstract name: a leading NUL, then the name, with
/// the length covering exactly those bytes.
pub(super) fn abstract_socket_address(
    abstract_socket_name: &str,
) -> std::io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    // SAFETY: `sockaddr_un` is plain data; all-zero is a valid value.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let name = abstract_socket_name.as_bytes();
    if name.len() + 1 > address.sun_path.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "the abstract socket name is {} bytes, longer than a unix socket address holds",
                name.len()
            ),
        ));
    }
    for (slot, byte) in address.sun_path[1..].iter_mut().zip(name) {
        *slot = *byte as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + 1 + name.len();
    Ok((address, length as libc::socklen_t))
}

/// Connect without blocking, so a holder whose queue is full never hangs the
/// caller, and read the listener's credentials from the kernel.
pub(super) fn ask_whoever_listens_on(
    abstract_socket_name: &str,
) -> std::io::Result<AbstractSocketAnswer> {
    let (address, address_length) = abstract_socket_address(abstract_socket_name)?;
    // SAFETY: plain socket creation; the descriptor is owned below.
    let descriptor = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `descriptor` is a fresh socket nothing else owns.
    let connection = unsafe { OwnedFd::from_raw_fd(descriptor) };
    // SAFETY: `address` is a live `sockaddr_un` and `address_length` covers its used bytes.
    let connected = unsafe {
        libc::connect(
            connection.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
            address_length,
        )
    };
    if connected != 0 {
        let failure = std::io::Error::last_os_error();
        return match failure.raw_os_error() {
            Some(libc::ECONNREFUSED) => Ok(AbstractSocketAnswer::NothingListens),
            Some(libc::EAGAIN) => Ok(AbstractSocketAnswer::ListenerQueueIsFull),
            _ => Err(failure),
        };
    }
    // SAFETY: `ucred` is plain data; all-zero is a valid value.
    let mut peer_credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut peer_credentials_length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the out-pointers are to live locals of the size passed.
    let read = unsafe {
        libc::getsockopt(
            connection.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer_credentials as *mut libc::ucred).cast::<libc::c_void>(),
            &mut peer_credentials_length,
        )
    };
    if read != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(AbstractSocketAnswer::Answered(peer_credentials))
}

/// Where this build's machine runtime lock lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MachineRuntimeLockLocation {
    pub(super) abstract_socket_name: String,
}

impl MachineRuntimeLockLocation {
    #[cfg(not(feature = "machine-directories-under-a-test-root"))]
    pub(super) fn for_this_build() -> Result<Self, MachineRuntimeLockRefusal> {
        Ok(Self {
            abstract_socket_name: MACHINE_RUNTIME_LOCK_ABSTRACT_SOCKET_NAME.to_string(),
        })
    }

    #[cfg(feature = "machine-directories-under-a-test-root")]
    pub(super) fn for_this_build() -> Result<Self, MachineRuntimeLockRefusal> {
        let test_machine_root =
            crate::machine_directories_test_root::TestMachineRoot::from_the_environment()?;
        Ok(Self {
            abstract_socket_name: test_machine_root.machine_runtime_lock_abstract_socket_name(
                MACHINE_RUNTIME_LOCK_ABSTRACT_SOCKET_NAME,
            ),
        })
    }
}

pub(super) fn take_the_machine_runtime_lock_at(
    location: &MachineRuntimeLockLocation,
) -> Result<MachineRuntimeLock, MachineRuntimeLockRefusal> {
    Ok(MachineRuntimeLock {
        _listening_abstract_socket: take(&location.abstract_socket_name)?,
    })
}

pub(super) fn holder_of_the_machine_runtime_lock_at(
    location: &MachineRuntimeLockLocation,
) -> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal> {
    holder(&location.abstract_socket_name)
}
