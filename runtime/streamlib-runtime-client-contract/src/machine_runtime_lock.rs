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
//! unnameable. On macOS it is
//! an `fcntl` write lock on a root-owned 0666 file in a root-owned directory,
//! which any user may take and none may replace.

use std::fmt;
use std::path::PathBuf;

/// The abstract unix socket name the Linux machine runtime lock binds, without its leading NUL.
pub const MACHINE_RUNTIME_LOCK_ABSTRACT_SOCKET_NAME: &str = "tatolab-runtime";

/// The root-owned directory the macOS machine runtime lock file sits in.
pub const MACHINE_RUNTIME_LOCK_DIRECTORY_ON_MACOS: &str = "/Library/Application Support/Tatolab";

/// The macOS machine runtime lock file's name inside its directory.
pub const MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS: &str = "runtime.lock";

/// The process holding the machine runtime lock, as far as the kernel names it.
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
    /// Another process holds the lock, named by the kernel.
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
    /// The lock's directory or file does not exist.
    #[error(
        "the machine runtime lock cannot be taken: {missing_path} does not exist. Tatolab.app \
         creates it at first launch; to create it from a terminal, run: {creation_commands}"
    )]
    LockLocationIsMissing {
        missing_path: PathBuf,
        creation_commands: String,
    },
    /// The lock's directory or file has a type, owner or mode anyone could subvert.
    #[error(
        "the machine runtime lock cannot be trusted: the {property} of {path} is {found}, and it \
         must be {required}"
    )]
    LockLocationCannotBeTrusted {
        path: PathBuf,
        property: &'static str,
        found: String,
        required: String,
    },
    /// The operating system refused an operation the lock needs.
    #[error("the machine runtime lock {lock} could not be taken: {source}")]
    CouldNotBeTaken {
        lock: String,
        source: std::io::Error,
    },
    /// This test build's machine root was refused.
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
    _locked_file: macos_fcntl_lock::LockedMachineRuntimeLockFile,
}

impl MachineRuntimeLock {
    /// Take the machine runtime lock, refusing with its holder named when another process has it.
    pub fn take() -> Result<Self, MachineRuntimeLockRefusal> {
        let location = MachineRuntimeLockLocation::for_this_build()?;
        take_the_machine_runtime_lock_at(&location)
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
    lock_directory: PathBuf,
    #[cfg(target_os = "macos")]
    lock_file: PathBuf,
    #[cfg(target_os = "macos")]
    expected_owner_uid: u32,
}

impl MachineRuntimeLockLocation {
    #[cfg(not(feature = "machine-directories-under-a-test-root"))]
    fn for_this_build() -> Result<Self, MachineRuntimeLockRefusal> {
        Ok(Self {
            #[cfg(target_os = "linux")]
            abstract_socket_name: MACHINE_RUNTIME_LOCK_ABSTRACT_SOCKET_NAME.to_string(),
            #[cfg(target_os = "macos")]
            lock_directory: PathBuf::from(MACHINE_RUNTIME_LOCK_DIRECTORY_ON_MACOS),
            #[cfg(target_os = "macos")]
            lock_file: PathBuf::from(MACHINE_RUNTIME_LOCK_DIRECTORY_ON_MACOS)
                .join(MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS),
            #[cfg(target_os = "macos")]
            expected_owner_uid: 0,
        })
    }

    #[cfg(feature = "machine-directories-under-a-test-root")]
    fn for_this_build() -> Result<Self, MachineRuntimeLockRefusal> {
        let test_machine_root =
            crate::machine_directories_test_root::TestMachineRoot::from_the_environment()?;
        Ok(Self {
            #[cfg(target_os = "linux")]
            abstract_socket_name: test_machine_root.machine_runtime_lock_abstract_socket_name(
                MACHINE_RUNTIME_LOCK_ABSTRACT_SOCKET_NAME,
            ),
            #[cfg(target_os = "macos")]
            lock_directory: test_machine_root.machine_runtime_lock_directory(),
            #[cfg(target_os = "macos")]
            lock_file: test_machine_root
                .machine_runtime_lock_directory()
                .join(MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS),
            #[cfg(target_os = "macos")]
            expected_owner_uid: crate::streamlib_runtime_directory::current_process_uid(),
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
        _locked_file: macos_fcntl_lock::take(location)?,
    })
}

#[cfg(target_os = "macos")]
fn holder_of_the_machine_runtime_lock_at(
    location: &MachineRuntimeLockLocation,
) -> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal> {
    macos_fcntl_lock::holder(location)
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

/// The type, owner and mode checks the macOS lock's directory and file must
/// pass, as pure functions over their metadata so every refusal is testable on
/// every floor.
#[cfg(any(target_os = "macos", test))]
mod lock_location_trust_check {
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use super::MachineRuntimeLockRefusal;

    /// The only mode the lock file is trusted at: every user may open it to
    /// take the lock, so whoever starts first is the machine's runtime.
    const MACHINE_RUNTIME_LOCK_FILE_MODE: u32 = 0o666;

    /// The permission bits that let a group or other replace an entry in a directory.
    const GROUP_AND_OTHER_WRITE_BITS: u32 = 0o022;

    /// What kind of entry stands at a lock path, as `lstat` sees it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum LockPathEntryKind {
        Directory,
        RegularFile,
        Symlink,
        OtherEntry,
    }

    impl LockPathEntryKind {
        fn described(self) -> &'static str {
            match self {
                Self::Directory => "a directory",
                Self::RegularFile => "a regular file",
                Self::Symlink => "a symlink",
                Self::OtherEntry => "neither a regular file nor a directory",
            }
        }
    }

