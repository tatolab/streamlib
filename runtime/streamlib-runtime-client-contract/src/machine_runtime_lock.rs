// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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

use std::fmt;
use std::path::PathBuf;

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

/// Where this build's machine runtime lock lives.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MachineRuntimeLockLocation {
    #[cfg(target_os = "linux")]
    abstract_socket_name: String,
    #[cfg(target_os = "macos")]
    loopback_http_listener_address: std::net::SocketAddrV4,
}

impl MachineRuntimeLockLocation {
    #[cfg(not(feature = "machine-directories-under-a-test-root"))]
    fn for_this_build() -> Result<Self, MachineRuntimeLockRefusal> {
        Ok(Self {
            #[cfg(target_os = "linux")]
            abstract_socket_name: MACHINE_RUNTIME_LOCK_ABSTRACT_SOCKET_NAME.to_string(),
            #[cfg(target_os = "macos")]
            loopback_http_listener_address: std::net::SocketAddrV4::new(
                std::net::Ipv4Addr::LOCALHOST,
                crate::local_api_wire_contract::LOOPBACK_HTTP_LISTENER_DEFAULT_PORT,
            ),
        })
    }

    #[cfg(all(feature = "machine-directories-under-a-test-root", target_os = "linux"))]
    fn for_this_build() -> Result<Self, MachineRuntimeLockRefusal> {
        let test_machine_root =
            crate::machine_directories_test_root::TestMachineRoot::from_the_environment()?;
        Ok(Self {
            abstract_socket_name: test_machine_root.machine_runtime_lock_abstract_socket_name(
                MACHINE_RUNTIME_LOCK_ABSTRACT_SOCKET_NAME,
            ),
        })
    }

