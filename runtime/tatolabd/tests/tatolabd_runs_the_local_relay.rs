// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd` running the machine's local relay: both `moq-relay` children up
//! as it serves, each answering its own certificate's fingerprint; both ended
//! with the runtime, whether it stops cleanly or is killed; a unit without the
//! relay runs without one, saying why. GPU-free. Built with
//! `--features machine-directories-under-a-test-root`, and needing
//! `cargo xtask stage-local-relay`.

#![cfg(feature = "machine-directories-under-a-test-root")]

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{
    LocalRelayPortsOfATest, SpawnedTatolabd, TatolabTestMachineRoot, TemporaryRuntimeUnit,
    every_file_under, http_get_body_on_loopback, the_port_is_free_for_udp_and_tcp_on,
    the_relay_listed, the_relays_listed,
};

/// How long both children have to answer once `tatolabd` serves, and to be
/// gone once it is not.
const LOCAL_RELAY_BUDGET: Duration = Duration::from_secs(20);

const LOOPBACK_RELAY_ID: &str = "local-loopback";
const EVERY_INTERFACE_RELAY_ID: &str = "local-every-interface";

fn wait_until(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    condition()
}

/// The fingerprint `port` serves once it answers one.
fn the_fingerprint_served_on(port: u16) -> String {
    let mut served = None;
    assert!(
        wait_until(LOCAL_RELAY_BUDGET, || {
            served = http_get_body_on_loopback(port, "/certificate.sha256");
            served.is_some()
        }),
        "nothing answered /certificate.sha256 on port {port} within {LOCAL_RELAY_BUDGET:?}"
    );
    let served = served.unwrap();
    assert!(
        served.len() == 64 && served.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "port {port} served {served:?}, not a SHA-256 in hex"
    );
    served
}

/// The process id in `instance_directory`'s pidfile, once it is written.
fn the_child_named_by_the_pidfile_in(instance_directory: &Path) -> u32 {
    let pidfile = instance_directory.join("moq-relay.pid");
    let mut process_id = None;
    assert!(
        wait_until(LOCAL_RELAY_BUDGET, || {
            process_id = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|text| text.trim().parse().ok());
            process_id.is_some()
        }),
        "no pidfile at {}",
        pidfile.display()
    );
    process_id.unwrap()
}

fn the_process_exists(process_id: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; it only asks whether the process exists.
    unsafe { libc::kill(process_id as libc::pid_t, 0) == 0 }
}

fn both_ports_are_free(ports: LocalRelayPortsOfATest) -> bool {
    the_port_is_free_for_udp_and_tcp_on("127.0.0.1", ports.loopback)
        && the_port_is_free_for_udp_and_tcp_on("[::]", ports.every_interface)
}

fn spawn_tatolabd_with_its_local_relay(
    machine_root: &TatolabTestMachineRoot,
    runtime_unit: &TemporaryRuntimeUnit,
    ports: LocalRelayPortsOfATest,
) -> SpawnedTatolabd {
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    ports.given_to(&mut tatolabd_command);
    SpawnedTatolabd::spawn_and_wait_until_serving(tatolabd_command)
}

/// The publisher dials each listener with the token in the URL's query, and
/// the stack's WebTransport and WebSocket clients print that URL whole below
/// `warn`; at `RUST_LOG=trace` it still reaches no log line.
#[test]
fn each_listener_is_listed_joined_once_the_publishers_session_connects_and_its_token_is_never_logged()
 {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend_and_the_local_relay();
    let machine_root = TatolabTestMachineRoot::new();
    let ports = LocalRelayPortsOfATest::free_now();
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    ports.given_to(&mut tatolabd_command);
    tatolabd_command.env("RUST_LOG", "trace");
    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(tatolabd_command);

    for relay_id in [LOOPBACK_RELAY_ID, EVERY_INTERFACE_RELAY_ID] {
        let mut listed = serde_json::Value::Null;
        assert!(
            wait_until(LOCAL_RELAY_BUDGET, || {
                listed = the_relay_listed(&the_relays_listed(&machine_root), relay_id)
                    ["connection_state"]
                    .clone();
                listed == serde_json::json!({"state": "joined"})
            }),
            "`{relay_id}` is listed {listed} {LOCAL_RELAY_BUDGET:?} after the runtime serves"
        );
    }
    runtime.interrupt_and_expect_a_clean_exit();

    let standard_error = runtime.standard_error();
    assert!(
        standard_error.contains("the publisher's session with the relay is connected"),
        "{standard_error}"
    );
    let runtime_log = every_file_under(&machine_root.runtime_log_directory());
    assert!(!runtime_log.is_empty(), "the runtime's own log is read");
    for (shown_in, text) in [
        ("the standard error", standard_error),
        ("the runtime log", runtime_log),
    ] {
        assert!(
            !text.contains("jwt="),
            "a dial token is in {shown_in}:\n{text}"
        );
    }
}