    /// The parts of a lock path's metadata the trust checks read.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct LockPathMetadata {
        pub(super) kind: LockPathEntryKind,
        pub(super) owner_uid: u32,
        pub(super) mode: u32,
    }

    impl LockPathMetadata {
        /// From metadata that did not follow a symlink (`lstat`, or `fstat` of an open descriptor).
        pub(super) fn from_unfollowed_metadata(metadata: &std::fs::Metadata) -> Self {
            let file_type = metadata.file_type();
            let kind = if file_type.is_symlink() {
                LockPathEntryKind::Symlink
            } else if file_type.is_dir() {
                LockPathEntryKind::Directory
            } else if file_type.is_file() {
                LockPathEntryKind::RegularFile
            } else {
                LockPathEntryKind::OtherEntry
            };
            Self {
                kind,
                owner_uid: metadata.uid(),
                mode: metadata.mode() & 0o7777,
            }
        }
    }

    fn described_uid(uid: u32) -> String {
        if uid == 0 {
            "uid 0 (root)".to_string()
        } else {
            format!("uid {uid}")
        }
    }

    fn cannot_be_trusted(
        path: &Path,
        property: &'static str,
        found: String,
        required: String,
    ) -> MachineRuntimeLockRefusal {
        MachineRuntimeLockRefusal::LockLocationCannotBeTrusted {
            path: path.to_path_buf(),
            property,
            found,
            required,
        }
    }

    /// Refuse a lock path that is not of `expected_kind`, or that `expected_owner_uid` does not
    /// own.
    fn refuse_unless_kind_and_owner(
        path: &Path,
        metadata: LockPathMetadata,
        expected_kind: LockPathEntryKind,
        required_kind_text: &str,
        expected_owner_uid: u32,
    ) -> Result<(), MachineRuntimeLockRefusal> {
        if metadata.kind != expected_kind {
            return Err(cannot_be_trusted(
                path,
                "type",
                metadata.kind.described().to_string(),
                required_kind_text.to_string(),
            ));
        }
        if metadata.owner_uid != expected_owner_uid {
            return Err(cannot_be_trusted(
                path,
                "owner",
                described_uid(metadata.owner_uid),
                described_uid(expected_owner_uid),
            ));
        }
        Ok(())
    }

    /// The lock's directory must be a real directory the expected owner owns,
    /// which no group or other can write into.
    pub(super) fn refuse_a_lock_directory_that_cannot_be_trusted(
        path: &Path,
        metadata: LockPathMetadata,
        expected_owner_uid: u32,
    ) -> Result<(), MachineRuntimeLockRefusal> {
        refuse_unless_kind_and_owner(
            path,
            metadata,
            LockPathEntryKind::Directory,
            "a real directory, not a symlink",
            expected_owner_uid,
        )?;
        if metadata.mode & GROUP_AND_OTHER_WRITE_BITS != 0 {
            return Err(cannot_be_trusted(
                path,
                "mode",
                format!("{:04o}", metadata.mode),
                "writable by its owner only (no group or other write bit)".to_string(),
            ));
        }
        Ok(())
    }

    /// The lock file must be a regular file the expected owner owns, at exactly 0666.
    pub(super) fn refuse_a_lock_file_that_cannot_be_trusted(
        path: &Path,
        metadata: LockPathMetadata,
        expected_owner_uid: u32,
    ) -> Result<(), MachineRuntimeLockRefusal> {
        refuse_unless_kind_and_owner(
            path,
            metadata,
            LockPathEntryKind::RegularFile,
            "a regular file, not a symlink",
            expected_owner_uid,
        )?;
        if metadata.mode != MACHINE_RUNTIME_LOCK_FILE_MODE {
            return Err(cannot_be_trusted(
                path,
                "mode",
                format!("{:04o}", metadata.mode),
                format!("exactly {MACHINE_RUNTIME_LOCK_FILE_MODE:04o}"),
            ));
        }
        Ok(())
    }

