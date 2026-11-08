// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The runtime's loopback HTTP, served beside its local API on `127.0.0.1` at
//! [`LOOPBACK_HTTP_LISTENER_DEFAULT_PORT`]. On macOS it is served on the
//! machine runtime lock's own socket.

use std::net::{Ipv4Addr, TcpListener};
use std::sync::Arc;

use streamlib::sdk::runtime::Runner;
use streamlib_api_server::{
    LoopbackHttpListenerServedForAnEngine, serve_the_loopback_http_listener,
};
#[cfg(not(feature = "machine-directories-under-a-test-root"))]
use streamlib_runtime_client_contract::local_api_wire_contract::LOOPBACK_HTTP_LISTENER_DEFAULT_PORT;
use streamlib_runtime_client_contract::machine_runtime_lock::MachineRuntimeLock;

/// The socket the machine runtime lock bound, for the loopback HTTP to be
/// served on: the lock's own on macOS, and `None` on Linux, where the runtime
/// binds its port itself.
pub(crate) fn the_loopback_http_listener_the_machine_runtime_lock_bound(
    machine_runtime_lock: &mut MachineRuntimeLock,
) -> Option<TcpListener> {
    #[cfg(target_os = "macos")]
    {
        machine_runtime_lock.take_the_bound_loopback_http_listener()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = machine_runtime_lock;
        None
    }
}

/// Serve `engine`'s loopback HTTP until the returned holder is dropped, on
/// `bound_by_the_machine_runtime_lock` — the macOS lock's socket. With none, as
/// on Linux, the runtime binds its port here, and a port another process holds
/// leaves it running without the listener, said once.
pub(crate) fn serve_the_loopback_http_listener_of_this_runtime(
    engine: &Arc<Runner>,
    bound_by_the_machine_runtime_lock: Option<TcpListener>,
) -> Option<LoopbackHttpListenerServedForAnEngine> {
    let bound_listener = match bound_by_the_machine_runtime_lock {
        Some(bound_listener) => bound_listener,
        None => bind_the_loopback_http_port()?,
    };
    match serve_the_loopback_http_listener(bound_listener, &engine.tokio_handle()) {
        Ok(served) => Some(served),
        Err(refusal) => {
            tracing::warn!("no loopback HTTP is served: {refusal}");
            None
        }
    }
}

/// `127.0.0.1` at [`the_loopback_http_port`], bound; `None`, said once, when it is not.
fn bind_the_loopback_http_port() -> Option<TcpListener> {
    let port = match the_loopback_http_port() {
        Ok(port) => port,
        Err(why_not) => {
            tracing::info!("no loopback HTTP is served: {why_not}");
            return None;
        }
    };
    match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(bound_listener) => Some(bound_listener),
        Err(failure) => {
            tracing::warn!(
                "no loopback HTTP is served: the loopback HTTP listener cannot listen on \
                 127.0.0.1:{port}: {failure}"
            );
            None
        }
    }
}

/// The port the loopback HTTP listener binds: its default.
#[cfg(not(feature = "machine-directories-under-a-test-root"))]
fn the_loopback_http_port() -> Result<u16, String> {
    Ok(LOOPBACK_HTTP_LISTENER_DEFAULT_PORT)
}

/// The port the test names, or why none is served: a test build never binds
/// the machine's own port.
#[cfg(feature = "machine-directories-under-a-test-root")]
fn the_loopback_http_port() -> Result<u16, String> {
    streamlib_runtime_client_contract::machine_directories_test_root::test_loopback_http_port_from_the_environment()
        .map_err(|refusal| refusal.to_string())
}