#[test]
fn tatolabd_starts_both_listeners_each_answering_its_own_fingerprint_and_ends_them_with_itself() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend_and_the_local_relay();
    let machine_root = TatolabTestMachineRoot::new();
    let ports = LocalRelayPortsOfATest::free_now();
    let mut runtime = spawn_tatolabd_with_its_local_relay(&machine_root, &runtime_unit, ports);

    let loopback_fingerprint = the_fingerprint_served_on(ports.loopback);
    let every_interface_fingerprint = the_fingerprint_served_on(ports.every_interface);
    assert_ne!(
        loopback_fingerprint, every_interface_fingerprint,
        "each listener serves a certificate of its own"
    );
    let children = [LOOPBACK_RELAY_ID, EVERY_INTERFACE_RELAY_ID].map(|relay_id| {
        the_child_named_by_the_pidfile_in(&machine_root.local_relay_instance_directory(relay_id))
    });

    runtime.interrupt_and_expect_a_clean_exit();
    for child in children {
        assert!(
            !the_process_exists(child),
            "child {child} outlived tatolabd"
        );
    }
    assert!(
        both_ports_are_free(ports),
        "a port is still bound: {ports:?}"
    );
    for relay_id in [LOOPBACK_RELAY_ID, EVERY_INTERFACE_RELAY_ID] {
        let instance_directory = machine_root.local_relay_instance_directory(relay_id);
        assert!(!instance_directory.join("moq-relay.pid").exists());
        assert!(!instance_directory.join("admission.sock").exists());
    }
    let standard_error = runtime.standard_error();
    assert!(
        standard_error.contains(&format!("listens at http://127.0.0.1:{}/", ports.loopback)),
        "{standard_error}"
    );
}

#[test]
fn killing_tatolabd_leaves_no_child_and_no_bound_port_behind() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend_and_the_local_relay();
    let machine_root = TatolabTestMachineRoot::new();
    let ports = LocalRelayPortsOfATest::free_now();
    let mut runtime = spawn_tatolabd_with_its_local_relay(&machine_root, &runtime_unit, ports);
    the_fingerprint_served_on(ports.loopback);
    the_fingerprint_served_on(ports.every_interface);
    let children = [LOOPBACK_RELAY_ID, EVERY_INTERFACE_RELAY_ID].map(|relay_id| {
        the_child_named_by_the_pidfile_in(&machine_root.local_relay_instance_directory(relay_id))
    });

    runtime.deliver(libc::SIGKILL);
    runtime.wait_for_exit_within(LOCAL_RELAY_BUDGET);

    #[cfg(target_os = "linux")]
    {
        assert!(
            wait_until(LOCAL_RELAY_BUDGET, || children
                .iter()
                .all(|child| !the_process_exists(*child))),
            "a child outlived the killed tatolabd: {children:?}"
        );
        assert!(
            wait_until(LOCAL_RELAY_BUDGET, || both_ports_are_free(ports)),
            "a port is still bound: {ports:?}"
        );
    }

    let mut restarted = spawn_tatolabd_with_its_local_relay(&machine_root, &runtime_unit, ports);
    the_fingerprint_served_on(ports.loopback);
    the_fingerprint_served_on(ports.every_interface);
    for child in children {
        assert!(
            !the_process_exists(child),
            "the killed runtime's child {child} still runs after the next start"
        );
    }
    restarted.interrupt_and_expect_a_clean_exit();
    assert!(
        both_ports_are_free(ports),
        "a port is still bound: {ports:?}"
    );
}

#[test]
fn a_test_runtime_naming_no_relay_ports_runs_no_local_relay_saying_why() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend_and_the_local_relay();
    let machine_root = TatolabTestMachineRoot::new();
    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );

    assert!(
        !machine_root
            .local_relay_instance_directory(LOOPBACK_RELAY_ID)
            .exists()
    );
    runtime.interrupt_and_expect_a_clean_exit();
    let standard_error = runtime.standard_error();
    assert!(
        standard_error.contains("no local relay runs")
            && standard_error.contains("never binds the machine's own"),
        "{standard_error}"
    );
}

#[test]
fn a_unit_without_the_relay_runs_with_no_local_relay_saying_why() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let ports = LocalRelayPortsOfATest::free_now();
    let mut runtime = spawn_tatolabd_with_its_local_relay(&machine_root, &runtime_unit, ports);

    assert_eq!(
        http_get_body_on_loopback(ports.loopback, "/certificate.sha256"),
        None
    );
    runtime.interrupt_and_expect_a_clean_exit();
    let standard_error = runtime.standard_error();
    assert!(
        standard_error.contains("no local relay runs")
            && standard_error.contains("moq-relay")
            && standard_error.contains("cargo xtask build-runtime"),
        "{standard_error}"
    );
}