    /// The shell commands that create the lock's directory and file, run with
    /// `sudo` when they must be owned by root.
    pub(super) fn the_commands_that_create_the_lock_location(
        lock_directory: &Path,
        lock_file: &Path,
        expected_owner_uid: u32,
    ) -> String {
        let administrator = if expected_owner_uid == 0 { "sudo " } else { "" };
        format!(
            "{administrator}mkdir -p \"{directory}\" && {administrator}touch \"{file}\" && \
             {administrator}chmod {MACHINE_RUNTIME_LOCK_FILE_MODE:04o} \"{file}\"",
            directory = lock_directory.display(),
            file = lock_file.display(),
        )
    }
}

#[cfg(target_os = "macos")]
mod macos_fcntl_lock {
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::lock_location_trust_check::{
        LockPathMetadata, refuse_a_lock_directory_that_cannot_be_trusted,
        refuse_a_lock_file_that_cannot_be_trusted, the_commands_that_create_the_lock_location,
    };
    use super::{
        MachineRuntimeLockHolder, MachineRuntimeLockLocation, MachineRuntimeLockRefusal,
        user_name_of_uid,
    };

    /// An `fcntl` lock is the process's, not the descriptor's: a second take in
    /// this process would succeed, and closing any descriptor of the file —
    /// a probe's included — would release it. So this process remembers it holds it.
    static THIS_PROCESS_HOLDS_THE_MACHINE_RUNTIME_LOCK: AtomicBool = AtomicBool::new(false);

    /// The open lock file whose write lock this process holds until it drops.
    #[derive(Debug)]
    pub(super) struct LockedMachineRuntimeLockFile {
        _lock_file: File,
    }

    impl Drop for LockedMachineRuntimeLockFile {
        fn drop(&mut self) {
            THIS_PROCESS_HOLDS_THE_MACHINE_RUNTIME_LOCK.store(false, Ordering::SeqCst);
        }
    }

    fn displayed_lock_name(lock_file: &Path) -> String {
        format!("the lock file {}", lock_file.display())
    }

    fn could_not_be_taken(lock_file: &Path, source: std::io::Error) -> MachineRuntimeLockRefusal {
        MachineRuntimeLockRefusal::CouldNotBeTaken {
            lock: displayed_lock_name(lock_file),
            source,
        }
    }

    fn lock_location_is_missing(
        location: &MachineRuntimeLockLocation,
        missing_path: &Path,
    ) -> MachineRuntimeLockRefusal {
        MachineRuntimeLockRefusal::LockLocationIsMissing {
            missing_path: missing_path.to_path_buf(),
            creation_commands: the_commands_that_create_the_lock_location(
                &location.lock_directory,
                &location.lock_file,
                location.expected_owner_uid,
            ),
        }
    }

