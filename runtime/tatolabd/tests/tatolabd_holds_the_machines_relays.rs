// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd` holding the machine's relays through its local API: the local
//! relay's two instances built in and never removed, a second instance of an
//! adapter added from its settings alone, and a secret kept in the store —
//! across a restart, and out of `relays.json`, `list_relays`, `graph`, the log
//! and the runtime's environment, which every child inherits. GPU-free, and
//! needing no `moq-relay`. Built with
//! `--features machine-directories-under-a-test-root`.

#![cfg(feature = "machine-directories-under-a-test-root")]

mod common;

use serde_json::json;

use common::{
    SpawnedTatolabd, TatolabTestMachineRoot, TemporaryRuntimeUnit, call_a_tool_over_the_local_api,
    the_mode_of,
};

/// The secret every test looks for where it must not be.
const RELAY_SECRET: &str = "relay-secret-5f1c9a7e";

/// The adapter a test build compiles in whose settings hold a secret.
const SECRET_HOLDING_TEST_RELAY_ADAPTER: &str = "test-secret-holder";

fn the_relays_listed(machine_root: &TatolabTestMachineRoot) -> serde_json::Value {
    call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "list_relays",
        json!({}),
    )
    .json()
}

fn the_relay_listed<'listing>(
    listing: &'listing serde_json::Value,
    relay_id: &str,
) -> &'listing serde_json::Value {
    listing["relays"]
        .as_array()
        .unwrap()
        .iter()
        .find(|relay| relay["relay_id"] == relay_id)
        .unwrap_or_else(|| panic!("`{relay_id}` is not listed: {listing}"))
}

/// `process_id`'s environment as the kernel holds it.
#[cfg(target_os = "linux")]
fn the_environment_of(process_id: i32) -> String {
    String::from_utf8_lossy(&std::fs::read(format!("/proc/{process_id}/environ")).unwrap())
        .into_owned()
}

#[test]
fn an_added_relays_secret_is_kept_in_the_store_across_a_restart_and_shown_nowhere() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );

    for label in ["first", "second"] {
        call_a_tool_over_the_local_api(
            &machine_root.local_api_socket_path(),
            "add_relay",
            json!({
                "adapter": SECRET_HOLDING_TEST_RELAY_ADAPTER,
                "settings": {"token": RELAY_SECRET, "label": label}
            }),
        )
        .json();
    }
    let listing = the_relays_listed(&machine_root);
    let second = the_relay_listed(&listing, "test-secret-holder-2");
    assert_eq!(second["settings"], json!({"label": "second"}));
    assert_eq!(second["secrets"], json!({"token": "set"}));
    assert_eq!(second["connection_state"], json!({"state": "joined"}));
    assert!(!listing.to_string().contains(RELAY_SECRET), "{listing}");

    let graph =
        call_a_tool_over_the_local_api(&machine_root.local_api_socket_path(), "graph", json!({}));
    assert!(!graph.text.contains(RELAY_SECRET), "{}", graph.text);
    #[cfg(target_os = "linux")]
    assert!(
        !the_environment_of(runtime.process_id()).contains(RELAY_SECRET),
        "the secret is in the environment every child of the runtime inherits"
    );

    let relays_record_path = machine_root.state_directory().join("relays.json");
    let relays_record = std::fs::read_to_string(&relays_record_path).unwrap();
    assert!(!relays_record.contains(RELAY_SECRET), "{relays_record}");
    assert_eq!(the_mode_of(&relays_record_path), 0o600);
    let fallback_secrets_path = machine_root
        .state_directory()
        .join("relay-credentials.json");
    assert_eq!(the_mode_of(&fallback_secrets_path), 0o600);

    runtime.interrupt_and_expect_a_clean_exit();
    let mut restarted = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );
    let listing = the_relays_listed(&machine_root);
    let first = the_relay_listed(&listing, "test-secret-holder-1");
    assert_eq!(first["secrets"], json!({"token": "set"}));
    assert_eq!(
        first["connection_state"],
        json!({"state": "joined"}),
        "re-joined at the start with the secret read back from the store"
    );
    restarted.interrupt_and_expect_a_clean_exit();

    for standard_error in [runtime.standard_error(), restarted.standard_error()] {
        assert!(!standard_error.contains(RELAY_SECRET), "{standard_error}");
        assert_eq!(
            standard_error.matches("no OS secret store answers").count(),
            1,
            "where the secrets are kept is said once at each start:\n{standard_error}"
        );
    }
}

#[test]
fn the_local_relays_two_instances_are_built_in_never_removed_and_never_added_to() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );

    let listing = the_relays_listed(&machine_root);
    for relay_id in ["local-loopback", "local-every-interface"] {
        let local = the_relay_listed(&listing, relay_id);
        assert_eq!(local["adapter"], "local");
        assert_eq!(local["built_in"], true);
        let removal = call_a_tool_over_the_local_api(
            &machine_root.local_api_socket_path(),
            "remove_relay",
            json!({"relay_id": relay_id}),
        );
        assert!(removal.is_error);
        assert!(
            removal.text.contains(&format!(
                "the relay `{relay_id}` is built in and is never removed"
            )),
            "{}",
            removal.text
        );
    }
    let addition = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "add_relay",
        json!({"adapter": "local"}),
    );
    assert!(addition.is_error);
    assert!(
        addition.text.contains("the local relay is built in"),
        "{}",
        addition.text
    );
    runtime.interrupt_and_expect_a_clean_exit();
}

