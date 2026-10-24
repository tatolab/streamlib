// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd` serving its loopback HTTP beside its local API: the listing of
//! the machine's streams at `/`, and the preview page staged beside its lend at
//! `/watch`. A test build binds the port its test names; on macOS that bind is
//! the machine runtime lock. Built with
//! `--features machine-directories-under-a-test-root`.

#![cfg(feature = "machine-directories-under-a-test-root")]

mod common;

use common::{
    SpawnedTatolabd, TatolabTestMachineRoot, TemporaryRuntimeUnit, a_port_free_for_udp_and_tcp_on,
    http_get_body_on_loopback,
};
use streamlib::sdk::processor_interpreter::{
    lend_directory_in_the_runtime_unit, watch_page_directory_beside_the_lend,
};
use streamlib_runtime_client_contract::machine_directories_test_root::TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE;

/// The preview page's entry file a test stages beside the unit's lend.
const STAGED_WATCH_PAGE: &str = "<!doctype html><title>watch</title>";

#[test]
fn the_runtime_lists_its_streams_and_serves_the_page_staged_beside_its_lend() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let watch_page_directory = watch_page_directory_beside_the_lend(
        &lend_directory_in_the_runtime_unit(runtime_unit.runtime_unit_root()),
    )
    .expect("the lend has a directory beside it");
    std::fs::create_dir_all(&watch_page_directory).unwrap();
    std::fs::write(watch_page_directory.join("index.html"), STAGED_WATCH_PAGE).unwrap();
    let machine_root = TatolabTestMachineRoot::new();
    let loopback_http_port = a_port_free_for_udp_and_tcp_on("127.0.0.1");
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    tatolabd_command.env(
        TEST_LOOPBACK_HTTP_PORT_ENVIRONMENT_VARIABLE,
        loopback_http_port.to_string(),
    );

    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(tatolabd_command);
    let listing = http_get_body_on_loopback(loopback_http_port, "/");
    let watch_page = http_get_body_on_loopback(loopback_http_port, "/watch/desk/camera");
    runtime.interrupt_and_expect_a_clean_exit();

    let listing: serde_json::Value =
        serde_json::from_str(&listing.expect("`/` answers 200")).unwrap();
    assert!(listing["machine"].is_string(), "{listing}");
    assert_eq!(listing["streams"], serde_json::json!([]));
    assert_eq!(watch_page.as_deref(), Some(STAGED_WATCH_PAGE));
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