    /// `lstat` both paths and refuse a missing or untrusted one by name.
    fn refuse_a_lock_location_that_is_missing_or_untrusted(
        location: &MachineRuntimeLockLocation,
    ) -> Result<(), MachineRuntimeLockRefusal> {
        for (path, refuse_it_unless_trusted) in [
            (
                &location.lock_directory,
                refuse_a_lock_directory_that_cannot_be_trusted
                    as fn(&Path, LockPathMetadata, u32) -> Result<(), MachineRuntimeLockRefusal>,
            ),
            (
                &location.lock_file,
                refuse_a_lock_file_that_cannot_be_trusted,
            ),
        ] {
            let metadata = match std::fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => {
                    return Err(lock_location_is_missing(location, path));
                }
                Err(failure) => return Err(could_not_be_taken(path, failure)),
            };
            refuse_it_unless_trusted(
                path,
                LockPathMetadata::from_unfollowed_metadata(&metadata),
                location.expected_owner_uid,
            )?;
        }
        Ok(())
    }

    /// Open the lock file without following a symlink, and check the file
    /// actually opened, so a swap after the `lstat` is refused too.
    fn open_the_trusted_lock_file(
        location: &MachineRuntimeLockLocation,
    ) -> Result<File, MachineRuntimeLockRefusal> {
        refuse_a_lock_location_that_is_missing_or_untrusted(location)?;
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&location.lock_file)
            .map_err(|failure| match failure.kind() {
                std::io::ErrorKind::NotFound => {
                    lock_location_is_missing(location, &location.lock_file)
                }
                _ => could_not_be_taken(&location.lock_file, failure),
            })?;
        let opened_metadata = lock_file
            .metadata()
            .map_err(|failure| could_not_be_taken(&location.lock_file, failure))?;
        refuse_a_lock_file_that_cannot_be_trusted(
            &location.lock_file,
            LockPathMetadata::from_unfollowed_metadata(&opened_metadata),
            location.expected_owner_uid,
        )?;
        Ok(lock_file)
    }

    fn whole_file_write_lock_request() -> libc::flock {
        // SAFETY: `flock` is plain data; all-zero is a valid value.
        let mut request: libc::flock = unsafe { std::mem::zeroed() };
        request.l_type = libc::F_WRLCK as libc::c_short;
        request.l_whence = libc::SEEK_SET as libc::c_short;
        request.l_start = 0;
        request.l_len = 0;
        request
    }

    /// The pid of the process whose lock conflicts with a whole-file write
    /// lock, or `None` when nothing does any more.
    fn pid_of_the_conflicting_lock_holder(lock_file: &File) -> std::io::Result<Option<i32>> {
        let mut request = whole_file_write_lock_request();
        // SAFETY: `request` is a live `flock` the call reads and overwrites.
        let status = unsafe { libc::fcntl(lock_file.as_raw_fd(), libc::F_GETLK, &mut request) };
        if status != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((request.l_type != libc::F_UNLCK as libc::c_short).then_some(request.l_pid))
    }

    pub(super) fn take(
        location: &MachineRuntimeLockLocation,
    ) -> Result<LockedMachineRuntimeLockFile, MachineRuntimeLockRefusal> {
        if THIS_PROCESS_HOLDS_THE_MACHINE_RUNTIME_LOCK.load(Ordering::SeqCst) {
            return Err(MachineRuntimeLockRefusal::HeldByAnotherRuntime {
                holder: holder_named_by_pid(&location.lock_file, std::process::id() as i32)?,
            });
        }
        let lock_file = open_the_trusted_lock_file(location)?;
        // A second attempt covers a holder that exited between the refused lock and the query.
        for _attempt in 0..2 {
            let request = whole_file_write_lock_request();
            // SAFETY: `request` is a live `flock` the call only reads.
            let status = unsafe { libc::fcntl(lock_file.as_raw_fd(), libc::F_SETLK, &request) };
            if status == 0 {
                THIS_PROCESS_HOLDS_THE_MACHINE_RUNTIME_LOCK.store(true, Ordering::SeqCst);
                return Ok(LockedMachineRuntimeLockFile {
                    _lock_file: lock_file,
                });
            }
            let failure = std::io::Error::last_os_error();
            if !matches!(failure.raw_os_error(), Some(libc::EAGAIN | libc::EACCES)) {
                return Err(could_not_be_taken(&location.lock_file, failure));
            }
            match pid_of_the_conflicting_lock_holder(&lock_file)
                .map_err(|failure| could_not_be_taken(&location.lock_file, failure))?
            {
                Some(holder_pid) => {
                    return Err(MachineRuntimeLockRefusal::HeldByAnotherRuntime {
                        holder: holder_named_by_pid(&location.lock_file, holder_pid)?,
                    });
                }
                None => continue,
            }
        }
        Err(MachineRuntimeLockRefusal::HeldByAProcessThatDoesNotAnswer {
            lock: displayed_lock_name(&location.lock_file),
        })
    }

    pub(super) fn holder(
        location: &MachineRuntimeLockLocation,
    ) -> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal> {
        if THIS_PROCESS_HOLDS_THE_MACHINE_RUNTIME_LOCK.load(Ordering::SeqCst) {
            return holder_named_by_pid(&location.lock_file, std::process::id() as i32).map(Some);
        }
        let lock_file = match open_the_trusted_lock_file(location) {
            Ok(lock_file) => lock_file,
            Err(MachineRuntimeLockRefusal::LockLocationIsMissing { .. }) => return Ok(None),
            Err(refusal) => return Err(refusal),
        };
        match pid_of_the_conflicting_lock_holder(&lock_file)
            .map_err(|failure| could_not_be_taken(&location.lock_file, failure))?
        {
            Some(holder_pid) => holder_named_by_pid(&location.lock_file, holder_pid).map(Some),
            None => Ok(None),
        }
    }

    /// Name a pid through libproc (`<libproc.h>`): its uid from its BSD info,
    /// its executable from its path.
    fn holder_named_by_pid(
        lock_file: &Path,
        pid: i32,
    ) -> Result<MachineRuntimeLockHolder, MachineRuntimeLockRefusal> {
        // SAFETY: `proc_bsdinfo` is plain data; all-zero is a valid value.
        let mut bsd_info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let bsd_info_size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: the buffer is a live `proc_bsdinfo` of exactly the size passed.
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                (&mut bsd_info as *mut libc::proc_bsdinfo).cast::<libc::c_void>(),
                bsd_info_size,
            )
        };
        if written != bsd_info_size {
            return Err(MachineRuntimeLockRefusal::HeldByAProcessThatCannotBeNamed {
                lock: displayed_lock_name(lock_file),
                pid,
                source: std::io::Error::last_os_error(),
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
            user_name: user_name_of_uid(bsd_info.pbi_uid),
            uid: bsd_info.pbi_uid,
            pid,
            executable,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::lock_location_trust_check::{
        LockPathEntryKind, LockPathMetadata, refuse_a_lock_directory_that_cannot_be_trusted,
        refuse_a_lock_file_that_cannot_be_trusted, the_commands_that_create_the_lock_location,
    };
    use super::*;
    use std::path::Path;

    const ROOT_UID: u32 = 0;

    fn refusal_text(outcome: Result<(), MachineRuntimeLockRefusal>) -> String {
        match outcome {
            Ok(()) => panic!("the check must refuse"),
            Err(refusal) => refusal.to_string(),
        }
    }

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

    #[test]
    fn a_root_owned_owner_only_writable_directory_and_a_root_owned_0666_file_are_trusted() {
        let directory = Path::new(MACHINE_RUNTIME_LOCK_DIRECTORY_ON_MACOS);
        for mode in [0o755, 0o700, 0o1755] {
            refuse_a_lock_directory_that_cannot_be_trusted(
                directory,
                LockPathMetadata {
                    kind: LockPathEntryKind::Directory,
                    owner_uid: ROOT_UID,
                    mode,
                },
                ROOT_UID,
            )
            .unwrap();
        }
        refuse_a_lock_file_that_cannot_be_trusted(
            &directory.join(MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS),
            LockPathMetadata {
                kind: LockPathEntryKind::RegularFile,
                owner_uid: ROOT_UID,
                mode: 0o666,
            },
            ROOT_UID,
        )
        .unwrap();
    }

    #[test]
    fn a_lock_directory_of_the_wrong_type_owner_or_mode_is_refused_by_name() {
        let directory = Path::new(MACHINE_RUNTIME_LOCK_DIRECTORY_ON_MACOS);
        let trusted = LockPathMetadata {
            kind: LockPathEntryKind::Directory,
            owner_uid: ROOT_UID,
            mode: 0o755,
        };
        for (metadata, property, found, required) in [
            (
                LockPathMetadata {
                    kind: LockPathEntryKind::Symlink,
                    ..trusted
                },
                "type",
                "a symlink",
                "a real directory",
            ),
            (
                LockPathMetadata {
                    kind: LockPathEntryKind::RegularFile,
                    ..trusted
                },
                "type",
                "a regular file",
                "a real directory",
            ),
            (
                LockPathMetadata {
                    owner_uid: 501,
                    ..trusted
                },
                "owner",
                "uid 501",
                "uid 0 (root)",
            ),
            (
                LockPathMetadata {
                    mode: 0o775,
                    ..trusted
                },
                "mode",
                "0775",
                "no group or other write bit",
            ),
            (
                LockPathMetadata {
                    mode: 0o757,
                    ..trusted
                },
                "mode",
                "0757",
                "no group or other write bit",
            ),
            (
                LockPathMetadata {
                    mode: 0o1777,
                    ..trusted
                },
                "mode",
                "1777",
                "no group or other write bit",
            ),
        ] {
            let refusal = refusal_text(refuse_a_lock_directory_that_cannot_be_trusted(
                directory, metadata, ROOT_UID,
            ));
            assert!(
                refusal.contains(MACHINE_RUNTIME_LOCK_DIRECTORY_ON_MACOS),
                "{refusal}"
            );
            assert!(refusal.contains(&format!("the {property} of")), "{refusal}");
            assert!(refusal.contains(&format!("is {found}")), "{refusal}");
            assert!(refusal.contains(required), "{refusal}");
        }
    }

    #[test]
    fn a_lock_file_of_the_wrong_type_owner_or_mode_is_refused_by_name() {
        let lock_file = Path::new(MACHINE_RUNTIME_LOCK_DIRECTORY_ON_MACOS)
            .join(MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS);
        let trusted = LockPathMetadata {
            kind: LockPathEntryKind::RegularFile,
            owner_uid: ROOT_UID,
            mode: 0o666,
        };
        for (metadata, property, found, required) in [
            (
                LockPathMetadata {
                    kind: LockPathEntryKind::Symlink,
                    ..trusted
                },
                "type",
                "a symlink",
                "a regular file",
            ),
            (
                LockPathMetadata {
                    kind: LockPathEntryKind::Directory,
                    ..trusted
                },
                "type",
                "a directory",
                "a regular file",
            ),
            (
                LockPathMetadata {
                    kind: LockPathEntryKind::OtherEntry,
                    ..trusted
                },
                "type",
                "neither a regular file nor a directory",
                "a regular file",
            ),
            (
                LockPathMetadata {
                    owner_uid: 501,
                    ..trusted
                },
                "owner",
                "uid 501",
                "uid 0 (root)",
            ),
            (
                LockPathMetadata {
                    mode: 0o644,
                    ..trusted
                },
                "mode",
                "0644",
                "exactly 0666",
            ),
            (
                LockPathMetadata {
                    mode: 0o600,
                    ..trusted
                },
                "mode",
                "0600",
                "exactly 0666",
            ),
            (
                LockPathMetadata {
                    mode: 0o777,
                    ..trusted
                },
                "mode",
                "0777",
                "exactly 0666",
            ),
            (
                LockPathMetadata {
                    mode: 0o4666,
                    ..trusted
                },
                "mode",
                "4666",
                "exactly 0666",
            ),
        ] {
            let refusal = refusal_text(refuse_a_lock_file_that_cannot_be_trusted(
                &lock_file, metadata, ROOT_UID,
            ));
            assert!(
                refusal.contains(&lock_file.display().to_string()),
                "{refusal}"
            );
            assert!(refusal.contains(&format!("the {property} of")), "{refusal}");
            assert!(refusal.contains(&format!("is {found}")), "{refusal}");
            assert!(refusal.contains(required), "{refusal}");
        }
    }

    #[test]
    fn a_test_roots_lock_is_trusted_owned_by_the_current_uid_and_refused_owned_by_root() {
        let this_uid = crate::streamlib_runtime_directory::current_process_uid();
        let lock_file = Path::new("/tmp/tl-abcd/lock/runtime.lock");
        let owned_by_this_uid = LockPathMetadata {
            kind: LockPathEntryKind::RegularFile,
            owner_uid: this_uid,
            mode: 0o666,
        };
        refuse_a_lock_file_that_cannot_be_trusted(lock_file, owned_by_this_uid, this_uid).unwrap();
        if this_uid != ROOT_UID {
            let refusal = refusal_text(refuse_a_lock_file_that_cannot_be_trusted(
                lock_file,
                LockPathMetadata {
                    owner_uid: ROOT_UID,
                    ..owned_by_this_uid
                },
                this_uid,
            ));
            assert!(
                refusal.contains(&format!("is uid 0 (root), and it must be uid {this_uid}")),
                "{refusal}"
            );
        }
    }

    #[test]
    fn metadata_read_without_following_a_symlink_feeds_the_checks_what_is_on_disk() {
        use std::os::unix::fs::PermissionsExt;
        let this_uid = crate::streamlib_runtime_directory::current_process_uid();
        let scratch_directory =
            crate::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
        let lock_directory = scratch_directory.path().join("lock");
        std::fs::create_dir(&lock_directory).unwrap();
        std::fs::set_permissions(&lock_directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let lock_file = lock_directory.join(MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS);
        std::fs::write(&lock_file, b"").unwrap();
        std::fs::set_permissions(&lock_file, std::fs::Permissions::from_mode(0o666)).unwrap();
        let symlink_to_the_lock_file = lock_directory.join("a-symlink");
        std::os::unix::fs::symlink(&lock_file, &symlink_to_the_lock_file).unwrap();
        let unfollowed = |path: &Path| {
            LockPathMetadata::from_unfollowed_metadata(&std::fs::symlink_metadata(path).unwrap())
        };

        refuse_a_lock_directory_that_cannot_be_trusted(
            &lock_directory,
            unfollowed(&lock_directory),
            this_uid,
        )
        .unwrap();
        refuse_a_lock_file_that_cannot_be_trusted(&lock_file, unfollowed(&lock_file), this_uid)
            .unwrap();
        let refusal = refusal_text(refuse_a_lock_file_that_cannot_be_trusted(
            &symlink_to_the_lock_file,
            unfollowed(&symlink_to_the_lock_file),
            this_uid,
        ));
        assert!(refusal.contains("the type of"), "{refusal}");
        assert!(refusal.contains("is a symlink"), "{refusal}");
    }

    #[test]
    fn the_missing_lock_refusal_names_the_exact_commands_that_create_it() {
        let lock_directory = Path::new(MACHINE_RUNTIME_LOCK_DIRECTORY_ON_MACOS);
        let lock_file = lock_directory.join(MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS);
        let refusal = MachineRuntimeLockRefusal::LockLocationIsMissing {
            missing_path: lock_directory.to_path_buf(),
            creation_commands: the_commands_that_create_the_lock_location(
                lock_directory,
                &lock_file,
                ROOT_UID,
            ),
        }
        .to_string();
        assert!(
            refusal.contains(
                "sudo mkdir -p \"/Library/Application Support/Tatolab\" && sudo touch \
                 \"/Library/Application Support/Tatolab/runtime.lock\" && sudo chmod 0666 \
                 \"/Library/Application Support/Tatolab/runtime.lock\""
            ),
            "{refusal}"
        );
        assert!(
            refusal.contains("Tatolab.app creates it at first launch"),
            "{refusal}"
        );
        assert!(
            refusal.contains("/Library/Application Support/Tatolab does not exist"),
            "{refusal}"
        );
    }

    /// The tests that take and probe a real lock at a location no real runtime
    /// and no other test uses. They run one at a time because a macOS process
    /// holds at most one machine runtime lock, wherever it lives.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod taken_and_probed_on_this_floor {
        use super::*;
        use crate::test_support::{
            assert_the_child_process_ran_the_test, rerun_this_test_in_a_child_process,
            spawn_this_test_from_a_test_binary,
        };
        use std::ffi::{OsStr, OsString};
        use std::io::Read;
        use std::time::{Duration, Instant};

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

        /// A lock location only one test uses, removed with it.
        struct LockLocationOnlyThisTestUses {
            location: MachineRuntimeLockLocation,
            #[cfg(target_os = "macos")]
            _scratch_directory: tempfile::TempDir,
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

            /// A `lock/` directory at 0755 holding `runtime.lock` at 0666, both
            /// owned by this uid, as a test root's harness lays them out.
            #[cfg(target_os = "macos")]
            fn new() -> Self {
                use std::os::unix::fs::PermissionsExt;
                let scratch_directory =
                    crate::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
                let lock_directory = a_lock_directory_at_0755_in(scratch_directory.path());
                let lock_file = lock_directory.join(MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS);
                std::fs::write(&lock_file, b"").unwrap();
                std::fs::set_permissions(&lock_file, std::fs::Permissions::from_mode(0o666))
                    .unwrap();
                Self {
                    location: lock_location_in(lock_directory),
                    _scratch_directory: scratch_directory,
                }
            }

            /// What a child process is handed to find this location again.
            fn handed_to_a_child(&self) -> &OsStr {
                #[cfg(target_os = "linux")]
                {
                    OsStr::new(&self.location.abstract_socket_name)
                }
                #[cfg(target_os = "macos")]
                {
                    self.location.lock_directory.as_os_str()
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
                lock_location_in(PathBuf::from(handed))
            }
        }

        #[cfg(target_os = "macos")]
        fn lock_location_in(lock_directory: PathBuf) -> MachineRuntimeLockLocation {
            MachineRuntimeLockLocation {
                lock_file: lock_directory.join(MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS),
                lock_directory,
                expected_owner_uid: crate::streamlib_runtime_directory::current_process_uid(),
            }
        }

        #[cfg(target_os = "macos")]
        fn a_lock_directory_at_0755_in(scratch_directory: &Path) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let lock_directory = scratch_directory.join("lock");
            std::fs::create_dir(&lock_directory).unwrap();
            std::fs::set_permissions(&lock_directory, std::fs::Permissions::from_mode(0o755))
                .unwrap();
            lock_directory
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
                let _held_lock = take_the_machine_runtime_lock_at(&location).unwrap();
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
            let _held_lock = take_the_machine_runtime_lock_at(&lock.location).unwrap();

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
            let _held_lock = take_the_machine_runtime_lock_at(&lock.location).unwrap();
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
                lock.handed_to_a_child(),
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

            let held_lock = take_the_machine_runtime_lock_at(&lock.location).unwrap();
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
        fn a_symlinked_lock_file_is_refused_as_a_symlink_by_the_take_and_the_probe() {
            use std::os::unix::fs::PermissionsExt;
            let scratch_directory =
                crate::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
            let lock_directory = a_lock_directory_at_0755_in(scratch_directory.path());
            let a_file_elsewhere = scratch_directory.path().join("a-file-elsewhere");
            std::fs::write(&a_file_elsewhere, b"").unwrap();
            std::fs::set_permissions(&a_file_elsewhere, std::fs::Permissions::from_mode(0o666))
                .unwrap();
            std::os::unix::fs::symlink(
                &a_file_elsewhere,
                lock_directory.join(MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS),
            )
            .unwrap();
            let location = lock_location_in(lock_directory);

            let refusals = [
                take_the_machine_runtime_lock_at(&location).unwrap_err(),
                holder_of_the_machine_runtime_lock_at(&location).unwrap_err(),
            ];

            for refusal in refusals {
                let MachineRuntimeLockRefusal::LockLocationCannotBeTrusted {
                    path,
                    property,
                    found,
                    ..
                } = &refusal
                else {
                    panic!("a symlinked lock file must be refused as untrusted: {refusal}");
                };
                assert_eq!(path, &location.lock_file);
                assert_eq!(*property, "type");
                assert_eq!(found, "a symlink");
            }
        }

        #[cfg(target_os = "macos")]
        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn a_missing_lock_location_is_refused_naming_the_commands_that_create_it_and_probes_as_free()
         {
            let scratch_directory =
                crate::test_support::a_temporary_directory_at_owner_only_mode().unwrap();
            let without_its_file =
                lock_location_in(a_lock_directory_at_0755_in(scratch_directory.path()));
            let without_its_directory =
                lock_location_in(scratch_directory.path().join("never-made"));

            for (location, missing_path) in [
                (&without_its_file, &without_its_file.lock_file),
                (
                    &without_its_directory,
                    &without_its_directory.lock_directory,
                ),
            ] {
                let refusal = take_the_machine_runtime_lock_at(location).unwrap_err();
                let MachineRuntimeLockRefusal::LockLocationIsMissing {
                    missing_path: refused_missing_path,
                    creation_commands,
                } = &refusal
                else {
                    panic!("a missing lock location must be refused as missing: {refusal}");
                };
                assert_eq!(refused_missing_path, missing_path);
                assert!(
                    creation_commands.contains(&format!(
                        "mkdir -p \"{}\"",
                        location.lock_directory.display()
                    )),
                    "{creation_commands}"
                );
                assert!(
                    creation_commands
                        .contains(&format!("chmod 0666 \"{}\"", location.lock_file.display())),
                    "{creation_commands}"
                );
                assert_eq!(
                    holder_of_the_machine_runtime_lock_at(location).unwrap(),
                    None
                );
            }
        }

        #[cfg(target_os = "macos")]
        #[test]
        #[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
        fn a_lock_file_at_any_mode_but_0666_is_refused_naming_its_mode() {
            use std::os::unix::fs::PermissionsExt;
            let lock = LockLocationOnlyThisTestUses::new();
            for mode in [0o644, 0o600, 0o777] {
                std::fs::set_permissions(
                    &lock.location.lock_file,
                    std::fs::Permissions::from_mode(mode),
                )
                .unwrap();

                let refusal = take_the_machine_runtime_lock_at(&lock.location).unwrap_err();

                let MachineRuntimeLockRefusal::LockLocationCannotBeTrusted {
                    path,
                    property,
                    found,
                    ..
                } = &refusal
                else {
                    panic!("a lock file at mode {mode:04o} must be refused: {refusal}");
                };
                assert_eq!(path, &lock.location.lock_file);
                assert_eq!(*property, "mode");
                assert_eq!(found, &format!("{mode:04o}"));
            }
        }
    }

    /// The test build's root, read from the environment, so each arm runs in a
    /// child process whose environment it owns.
    #[cfg(feature = "machine-directories-under-a-test-root")]
    mod under_a_test_machine_root {
        use super::*;
        use crate::machine_directories_test_root::TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE;
        use crate::streamlib_runtime_directory::StreamlibRuntimeDirectory;
        use crate::tatolab_state_directory::TatolabStateDirectory;
        use std::os::unix::fs::PermissionsExt;

        /// Set only in the child process the unset-root test re-runs itself in.
        const UNSET_TEST_MACHINE_ROOT_CHILD_ENVIRONMENT_VARIABLE: &str =
            "STREAMLIB_TEST_UNSET_TEST_MACHINE_ROOT_CHILD";

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

                #[cfg(target_os = "macos")]
                {
                    let lock_directory = test_machine_root.join("lock");
                    std::fs::create_dir(&lock_directory).unwrap();
                    std::fs::set_permissions(
                        &lock_directory,
                        std::fs::Permissions::from_mode(0o755),
                    )
                    .unwrap();
                    let lock_file = lock_directory.join(MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS);
                    std::fs::write(&lock_file, b"").unwrap();
                    std::fs::set_permissions(&lock_file, std::fs::Permissions::from_mode(0o666))
                        .unwrap();
                }
                assert_eq!(holder_of_the_machine_runtime_lock().unwrap(), None);
                let _held_lock = MachineRuntimeLock::take().unwrap();
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
                return;
            }

            // Short and under /tmp whatever TMPDIR says: socket paths under it stay below 104 bytes.
            let test_machine_root = tempfile::Builder::new()
                .prefix("tl-")
                .tempdir_in("/tmp")
                .unwrap();
            let child = crate::test_support::rerun_this_test_in_a_child_process(
                "machine_runtime_lock::tests::under_a_test_machine_root::the_test_root_moves_the_runtime_directory_the_state_directory_and_the_lock",
                TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE,
                test_machine_root.path().as_os_str(),
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
    }
}
