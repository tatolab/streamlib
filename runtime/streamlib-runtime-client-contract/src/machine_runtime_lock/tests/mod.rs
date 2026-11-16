// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
#[cfg(feature = "machine-directories-under-a-test-root")]
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

    fn what_a_probe_reads_off_a_stub_answering(answer: Vec<u8>) -> LoopbackHttpListenerPidAnswer {
        let (listener, address) = a_listener_on_a_free_loopback_port();
        let _stub = TestThreadAnsweringEveryConnection::start(listener, answer);
        ask_the_loopback_http_listener_for_its_pid(address, LOOPBACK_HTTP_LISTENER_PID_PROBE_BOUND)
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
mod taken_and_probed_on_this_floor;

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
        if let Some(test_machine_root) = std::env::var_os(TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE) {
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
            #[cfg_attr(
                not(target_os = "macos"),
                allow(unused_mut, reason = "only the macOS build mutates it")
            )]
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
                let held_at_the_production_name = super::super::linux_abstract_socket_lock::holder(
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
        let child = crate::test_support::rerun_this_test_in_a_child_process_with_its_environment(
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

        let child = crate::test_support::rerun_this_test_in_a_child_process_with_its_environment(
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
        if std::env::var_os(UNSET_TEST_LOOPBACK_HTTP_PORT_CHILD_ENVIRONMENT_VARIABLE).is_some() {
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
        let child = crate::test_support::rerun_this_test_in_a_child_process_with_its_environment(
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