/// Every file under `directory`, read as text.
fn every_file_under(directory: &std::path::Path) -> String {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return String::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                every_file_under(&path)
            } else {
                String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()).into_owned()
            }
        })
        .collect()
}

#[test]
fn a_relays_secret_reaches_no_log_line_even_where_rust_log_admits_trace() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    tatolabd_command.env("RUST_LOG", "trace");
    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(tatolabd_command);

    call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "add_relay",
        json!({
            "adapter": SECRET_HOLDING_TEST_RELAY_ADAPTER,
            "settings": {"token": RELAY_SECRET}
        }),
    )
    .json();
    call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "configure_relay",
        json!({"relay_id": "test-secret-holder-1", "settings": {"token": RELAY_SECRET}}),
    )
    .json();
    runtime.interrupt_and_expect_a_clean_exit();

    let standard_error = runtime.standard_error();
    assert!(
        standard_error.contains("test-secret-holder-1"),
        "the log is read: {standard_error}"
    );
    assert!(!standard_error.contains(RELAY_SECRET), "{standard_error}");
    let runtime_log = every_file_under(&machine_root.runtime_log_directory());
    assert!(!runtime_log.is_empty(), "the runtime's own log is read");
    assert!(!runtime_log.contains(RELAY_SECRET), "{runtime_log}");
}

/// One `/mcp/stdio` connection to the runtime, carrying JSON-RPC lines raw.
struct McpStdioConnection {
    to_the_runtime: std::os::unix::net::UnixStream,
    from_the_runtime: std::io::BufReader<std::os::unix::net::UnixStream>,
}

impl McpStdioConnection {
    fn upgraded_at(local_api_socket_path: &std::path::Path) -> Self {
        use std::io::{BufRead as _, Write as _};

        let mut to_the_runtime =
            std::os::unix::net::UnixStream::connect(local_api_socket_path).unwrap();
        to_the_runtime
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        to_the_runtime
            .write_all(
                b"GET /mcp/stdio HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
                  Upgrade: mcp-stdio\r\n\r\n",
            )
            .unwrap();
        let mut from_the_runtime = std::io::BufReader::new(to_the_runtime.try_clone().unwrap());
        let mut head_line = String::new();
        loop {
            head_line.clear();
            from_the_runtime.read_line(&mut head_line).unwrap();
            if head_line == "\r\n" {
                break;
            }
        }
        Self {
            to_the_runtime,
            from_the_runtime,
        }
    }

    fn send_line(&mut self, line: &str) {
        use std::io::Write as _;
        self.to_the_runtime
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
    }

    fn the_next_line(&mut self) -> String {
        use std::io::BufRead as _;
        let mut line = String::new();
        let read = self.from_the_runtime.read_line(&mut line).unwrap();
        assert_ne!(read, 0, "the runtime closed the connection");
        line
    }
}

/// A request's `_meta`, as a client at the latest revision sends it.
fn request_meta() -> serde_json::Value {
    serde_json::to_value(rmcp::model::RequestMetaObject::with_client_context(
        rmcp::model::ProtocolVersion::LATEST,
        rmcp::model::Implementation::new("relay-secret-test", "0"),
        rmcp::model::ClientCapabilities::default(),
    ))
    .unwrap()
}

#[test]
fn a_relays_secret_sent_over_mcp_stdio_in_any_shape_reaches_no_log_line_at_trace() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    tatolabd_command.env("RUST_LOG", "trace");
    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(tatolabd_command);
    let add_relay_params = json!({
        "name": "add_relay",
        "arguments": {
            "adapter": SECRET_HOLDING_TEST_RELAY_ADAPTER,
            "settings": {"token": RELAY_SECRET}
        },
        "_meta": request_meta()
    });

    let mut first_message_a_notification =
        McpStdioConnection::upgraded_at(&machine_root.local_api_socket_path());
    first_message_a_notification.send_line(
        &json!({"jsonrpc": "2.0", "method": "tools/call", "params": add_relay_params}).to_string(),
    );
    drop(first_message_a_notification);

    let mut connection = McpStdioConnection::upgraded_at(&machine_root.local_api_socket_path());
    connection.send_line(
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": request_meta()}})
            .to_string(),
    );
    assert!(connection.the_next_line().contains("\"id\":1"));
    for line_carrying_the_secret in [
        json!({"jsonrpc": "2.0", "method": "tools/call", "params": add_relay_params}),
        json!({"jsonrpc": "1.0", "id": 2, "method": "tools/call", "params": add_relay_params}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": [RELAY_SECRET]}),
    ] {
        connection.send_line(&line_carrying_the_secret.to_string());
    }
    connection.send_line(
        &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": {"_meta": request_meta()}})
            .to_string(),
    );
    while !connection.the_next_line().contains("\"id\":4") {}
    drop(connection);
    runtime.interrupt_and_expect_a_clean_exit();

    let standard_error = runtime.standard_error();
    assert!(!standard_error.contains(RELAY_SECRET), "{standard_error}");
    let runtime_log = every_file_under(&machine_root.runtime_log_directory());
    assert!(!runtime_log.contains(RELAY_SECRET), "{runtime_log}");
}