    /// A test build still needs its machine root, and binds the loopback HTTP port its test
    /// names, never the machine's own.
    #[cfg(all(feature = "machine-directories-under-a-test-root", target_os = "macos"))]
    fn for_this_build() -> Result<Self, MachineRuntimeLockRefusal> {
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

#[cfg(target_os = "linux")]
fn take_the_machine_runtime_lock_at(
    location: &MachineRuntimeLockLocation,
) -> Result<MachineRuntimeLock, MachineRuntimeLockRefusal> {
    Ok(MachineRuntimeLock {
        _listening_abstract_socket: linux_abstract_socket_lock::take(
            &location.abstract_socket_name,
        )?,
    })
}

#[cfg(target_os = "linux")]
fn holder_of_the_machine_runtime_lock_at(
    location: &MachineRuntimeLockLocation,
) -> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal> {
    linux_abstract_socket_lock::holder(&location.abstract_socket_name)
}

#[cfg(target_os = "macos")]
fn take_the_machine_runtime_lock_at(
    location: &MachineRuntimeLockLocation,
) -> Result<MachineRuntimeLock, MachineRuntimeLockRefusal> {
    Ok(MachineRuntimeLock {
        bound_loopback_http_listener: macos_loopback_port_lock::take(
            location.loopback_http_listener_address,
        )?,
    })
}

#[cfg(target_os = "macos")]
fn holder_of_the_machine_runtime_lock_at(
    location: &MachineRuntimeLockLocation,
) -> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal> {
    macos_loopback_port_lock::holder(location.loopback_http_listener_address)
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

#[cfg(target_os = "linux")]
mod linux_abstract_socket_lock {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener};

    use super::{MachineRuntimeLockHolder, MachineRuntimeLockRefusal, user_name_of_uid};

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
            let ready =
                unsafe { libc::poll(watched.as_mut_ptr(), watched.len() as libc::nfds_t, -1) };
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
                AbstractSocketAnswer::NothingListens
                | AbstractSocketAnswer::ListenerQueueIsFull => {
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
}

/// Asking whatever listens on a loopback port which pid serves it, as the
/// loopback HTTP listener answers on its holder route, within a bound so a
/// holder that never answers never hangs the caller.
#[cfg(any(target_os = "macos", test))]
mod loopback_http_listener_pid_probe {
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
        let mut connection = match TcpStream::connect_timeout(&SocketAddr::V4(address), probe_bound)
        {
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
}

#[cfg(target_os = "macos")]
mod macos_loopback_port_lock {
    use std::net::{SocketAddrV4, TcpListener};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    use super::loopback_http_listener_pid_probe::{
        LOOPBACK_HTTP_LISTENER_PID_PROBE_BOUND, LoopbackHttpListenerPidAnswer,
        ask_the_loopback_http_listener_for_its_pid,
    };
    use super::{MachineRuntimeLockHolder, MachineRuntimeLockRefusal, user_name_of_uid};

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

    fn could_not_be_taken(
        address: SocketAddrV4,
        source: std::io::Error,
    ) -> MachineRuntimeLockRefusal {
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
            LoopbackHttpListenerPidAnswer::Answered(pid) => {
                holder_named_by_pid(address, pid).map(Some)
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn the_holder_reads_as_user_uid_pid_and_executable() {
        let holder = MachineRuntimeLockHolder {
            user_name: Some("jonathan".to_string()),
            uid: 1000,
            pid: 4242,
            executable: Some(PathBuf::from("/home/jonathan/.local/bin/tatolabd")),
        };
        assert_eq!(
            holder.to_string(),
            "user jonathan (uid 1000), pid 4242, /home/jonathan/.local/bin/tatolabd"
        );
        let unnamed_holder = MachineRuntimeLockHolder {
            user_name: None,
            executable: None,
            ..holder
        };
        assert_eq!(
            unnamed_holder.to_string(),
            "uid 1000, pid 4242, executable not readable"
        );
    }

    /// A loopback port free when chosen.
    #[cfg(any(target_os = "macos", feature = "machine-directories-under-a-test-root"))]
    fn a_free_loopback_port() -> u16 {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// An HTTP/1.1 answer of `status_line` carrying `body`, closing its connection.
    fn an_http_answer(status_line: &str, body: &str) -> Vec<u8> {
        format!(
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    /// A test thread answering every connection to its listener with the same
    /// bytes, once it has read the request's head, until it drops.
    struct TestThreadAnsweringEveryConnection {
        stop_answering: Arc<AtomicBool>,
        answering_thread: Option<std::thread::JoinHandle<()>>,
    }

    impl TestThreadAnsweringEveryConnection {
        fn start(listener: TcpListener, answer: Vec<u8>) -> Self {
            listener.set_nonblocking(true).unwrap();
            let stop_answering = Arc::new(AtomicBool::new(false));
            let stop_answering_seen_by_the_thread = Arc::clone(&stop_answering);
            let answering_thread = std::thread::spawn(move || {
                while !stop_answering_seen_by_the_thread.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut connection, _peer_address)) => {
                            let _ = connection.set_nonblocking(false);
                            let _ = connection.set_read_timeout(Some(Duration::from_secs(5)));
                            read_the_request_head(&mut connection);
                            let _ = connection.write_all(&answer);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(1)),
                    }
                }
            });
            Self {
                stop_answering,
                answering_thread: Some(answering_thread),
            }
        }

        /// Answering this process's pid, as the loopback HTTP listener answers its holder route.
        #[cfg(target_os = "macos")]
        fn answering_this_processes_pid(listener: TcpListener) -> Self {
            Self::start(
                listener,
                an_http_answer(
                    "HTTP/1.1 200 OK",
                    &format!(r#"{{"pid":{}}}"#, std::process::id()),
                ),
            )
        }
    }

    impl Drop for TestThreadAnsweringEveryConnection {
        fn drop(&mut self) {
            self.stop_answering.store(true, Ordering::SeqCst);
            if let Some(answering_thread) = self.answering_thread.take() {
                let _ = answering_thread.join();
            }
        }
    }

    fn read_the_request_head(connection: &mut TcpStream) {
        let mut request_head = Vec::new();
        let mut read_byte = [0u8; 1];
        while !request_head.ends_with(b"\r\n\r\n") {
            match connection.read(&mut read_byte) {
                Ok(1) => request_head.push(read_byte[0]),
                _ => return,
            }
        }
    }

    /// Each answer the pid probe can read off a listener, against a stub on a free port.
    mod loopback_http_listener_pid_probe_outcomes {
        use super::super::loopback_http_listener_pid_probe::{
            LOOPBACK_HTTP_LISTENER_PID_PROBE_BOUND, LoopbackHttpListenerPidAnswer,
            ask_the_loopback_http_listener_for_its_pid,
        };
        use super::*;

        const A_SHORT_PROBE_BOUND: Duration = Duration::from_millis(300);

        fn a_listener_on_a_free_loopback_port() -> (TcpListener, SocketAddrV4) {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let SocketAddr::V4(address) = listener.local_addr().unwrap() else {
                panic!("a loopback listener has an IPv4 address");
            };
            (listener, address)
        }

        fn what_a_probe_reads_off_a_stub_answering(
            answer: Vec<u8>,
        ) -> LoopbackHttpListenerPidAnswer {
            let (listener, address) = a_listener_on_a_free_loopback_port();
            let _stub = TestThreadAnsweringEveryConnection::start(listener, answer);
            ask_the_loopback_http_listener_for_its_pid(
                address,
                LOOPBACK_HTTP_LISTENER_PID_PROBE_BOUND,
            )
            .unwrap()
        }

        #[test]
        fn a_stub_answering_pid_1_is_answered_as_pid_1() {
            assert_eq!(
                what_a_probe_reads_off_a_stub_answering(an_http_answer(
                    "HTTP/1.1 200 OK",
                    r#"{"pid":1}"#
                )),
                LoopbackHttpListenerPidAnswer::Answered(1)
            );
        }

        /// The kernel accepts the connection into the stub's queue, and nothing reads it.
        #[test]
        fn a_stub_that_accepts_and_never_answers_does_not_answer_within_the_bound() {
            let (_listener, address) = a_listener_on_a_free_loopback_port();
            let probe_started = Instant::now();

            let answer =
                ask_the_loopback_http_listener_for_its_pid(address, A_SHORT_PROBE_BOUND).unwrap();

            assert_eq!(answer, LoopbackHttpListenerPidAnswer::DoesNotAnswer);
            assert!(
                probe_started.elapsed() < A_SHORT_PROBE_BOUND * 10,
                "the probe took {:?}",
                probe_started.elapsed()
            );
        }

        #[test]
        fn a_stub_answering_anything_but_a_pid_does_not_answer() {
            for answer in [
                b"garbage".to_vec(),
                an_http_answer("HTTP/1.1 200 OK", "garbage"),
                an_http_answer("HTTP/1.1 200 OK", r#"{"pid":"one"}"#),
                an_http_answer("HTTP/1.1 200 OK", r#"{"pid":0}"#),
                an_http_answer("HTTP/1.1 404 Not Found", r#"{"pid":1}"#),
                an_http_answer("HTTP/1.1 421 Misdirected Request", r#"{"pid":1}"#),
            ] {
                assert_eq!(
                    what_a_probe_reads_off_a_stub_answering(answer.clone()),
                    LoopbackHttpListenerPidAnswer::DoesNotAnswer,
                    "{}",
                    String::from_utf8_lossy(&answer)
                );
            }
        }

        #[test]
        fn a_port_nothing_listens_on_is_answered_by_nothing() {
            let (listener, address) = a_listener_on_a_free_loopback_port();
            drop(listener);

            assert_eq!(
                ask_the_loopback_http_listener_for_its_pid(address, A_SHORT_PROBE_BOUND).unwrap(),
                LoopbackHttpListenerPidAnswer::NothingListens
            );
        }

        /// Pid 1 runs as root, another user's process to this test: its name
        /// comes from the kernel, never from the stub.
        #[cfg(target_os = "macos")]
        #[test]
        fn a_stub_answering_pid_1_is_named_root_running_launchd() {
            let (listener, address) = a_listener_on_a_free_loopback_port();
            let _stub = TestThreadAnsweringEveryConnection::start(
                listener,
                an_http_answer("HTTP/1.1 200 OK", r#"{"pid":1}"#),
            );

            assert_eq!(
                super::super::macos_loopback_port_lock::holder(address).unwrap(),
                Some(MachineRuntimeLockHolder {
                    user_name: Some("root".to_string()),
                    uid: 0,
                    pid: 1,
                    executable: Some(PathBuf::from("/sbin/launchd")),
                })
            );
        }
    }

    /// The tests that take and probe a real lock at a location no real runtime
    /// and no other test uses. They run one at a time, so no child process one
    /// of them spawns inherits a lock another holds.
    mod taken_and_probed_on_this_floor {
        use super::*;
        use crate::test_support::{
            assert_the_child_process_ran_the_test, rerun_this_test_in_a_child_process,
            spawn_this_test_from_a_test_binary,
        };
        use std::ffi::OsString;

        /// Set only in the child process that holds the lock while its parent is refused.
        const HOLD_THE_LOCK_CHILD_ENVIRONMENT_VARIABLE: &str =
            "STREAMLIB_TEST_MACHINE_RUNTIME_LOCK_TO_HOLD";

        /// The file the holding child creates once it holds the lock.
        const HOLDING_CHILD_READY_FILE_ENVIRONMENT_VARIABLE: &str =
            "STREAMLIB_TEST_MACHINE_RUNTIME_LOCK_HOLDING_CHILD_READY_FILE";

        /// Set only in the child process that probes a lock its parent holds.
        const PROBE_THE_PARENTS_LOCK_CHILD_ENVIRONMENT_VARIABLE: &str =
            "STREAMLIB_TEST_MACHINE_RUNTIME_LOCK_HELD_BY_THE_PARENT";

        const HOLDING_CHILD_TEST_PATH: &str = "machine_runtime_lock::tests::taken_and_probed_on_this_floor::a_take_while_another_executable_holds_the_lock_is_refused_naming_its_uid_pid_and_executable";

        const PROBING_CHILD_TEST_PATH: &str = "machine_runtime_lock::tests::taken_and_probed_on_this_floor::probes_and_refused_takes_by_the_holder_leave_the_lock_held";

        /// A lock location only one test uses.
        struct LockLocationOnlyThisTestUses {
            location: MachineRuntimeLockLocation,
        }

        impl LockLocationOnlyThisTestUses {
            /// An abstract name no real runtime and no other test binds.
            #[cfg(target_os = "linux")]
            fn new() -> Self {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static NEXT_TEST_LOCK_NUMBER: AtomicUsize = AtomicUsize::new(0);
                Self {
                    location: MachineRuntimeLockLocation {
                        abstract_socket_name: format!(
                            "tatolab-runtime-test-{}-{}",
                            std::process::id(),
                            NEXT_TEST_LOCK_NUMBER.fetch_add(1, Ordering::SeqCst)
                        ),
                    },
                }
            }

            /// A loopback port free when chosen, which no real runtime binds.
            #[cfg(target_os = "macos")]
            fn new() -> Self {
                Self {
                    location: MachineRuntimeLockLocation {
                        loopback_http_listener_address: SocketAddrV4::new(
                            Ipv4Addr::LOCALHOST,
                            a_free_loopback_port(),
                        ),
                    },
                }
            }

            /// What a child process is handed to find this location again.
            fn handed_to_a_child(&self) -> OsString {
                #[cfg(target_os = "linux")]
                {
                    OsString::from(&self.location.abstract_socket_name)
                }
                #[cfg(target_os = "macos")]
                {
                    OsString::from(
                        self.location
                            .loopback_http_listener_address
                            .port()
                            .to_string(),
                    )
                }
            }
        }

        fn the_location_a_parent_handed_this_child(handed: OsString) -> MachineRuntimeLockLocation {
            #[cfg(target_os = "linux")]
            {
                MachineRuntimeLockLocation {
                    abstract_socket_name: handed.into_string().unwrap(),
                }
            }
            #[cfg(target_os = "macos")]
            {
                MachineRuntimeLockLocation {
                    loopback_http_listener_address: SocketAddrV4::new(
                        Ipv4Addr::LOCALHOST,
                        handed.into_string().unwrap().parse().unwrap(),
                    ),
                }
            }
        }

        /// A lock this test holds, answering its holder route on macOS as
        /// `tatolabd` does once it serves its loopback HTTP listener.
        struct LockHeldAndAnsweringAsItsHolder {
            #[cfg(target_os = "macos")]
            _holder_route_answered: TestThreadAnsweringEveryConnection,
            _held_lock: MachineRuntimeLock,
        }

        fn take_and_answer_as_its_holder(
            location: &MachineRuntimeLockLocation,
        ) -> LockHeldAndAnsweringAsItsHolder {
            #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
            let mut held_lock = take_the_machine_runtime_lock_at(location).unwrap();
            LockHeldAndAnsweringAsItsHolder {
                #[cfg(target_os = "macos")]
                _holder_route_answered:
                    TestThreadAnsweringEveryConnection::answering_this_processes_pid(
                        held_lock.take_the_bound_loopback_http_listener().unwrap(),
                    ),
                _held_lock: held_lock,
            }
        }

        /// The holder with its executable canonicalized, since `/proc/<pid>/exe`
        /// and `proc_pidpath` name the file with every symlink resolved.
        fn with_its_executable_canonicalized(
            holder: MachineRuntimeLockHolder,
        ) -> MachineRuntimeLockHolder {
            MachineRuntimeLockHolder {
                executable: holder
                    .executable
                    .map(|executable| std::fs::canonicalize(executable).unwrap()),
                ..holder
            }
        }

        fn a_holder_running_as_this_uid(pid: i32, executable: &Path) -> MachineRuntimeLockHolder {
            let uid = crate::streamlib_runtime_directory::current_process_uid();
            MachineRuntimeLockHolder {
                user_name: user_name_of_uid(uid),
                uid,
                pid,
                executable: Some(std::fs::canonicalize(executable).unwrap()),
            }
        }

        fn this_process_as_the_holder() -> MachineRuntimeLockHolder {
            a_holder_running_as_this_uid(
                std::process::id() as i32,
                &std::env::current_exe().unwrap(),
            )
        }

        fn parent_process_id() -> i32 {
            // SAFETY: getppid takes no arguments and cannot fail.
            unsafe { libc::getppid() }
        }

        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn a_take_while_another_executable_holds_the_lock_is_refused_naming_its_uid_pid_and_executable()
         {
            if let Some(handed) = std::env::var_os(HOLD_THE_LOCK_CHILD_ENVIRONMENT_VARIABLE) {
                let location = the_location_a_parent_handed_this_child(handed);
                let _held_lock = take_and_answer_as_its_holder(&location);
                std::fs::write(
                    std::env::var_os(HOLDING_CHILD_READY_FILE_ENVIRONMENT_VARIABLE).unwrap(),
                    b"",
                )
                .unwrap();
                let mut until_the_parent_closes_standard_input = Vec::new();
                std::io::stdin()
                    .read_to_end(&mut until_the_parent_closes_standard_input)
                    .unwrap();
                return;
            }

            let lock = LockLocationOnlyThisTestUses::new();
            // A copy of this test binary under another name holds the lock, so
            // the executable named is the holder's, never the prober's own.
            let holder_binary_directory =
                crate::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
            let holder_binary = holder_binary_directory
                .path()
                .join("machine-runtime-lock-holder");
            std::fs::copy(std::env::current_exe().unwrap(), &holder_binary).unwrap();
            let ready_file = holder_binary_directory.path().join("holding-the-lock");
            let mut holding_child = spawn_the_holding_child(&holder_binary, |child_command| {
                child_command
                    .env(
                        HOLD_THE_LOCK_CHILD_ENVIRONMENT_VARIABLE,
                        lock.handed_to_a_child(),
                    )
                    .env(HOLDING_CHILD_READY_FILE_ENVIRONMENT_VARIABLE, &ready_file);
            });
            wait_until_the_holding_child_holds_the_lock(&mut holding_child, &ready_file);
            let expected_holder =
                a_holder_running_as_this_uid(holding_child.id() as i32, &holder_binary);

            let refusal = take_the_machine_runtime_lock_at(&lock.location).unwrap_err();
            let probed_holder = holder_of_the_machine_runtime_lock_at(&lock.location).unwrap();

            drop(holding_child.stdin.take());
            let holding_child_output = holding_child.wait_with_output().unwrap();
            assert_the_child_process_ran_the_test(HOLDING_CHILD_TEST_PATH, &holding_child_output);
            assert!(
                holding_child_output.status.success(),
                "the holding child failed: {}{}",
                String::from_utf8_lossy(&holding_child_output.stdout),
                String::from_utf8_lossy(&holding_child_output.stderr),
            );
            assert_ne!(
                expected_holder.executable,
                this_process_as_the_holder().executable
            );
            let refusal_text = refusal.to_string();
            let MachineRuntimeLockRefusal::HeldByAnotherRuntime { holder } = refusal else {
                panic!("the refusal must name the holder: {refusal}");
            };
            assert_eq!(with_its_executable_canonicalized(holder), expected_holder);
            assert_eq!(
                probed_holder.map(with_its_executable_canonicalized),
                Some(expected_holder.clone())
            );
            assert!(
                refusal_text.starts_with("another runtime holds this machine: "),
                "{refusal_text}"
            );
            assert!(
                refusal_text.contains(&format!("uid {}", expected_holder.uid)),
                "{refusal_text}"
            );
            assert!(
                refusal_text.contains(&format!("pid {}", expected_holder.pid)),
                "{refusal_text}"
            );
            assert!(
                refusal_text.contains("machine-runtime-lock-holder"),
                "{refusal_text}"
            );
        }

        /// Start the holding child, retrying while a child another test forks
        /// still holds the just-copied binary open for writing.
        fn spawn_the_holding_child(
            holder_binary: &Path,
            prepare_the_child_environment: impl Fn(&mut std::process::Command),
        ) -> std::process::Child {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match spawn_this_test_from_a_test_binary(
                    holder_binary,
                    HOLDING_CHILD_TEST_PATH,
                    &prepare_the_child_environment,
                ) {
                    Ok(holding_child) => return holding_child,
                    Err(failure)
                        if failure.raw_os_error() == Some(libc::ETXTBSY)
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(failure) => panic!("the holding child did not start: {failure}"),
                }
            }
        }

        fn wait_until_the_holding_child_holds_the_lock(
            holding_child: &mut std::process::Child,
            ready_file: &Path,
        ) {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !ready_file.exists() {
                if let Some(status) = holding_child.try_wait().unwrap() {
                    let mut standard_output = String::new();
                    let mut standard_error = String::new();
                    if let Some(mut stdout) = holding_child.stdout.take() {
                        let _ = stdout.read_to_string(&mut standard_output);
                    }
                    if let Some(mut stderr) = holding_child.stderr.take() {
                        let _ = stderr.read_to_string(&mut standard_error);
                    }
                    panic!(
                        "the holding child exited ({status}) before it held the lock: \
                         {standard_output}{standard_error}"
                    );
                }
                assert!(
                    Instant::now() < deadline,
                    "the holding child did not hold the lock within 30 s"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn a_second_take_in_this_process_is_refused_naming_this_process() {
            let lock = LockLocationOnlyThisTestUses::new();
            let _held_lock = take_and_answer_as_its_holder(&lock.location);

            let refusal = take_the_machine_runtime_lock_at(&lock.location).unwrap_err();

            let MachineRuntimeLockRefusal::HeldByAnotherRuntime { holder } = refusal else {
                panic!("the refusal must name the holder: {refusal}");
            };
            assert_eq!(
                with_its_executable_canonicalized(holder),
                this_process_as_the_holder()
            );
        }

        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn probes_and_refused_takes_by_the_holder_leave_the_lock_held() {
            if let Some(handed) =
                std::env::var_os(PROBE_THE_PARENTS_LOCK_CHILD_ENVIRONMENT_VARIABLE)
            {
                let location = the_location_a_parent_handed_this_child(handed);
                let parent_pid = parent_process_id();
                assert_eq!(
                    holder_of_the_machine_runtime_lock_at(&location)
                        .unwrap()
                        .map(|holder| holder.pid),
                    Some(parent_pid)
                );
                match take_the_machine_runtime_lock_at(&location) {
                    Err(MachineRuntimeLockRefusal::HeldByAnotherRuntime { holder }) => {
                        assert_eq!(holder.pid, parent_pid);
                    }
                    other => panic!("the parent's lock must refuse this take: {other:?}"),
                }
                return;
            }

            let lock = LockLocationOnlyThisTestUses::new();
            let _held_lock = take_and_answer_as_its_holder(&lock.location);
            for _probe_round in 0..3 {
                assert_eq!(
                    holder_of_the_machine_runtime_lock_at(&lock.location)
                        .unwrap()
                        .map(with_its_executable_canonicalized),
                    Some(this_process_as_the_holder())
                );
                assert!(matches!(
                    take_the_machine_runtime_lock_at(&lock.location),
                    Err(MachineRuntimeLockRefusal::HeldByAnotherRuntime { .. })
                ));
            }

            let child = rerun_this_test_in_a_child_process(
                PROBING_CHILD_TEST_PATH,
                PROBE_THE_PARENTS_LOCK_CHILD_ENVIRONMENT_VARIABLE,
                &lock.handed_to_a_child(),
            );
            assert!(
                child.status.success(),
                "the probing child did not find the lock held by this process: {}{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr),
            );
        }

        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn the_lock_is_free_again_once_its_holder_drops_it() {
            let lock = LockLocationOnlyThisTestUses::new();
            drop(take_the_machine_runtime_lock_at(&lock.location).unwrap());

            let _taken_again = once_every_forked_copy_of_the_dropped_lock_is_gone(|| {
                take_the_machine_runtime_lock_at(&lock.location).ok()
            });
        }

        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn the_holder_probe_names_nobody_when_free_and_the_holder_when_held() {
            let lock = LockLocationOnlyThisTestUses::new();
            assert_eq!(
                holder_of_the_machine_runtime_lock_at(&lock.location).unwrap(),
                None
            );

            let held_lock = take_and_answer_as_its_holder(&lock.location);
            assert_eq!(
                holder_of_the_machine_runtime_lock_at(&lock.location)
                    .unwrap()
                    .map(with_its_executable_canonicalized),
                Some(this_process_as_the_holder())
            );

            drop(held_lock);
            once_every_forked_copy_of_the_dropped_lock_is_gone(|| {
                holder_of_the_machine_runtime_lock_at(&lock.location)
                    .unwrap()
                    .is_none()
                    .then_some(())
            });
            let _taken_after_the_probes = take_the_machine_runtime_lock_at(&lock.location).unwrap();
        }

        /// On Linux a test beside this one that spawns a child process copies
        /// every descriptor this process holds into it until it execs, the
        /// dropped lock's socket included, so the name frees once that copy closes.
        fn once_every_forked_copy_of_the_dropped_lock_is_gone<T>(
            mut attempt: impl FnMut() -> Option<T>,
        ) -> T {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(outcome) = attempt() {
                    return outcome;
                }
                assert!(
                    Instant::now() < deadline,
                    "the dropped lock was still held 10 s later"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        #[cfg(target_os = "linux")]
        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn probes_and_refused_takes_never_fill_the_holders_listen_queue() {
            const SHRUNK_LISTEN_BACKLOG: libc::c_int = 1;
            const PROBES_PAST_THE_SHRUNK_BACKLOG: usize = 64;
            let lock = LockLocationOnlyThisTestUses::new();
            let held_lock = take_the_machine_runtime_lock_at(&lock.location).unwrap();
            let listening_descriptor = held_lock
                ._listening_abstract_socket
                .listening_abstract_socket_descriptor();
            // A second listen on a listening unix socket changes only its backlog.
            // SAFETY: the descriptor is the held lock's open listening socket.
            let relistened = unsafe { libc::listen(listening_descriptor, SHRUNK_LISTEN_BACKLOG) };
            assert_eq!(relistened, 0, "{}", std::io::Error::last_os_error());

            for probe_number in 0..PROBES_PAST_THE_SHRUNK_BACKLOG {
                let named_holder = until_the_holder_answers(|| {
                    if probe_number % 2 == 0 {
                        holder_of_the_machine_runtime_lock_at(&lock.location)
                    } else {
                        match take_the_machine_runtime_lock_at(&lock.location) {
                            Ok(_) => panic!("a held lock was taken a second time"),
                            Err(MachineRuntimeLockRefusal::HeldByAnotherRuntime { holder }) => {
                                Ok(Some(holder))
                            }
                            Err(refusal) => Err(refusal),
                        }
                    }
                });
                assert_eq!(
                    with_its_executable_canonicalized(named_holder),
                    this_process_as_the_holder(),
                    "probe {probe_number}"
                );
            }
        }

        /// Retry while the holder's queue is momentarily full: its draining
        /// thread runs beside these probes, not in step with them.
        #[cfg(target_os = "linux")]
        fn until_the_holder_answers(
            mut probe: impl FnMut()
                -> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal>,
        ) -> MachineRuntimeLockHolder {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match probe() {
                    Ok(Some(holder)) => return holder,
                    Err(MachineRuntimeLockRefusal::HeldByAProcessThatDoesNotAnswer { lock }) => {
                        assert!(
                            Instant::now() < deadline,
                            "{lock} still did not answer 10 s later: its listen queue stays full"
                        );
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    other => panic!("the probe must name the holder: {other:?}"),
                }
            }
        }

        #[cfg(target_os = "linux")]
        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn a_squatter_that_binds_the_name_without_listening_is_refused() {
            use super::super::linux_abstract_socket_lock::abstract_socket_address;
            use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
            let lock = LockLocationOnlyThisTestUses::new();
            let (address, address_length) =
                abstract_socket_address(&lock.location.abstract_socket_name).unwrap();
            // SAFETY: plain socket creation; the descriptor is owned below.
            let descriptor =
                unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
            assert!(descriptor >= 0, "{}", std::io::Error::last_os_error());
            // SAFETY: `descriptor` is a fresh socket nothing else owns.
            let squatter = unsafe { OwnedFd::from_raw_fd(descriptor) };
            // SAFETY: `address` is a live `sockaddr_un` and `address_length` covers its used bytes.
            let bound = unsafe {
                libc::bind(
                    squatter.as_raw_fd(),
                    (&address as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
                    address_length,
                )
            };
            assert_eq!(bound, 0, "{}", std::io::Error::last_os_error());

            let refusal = take_the_machine_runtime_lock_at(&lock.location)
                .unwrap_err()
                .to_string();

            assert_eq!(
                refusal,
                format!(
                    "another runtime holds this machine: the abstract socket @{} is held by a \
                     process that does not answer",
                    lock.location.abstract_socket_name
                )
            );
        }

        #[cfg(target_os = "macos")]
        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn a_take_on_a_free_port_hands_over_its_bound_listener_once_and_keeps_the_port_bound() {
            let lock = LockLocationOnlyThisTestUses::new();
            let address = lock.location.loopback_http_listener_address;
            let mut held_lock = take_the_machine_runtime_lock_at(&lock.location).unwrap();

            let handed_over_listener = held_lock.take_the_bound_loopback_http_listener().unwrap();
            assert_eq!(
                handed_over_listener.local_addr().unwrap(),
                SocketAddr::V4(address)
            );
            assert!(held_lock.take_the_bound_loopback_http_listener().is_none());
            drop(handed_over_listener);

            assert_eq!(
                TcpListener::bind(address).unwrap_err().kind(),
                std::io::ErrorKind::AddrInUse,
                "the port must stay bound while the lock is held"
            );
        }

        /// Without a server answering the holder route, a second take and a
        /// probe meet a bound port that names nobody.
        #[cfg(target_os = "macos")]
        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn a_second_take_while_the_holder_serves_no_answer_is_refused_naming_the_port() {
            let lock = LockLocationOnlyThisTestUses::new();
            let _held_lock = take_the_machine_runtime_lock_at(&lock.location).unwrap();

            let refusal = take_the_machine_runtime_lock_at(&lock.location).unwrap_err();

            assert_eq!(
                refusal.to_string(),
                format!(
                    "another runtime holds this machine: the loopback port {} is held by a \
                     process that does not answer",
                    lock.location.loopback_http_listener_address
                )
            );
        }

        #[cfg(target_os = "macos")]
        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn a_foreign_listener_on_the_port_is_refused_as_a_process_that_does_not_answer() {
            let lock = LockLocationOnlyThisTestUses::new();
            let _foreign_listener =
                TcpListener::bind(lock.location.loopback_http_listener_address).unwrap();
            let expected_refusal = format!(
                "another runtime holds this machine: the loopback port {} is held by a process \
                 that does not answer",
                lock.location.loopback_http_listener_address
            );

            let refusals = [
                take_the_machine_runtime_lock_at(&lock.location).unwrap_err(),
                holder_of_the_machine_runtime_lock_at(&lock.location).unwrap_err(),
            ];

            for refusal in refusals {
                assert_eq!(refusal.to_string(), expected_refusal);
            }
        }
    }

    /// The test build's root, read from the environment, so each arm runs in a
    /// child process whose environment it owns.
    #[cfg(feature = "machine-directories-under-a-test-root")]
    mod under_a_test_machine_root {
        use super::*;
        use crate::machine_directories_test_root::{
            TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE, TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE,
        };
        use crate::streamlib_runtime_directory::StreamlibRuntimeDirectory;
        use crate::tatolab_state_directory::TatolabStateDirectory;
        use std::os::unix::fs::PermissionsExt;

        /// Set only in the child process the unset-root test re-runs itself in.
        const UNSET_TEST_MACHINE_ROOT_CHILD_ENVIRONMENT_VARIABLE: &str =
            "STREAMLIB_TEST_UNSET_TEST_MACHINE_ROOT_CHILD";

        /// Set only in the child process the unset-port test re-runs itself in.
        #[cfg(target_os = "macos")]
        const UNSET_TEST_LOOPBACK_HTTP_PORT_CHILD_ENVIRONMENT_VARIABLE: &str =
            "STREAMLIB_TEST_UNSET_TEST_LOOPBACK_HTTP_PORT_CHILD";

        fn mode_of(path: &Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        #[test]
        fn the_test_root_moves_the_runtime_directory_the_state_directory_and_the_lock() {
            if let Some(test_machine_root) =
                std::env::var_os(TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE)
            {
                let test_machine_root = PathBuf::from(test_machine_root);

                let runtime_directory = StreamlibRuntimeDirectory::resolve().unwrap();
                assert_eq!(runtime_directory.path(), test_machine_root.join("run"));
                assert_eq!(mode_of(runtime_directory.path()), 0o700);
                assert_eq!(
                    runtime_directory.local_api_socket_path(),
                    test_machine_root.join("run/local-api.sock")
                );
                assert_eq!(
                    StreamlibRuntimeDirectory::resolve_for_a_reader_without_creating()
                        .unwrap()
                        .path(),
                    test_machine_root.join("run")
                );

                let state_directory = TatolabStateDirectory::resolve().unwrap();
                assert_eq!(state_directory.path(), test_machine_root.join("state"));
                assert_eq!(mode_of(state_directory.path()), 0o700);

                assert_eq!(holder_of_the_machine_runtime_lock().unwrap(), None);
                #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
                let mut held_lock = MachineRuntimeLock::take().unwrap();
                #[cfg(target_os = "macos")]
                let _holder_route_answered = {
                    let handed_over_listener =
                        held_lock.take_the_bound_loopback_http_listener().unwrap();
                    let test_loopback_http_port =
                        crate::machine_directories_test_root::test_loopback_http_port_from_the_environment()
                            .unwrap();
                    assert_eq!(
                        handed_over_listener.local_addr().unwrap(),
                        SocketAddr::from((Ipv4Addr::LOCALHOST, test_loopback_http_port))
                    );
                    assert_ne!(
                        test_loopback_http_port,
                        crate::local_api_wire_contract::LOOPBACK_HTTP_LISTENER_DEFAULT_PORT
                    );
                    TestThreadAnsweringEveryConnection::answering_this_processes_pid(
                        handed_over_listener,
                    )
                };
                let this_pid = std::process::id() as i32;
                assert_eq!(
                    holder_of_the_machine_runtime_lock()
                        .unwrap()
                        .map(|holder| holder.pid),
                    Some(this_pid)
                );
                #[cfg(target_os = "linux")]
                {
                    let held_at_the_production_name =
                        super::super::linux_abstract_socket_lock::holder(
                            MACHINE_RUNTIME_LOCK_ABSTRACT_SOCKET_NAME,
                        )
                        .unwrap();
                    assert_ne!(
                        held_at_the_production_name.map(|holder| holder.pid),
                        Some(this_pid),
                        "the test build took the real machine's lock"
                    );
                }
                drop(held_lock);
                return;
            }

            // Short and under /tmp whatever TMPDIR says: socket paths under it stay below 104 bytes.
            let test_machine_root = tempfile::Builder::new()
                .prefix("tl-")
                .tempdir_in("/tmp")
                .unwrap();
            let test_loopback_http_port = a_free_loopback_port();
            let child =
                crate::test_support::rerun_this_test_in_a_child_process_with_its_environment(
                    "machine_runtime_lock::tests::under_a_test_machine_root::the_test_root_moves_the_runtime_directory_the_state_directory_and_the_lock",
                    |child_command| {
                        child_command
                            .env(
                                TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE,
                                test_machine_root.path(),
                            )
                            .env(
                                TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE,
                                test_loopback_http_port.to_string(),
                            );
                    },
                );
            assert!(
                child.status.success(),
                "the test-root arm failed in the child: {}{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr),
            );
        }

        #[test]
        fn an_unset_test_root_is_refused_by_every_machine_directory_and_the_lock() {
            if std::env::var_os(UNSET_TEST_MACHINE_ROOT_CHILD_ENVIRONMENT_VARIABLE).is_some() {
                let refusals = [
                    StreamlibRuntimeDirectory::resolve()
                        .unwrap_err()
                        .to_string(),
                    StreamlibRuntimeDirectory::resolve_for_a_reader_without_creating()
                        .unwrap_err()
                        .to_string(),
                    TatolabStateDirectory::resolve().unwrap_err().to_string(),
                    TatolabStateDirectory::resolve_for_a_reader_without_creating()
                        .unwrap_err()
                        .to_string(),
                    MachineRuntimeLock::take().unwrap_err().to_string(),
                    holder_of_the_machine_runtime_lock()
                        .unwrap_err()
                        .to_string(),
                ];
                for refusal in refusals {
                    assert!(
                        refusal.starts_with(&format!(
                            "{TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE} is not set"
                        )),
                        "{refusal}"
                    );
                }
                return;
            }

            let child =
                crate::test_support::rerun_this_test_in_a_child_process_with_its_environment(
                    "machine_runtime_lock::tests::under_a_test_machine_root::an_unset_test_root_is_refused_by_every_machine_directory_and_the_lock",
                    |child_command| {
                        child_command
                            .env(UNSET_TEST_MACHINE_ROOT_CHILD_ENVIRONMENT_VARIABLE, "1")
                            .env_remove(TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE);
                    },
                );
            assert!(
                child.status.success(),
                "the unset-root arm failed in the child: {}{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr),
            );
        }

        /// The macOS lock is the test's loopback HTTP port, so a test build
        /// naming none takes no lock at all, never the machine's own.
        #[cfg(target_os = "macos")]
        #[test]
        fn a_test_build_naming_no_loopback_http_port_is_refused_at_the_lock_naming_the_variable() {
            if std::env::var_os(UNSET_TEST_LOOPBACK_HTTP_PORT_CHILD_ENVIRONMENT_VARIABLE).is_some()
            {
                for refusal in [
                    MachineRuntimeLock::take().unwrap_err().to_string(),
                    holder_of_the_machine_runtime_lock()
                        .unwrap_err()
                        .to_string(),
                ] {
                    assert!(
                        refusal.starts_with(&format!(
                            "{TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE} is not set"
                        )),
                        "{refusal}"
                    );
                }
                return;
            }

            let test_machine_root = tempfile::Builder::new()
                .prefix("tl-")
                .tempdir_in("/tmp")
                .unwrap();
            let child =
                crate::test_support::rerun_this_test_in_a_child_process_with_its_environment(
                    "machine_runtime_lock::tests::under_a_test_machine_root::a_test_build_naming_no_loopback_http_port_is_refused_at_the_lock_naming_the_variable",
                    |child_command| {
                        child_command
                            .env(
                                UNSET_TEST_LOOPBACK_HTTP_PORT_CHILD_ENVIRONMENT_VARIABLE,
                                "1",
                            )
                            .env(
                                TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE,
                                test_machine_root.path(),
                            )
                            .env_remove(TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE);
                    },
                );
            assert!(
                child.status.success(),
                "the unset-port arm failed in the child: {}{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr),
            );
        }
    }
}
