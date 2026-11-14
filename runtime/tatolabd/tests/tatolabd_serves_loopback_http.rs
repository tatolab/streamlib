// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! `tatolabd` serving its loopback HTTP beside its local API: the machine
//! runtime lock holder's pid at `/machine-runtime-lock-holder`, the control
//! WebSocket at `/control/ws`, and nothing else. A test build binds the port its test names; on macOS that bind is the
//! machine runtime lock. Built with
//! `--features machine-directories-under-a-test-root`.

#![cfg(feature = "machine-directories-under-a-test-root")]

mod common;

use common::{
    SpawnedTatolabd, TatolabTestMachineRoot, TemporaryRuntimeUnit, a_port_free_for_udp_and_tcp_on,
    http_get_body_on_loopback, websocket_upgrade_status_line_on_loopback,
};
use streamlib_runtime_client_contract::machine_directories_test_root::TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE;

#[test]
fn the_runtime_answers_its_pid_as_the_lock_holder_opens_its_control_websocket_and_serves_nothing_else()
 {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let loopback_http_port = a_port_free_for_udp_and_tcp_on("127.0.0.1");
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    tatolabd_command.env(
        TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE,
        loopback_http_port.to_string(),
    );

    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(tatolabd_command);
    let runtime_pid = runtime.process_id();
    let lock_holder = http_get_body_on_loopback(loopback_http_port, "/machine-runtime-lock-holder");
    let listing = http_get_body_on_loopback(loopback_http_port, "/");
    let watch_page = http_get_body_on_loopback(loopback_http_port, "/watch/desk/camera");
    let control_websocket_opened_from_a_page_of_any_name =
        websocket_upgrade_status_line_on_loopback(
            loopback_http_port,
            "/control/ws",
            &format!("my-tool.test:{loopback_http_port}"),
            "http://my-tool.test:3000",
        );
    runtime.interrupt_and_expect_a_clean_exit();

    let lock_holder: serde_json::Value =
        serde_json::from_str(&lock_holder.expect("the lock holder route answers 200")).unwrap();
    assert_eq!(lock_holder, serde_json::json!({"pid": runtime_pid}));
    assert_eq!(listing, None);
    assert_eq!(watch_page, None);
    assert_eq!(
        control_websocket_opened_from_a_page_of_any_name.as_deref(),
        Some("HTTP/1.1 101 Switching Protocols")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_test_build_naming_no_port_serves_no_loopback_http() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();

    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );
    runtime.interrupt_and_expect_a_clean_exit();

    assert!(
        runtime
            .standard_error()
            .contains(TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE),
        "the runtime says why it serves no loopback HTTP:\n{}",
        runtime.standard_error()
    );
}

/// On Linux the loopback HTTP is not the lock, so a port another process
/// holds leaves the runtime serving without it, naming the port.
#[cfg(target_os = "linux")]
#[test]
fn a_loopback_http_port_another_process_holds_leaves_the_runtime_serving_without_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let held_loopback_http_port = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let held_port = held_loopback_http_port.local_addr().unwrap().port();
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    tatolabd_command.env(
        TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE,
        held_port.to_string(),
    );

    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(tatolabd_command);
    runtime.interrupt_and_expect_a_clean_exit();

    assert!(
        runtime.standard_error().contains(&format!(
            "the loopback HTTP listener cannot listen on 127.0.0.1:{held_port}"
        )),
        "the runtime names the held port it serves no loopback HTTP on:\n{}",
        runtime.standard_error()
    );
}
