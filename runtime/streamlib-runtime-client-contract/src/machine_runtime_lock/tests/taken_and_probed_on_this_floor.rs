// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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

/// A loopback port free when chosen, below the 49152–65535 range macOS draws a
/// port-0 bind from: a test probes the lock while it is free, and a listener
/// another test binds to port 0 meanwhile could otherwise land on it.
#[cfg(target_os = "macos")]
fn a_free_loopback_port_below_the_ephemeral_range() -> u16 {
    use std::sync::atomic::{AtomicU32, Ordering};
    const FIRST_CANDIDATE_PORT: u32 = 20_000;
    const CANDIDATE_PORT_COUNT: u32 = 20_000;
    static NEXT_CANDIDATE_OFFSET: AtomicU32 = AtomicU32::new(0);
    let offset_of_this_process = std::process::id().wrapping_mul(97);
    for _candidate in 0..CANDIDATE_PORT_COUNT {
        let candidate_offset = offset_of_this_process
            .wrapping_add(NEXT_CANDIDATE_OFFSET.fetch_add(1, Ordering::SeqCst))
            % CANDIDATE_PORT_COUNT;
        let candidate_port = (FIRST_CANDIDATE_PORT + candidate_offset) as u16;
        if TcpListener::bind((Ipv4Addr::LOCALHOST, candidate_port)).is_ok() {
            return candidate_port;
        }
    }
    panic!("no loopback port below the ephemeral range is free");
}

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
                    a_free_loopback_port_below_the_ephemeral_range(),
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
        _holder_route_answered: TestThreadAnsweringEveryConnection::answering_this_processes_pid(
            held_lock.take_the_bound_loopback_http_listener().unwrap(),
        ),
        _held_lock: held_lock,
    }
}

/// The holder with its executable canonicalized, since `/proc/<pid>/exe`
/// and `proc_pidpath` name the file with every symlink resolved.
fn with_its_executable_canonicalized(holder: MachineRuntimeLockHolder) -> MachineRuntimeLockHolder {
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
    a_holder_running_as_this_uid(std::process::id() as i32, &std::env::current_exe().unwrap())
}

fn parent_process_id() -> i32 {
    // SAFETY: getppid takes no arguments and cannot fail.
    unsafe { libc::getppid() }
}

#[test]
#[serial_test::serial(machine_runtime_lock_taken_in_this_process)]
fn a_take_while_another_executable_holds_the_lock_is_refused_naming_its_uid_pid_and_executable() {
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
    let expected_holder = a_holder_running_as_this_uid(holding_child.id() as i32, &holder_binary);

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
                if failure.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < deadline =>
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
    if let Some(handed) = std::env::var_os(PROBE_THE_PARENTS_LOCK_CHILD_ENVIRONMENT_VARIABLE) {
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
    mut probe: impl FnMut() -> Result<Option<MachineRuntimeLockHolder>, MachineRuntimeLockRefusal>,
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
