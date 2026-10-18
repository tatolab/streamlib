// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A local relay instance joined against the staged `moq-relay`, on free
//! ports, and the HTTP and MoQ reads the tests make of it.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use streamlib_relay_adapter_local::{
    LocalRelayAdapter, LocalRelayListener, LocalRelaySharedByBothInstances,
    mint_the_runtimes_relay_publish_token,
};
use streamlib_relay_adapter_sdk::{JoinedRelay, MachineName, RelayAdapter};

/// The variable naming a `moq-relay` other than the staged one.
pub const LOCAL_RELAY_EXECUTABLE_ENVIRONMENT_VARIABLE: &str = "TATOLAB_TEST_LOCAL_RELAY_EXECUTABLE";

/// How long a child has to answer over HTTP after joining.
pub const HTTP_ANSWER_BUDGET: Duration = Duration::from_secs(20);

/// The `moq-relay` the tests run: the variable's, else the one
/// `cargo xtask stage-local-relay` built.
pub fn the_staged_local_relay_executable() -> PathBuf {
    let relay_executable = std::env::var_os(LOCAL_RELAY_EXECUTABLE_ENVIRONMENT_VARIABLE)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/local-relay/bin/moq-relay")
        });
    assert!(
        relay_executable.is_file(),
        "no moq-relay at {}: run `cargo xtask stage-local-relay`, or name one in \
         {LOCAL_RELAY_EXECUTABLE_ENVIRONMENT_VARIABLE}",
        relay_executable.display()
    );
    relay_executable
        .canonicalize()
        .expect("the staged relay has a canonical path")
}

/// A port nothing holds on `listener`'s address, for QUIC and HTTP alike.
pub fn a_free_port_for(listener: LocalRelayListener) -> u16 {
    for _ in 0..50 {
        let probe = TcpListener::bind(listener.bind_address(0)).expect("an ephemeral port binds");
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        if the_port_is_free(listener, port) {
            return port;
        }
    }
    panic!("no port free for both UDP and TCP on {listener:?}");
}

/// Whether both UDP and TCP bind `port` on `listener`'s address.
pub fn the_port_is_free(listener: LocalRelayListener, port: u16) -> bool {
    let address = listener.bind_address(port);
    TcpListener::bind(address).is_ok() && UdpSocket::bind(address).is_ok()
}

/// One instance joined on a free port, with the async runtime its admission
/// is answered on and the tests' MoQ clients run on.
pub struct JoinedLocalRelayUnderTest {
    pub adapter: LocalRelayAdapter,
    pub joined: JoinedRelay,
    pub port: u16,
    pub async_runtime: tokio::runtime::Runtime,
    _relays_directory: tempfile::TempDir,
}

impl JoinedLocalRelayUnderTest {
    pub fn join(listener: LocalRelayListener) -> Self {
        let async_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let relays_directory = tempfile::Builder::new()
            .prefix("relays-")
            .tempdir_in("/tmp")
            .unwrap();
        let mut adapter = LocalRelayAdapter::for_the_listener(
            listener,
            LocalRelaySharedByBothInstances {
                relay_executable: the_staged_local_relay_executable(),
                relays_directory: relays_directory.path().to_path_buf(),
                machine_name: MachineName::from_a_hostname("desk").unwrap(),
                runtime_publish_token: mint_the_runtimes_relay_publish_token().unwrap(),
                async_runtime: async_runtime.handle().clone(),
            },
        );
        let port = a_free_port_for(listener);
        let joined = adapter
            .join(serde_json::json!({ "port": port }))
            .unwrap_or_else(|refusal| panic!("{listener:?}: {refusal}"));
        Self {
            adapter,
            joined,
            port,
            async_runtime,
            _relays_directory: relays_directory,
        }
    }
}

impl Drop for JoinedLocalRelayUnderTest {
    fn drop(&mut self) {
        self.adapter.leave();
    }
}

/// A MoQ client with the stack's defaults.
pub fn a_moq_client() -> moq_tokio::Client {
    moq_tokio::connect::Config::default()
        .init(moq_tokio::quic::Config::default())
        .expect("a MoQ client builds")
}

/// The body of `GET <path>` on loopback `port`, or `None` when nothing
/// answers it with a 200.
pub fn http_get_body(port: u16, path: &str) -> Option<String> {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).ok()?;
    let (head, body) = answer.split_once("\r\n\r\n")?;
    head.starts_with("HTTP/1.1 200")
        .then(|| body.trim().to_string())
}

/// Whether `condition` holds within `budget`.
pub fn wait_until(budget: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    condition()
}

/// Wait until loopback `port` serves `fingerprint` at `/certificate.sha256`.
pub fn wait_until_the_served_fingerprint_is(port: u16, fingerprint: &str) {
    let mut last_served = None;
    let served = wait_until(HTTP_ANSWER_BUDGET, || {
        last_served = http_get_body(port, "/certificate.sha256");
        last_served.as_deref() == Some(fingerprint)
    });
    assert!(
        served,
        "port {port} served {last_served:?} rather than {fingerprint} within {HTTP_ANSWER_BUDGET:?}"
    );
}
