// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The runtime's HTTP on loopback: `GET /machine-runtime-lock-holder` answers
//! the serving process's pid. It serves no media and listens on nothing but
//! `127.0.0.1`.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use streamlib::sdk::error::{Error, Result};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH, MachineRuntimeLockHolderPidAnswer,
};
use tokio_util::sync::CancellationToken;

use crate::axum_router_served_until_stopped::AxumRouterServedUntilStopped;

/// The listener's router.
fn build_loopback_http_router() -> axum::Router {
    axum::Router::new()
        .route(
            MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH,
            get(the_pid_of_this_process),
        )
        .layer(axum::middleware::from_fn(
            answer_only_requests_naming_loopback_as_their_host,
        ))
}

/// The names a request to the listener may give as its `Host`.
const LOOPBACK_HOST_NAMES: [&str; 2] = ["127.0.0.1", "localhost"];

/// Answer a request only when its `Host` names loopback. Binding loopback keeps
/// other machines out; this keeps out a page whose own name a DNS rebinding
/// pointed at `127.0.0.1`, which a browser lets read what it fetches.
async fn answer_only_requests_naming_loopback_as_their_host(
    request: Request,
    next: axum::middleware::Next,
) -> Response {
    let named_host = request
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|host| host.to_str().ok())
        .map(|host| host.rsplit_once(':').map_or(host, |(name, _port)| name));
    match named_host {
        Some(host) if LOOPBACK_HOST_NAMES.contains(&host) => next.run(request).await,
        _ => (
            StatusCode::MISDIRECTED_REQUEST,
            "this listener answers only requests addressed to 127.0.0.1 or localhost",
        )
            .into_response(),
    }
}

async fn the_pid_of_this_process() -> Json<MachineRuntimeLockHolderPidAnswer> {
    Json(MachineRuntimeLockHolderPidAnswer {
        pid: std::process::id() as i32,
    })
}

/// The loopback HTTP listener being served for an engine. Dropping it stops
/// serving.
#[must_use = "dropping this stops the loopback HTTP listener"]
#[derive(Debug)]
pub struct LoopbackHttpListenerServedForAnEngine {
    bound_address: SocketAddr,
    _served: AxumRouterServedUntilStopped,
}

impl LoopbackHttpListenerServedForAnEngine {
    /// The address the listener answers on: `127.0.0.1` and its port.
    pub fn bound_address(&self) -> SocketAddr {
        self.bound_address
    }
}

/// Serve the loopback HTTP listener on `bound_listener` until the returned
/// holder is dropped, on `tokio_handle`. A listener bound anywhere but
/// loopback is refused.
pub fn serve_the_loopback_http_listener(
    bound_listener: std::net::TcpListener,
    tokio_handle: &tokio::runtime::Handle,
) -> Result<LoopbackHttpListenerServedForAnEngine> {
    let bound_address = bound_listener.local_addr().map_err(|failure| {
        Error::Runtime(format!(
            "the loopback HTTP listener cannot read its bound listener's address: {failure}"
        ))
    })?;
    if !bound_address.ip().is_loopback() {
        return Err(Error::Runtime(format!(
            "the loopback HTTP listener serves only on loopback, and its listener is bound on \
             {bound_address}"
        )));
    }
    let refused = |failure: std::io::Error| {
        Error::Runtime(format!(
            "the loopback HTTP listener cannot serve on {bound_address}: {failure}"
        ))
    };
    bound_listener.set_nonblocking(true).map_err(refused)?;
    let loopback_http_listener = {
        let _entered_tokio_runtime = tokio_handle.enter();
        tokio::net::TcpListener::from_std(bound_listener).map_err(refused)?
    };
    let served = AxumRouterServedUntilStopped::serve(
        loopback_http_listener,
        build_loopback_http_router(),
        CancellationToken::new(),
        "the loopback HTTP listener",
        tokio_handle,
    );
    tracing::info!("Loopback HTTP listening on http://{bound_address}/");
    Ok(LoopbackHttpListenerServedForAnEngine {
        bound_address,
        _served: served,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::{IpAddr, Ipv4Addr, UdpSocket};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// One `GET path` sent to `address` with `host` as its `Host`: its status
    /// code and body.
    async fn http_get_naming_the_host(
        address: SocketAddr,
        host: &str,
        path: &str,
    ) -> (u16, String) {
        let mut connection = tokio::net::TcpStream::connect(address)
            .await
            .expect("the loopback HTTP listener accepts");
        connection
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut answer = String::new();
        connection.read_to_string(&mut answer).await.unwrap();
        let (head, body) = answer
            .split_once("\r\n\r\n")
            .expect("an HTTP answer has a head");
        let status_code = head
            .split_whitespace()
            .nth(1)
            .and_then(|status_code| status_code.parse().ok())
            .expect("an HTTP status line names a code");
        (status_code, body.to_string())
    }

    /// The listener on a free loopback port.
    fn served_on_a_free_loopback_port() -> LoopbackHttpListenerServedForAnEngine {
        serve_the_loopback_http_listener(
            std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                .expect("a free loopback port binds"),
            &tokio::runtime::Handle::current(),
        )
        .expect("a bound loopback listener is served")
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn the_listener_binds_loopback_alone() {
        let served = served_on_a_free_loopback_port();

        assert_eq!(served.bound_address().ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        if let Some(address_off_loopback) = this_hosts_address_off_loopback() {
            assert!(
                tokio::net::TcpStream::connect(SocketAddr::new(
                    address_off_loopback,
                    served.bound_address().port()
                ))
                .await
                .is_err(),
                "the listener answered at {address_off_loopback}"
            );
        }
    }

    /// The machine runtime lock's prober reads who runs the pid from the
    /// kernel; only the pid is the listener's to say, and only to a request
    /// naming loopback as its host, so a page that reached `127.0.0.1` through
    /// a rebound name of its own reads nothing.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn the_lock_holder_route_answers_this_processes_pid_to_loopback_alone() {
        let served = served_on_a_free_loopback_port();
        let port = served.bound_address().port();

        for host in [format!("localhost:{port}"), format!("127.0.0.1:{port}")] {
            assert_eq!(
                http_get_naming_the_host(
                    served.bound_address(),
                    &host,
                    MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH
                )
                .await,
                (200, format!(r#"{{"pid":{}}}"#, std::process::id())),
                "{host}"
            );
        }
        for host in [format!("attacker.example:{port}"), "attacker.example".to_string()] {
            assert_eq!(
                http_get_naming_the_host(
                    served.bound_address(),
                    &host,
                    MACHINE_RUNTIME_LOCK_HOLDER_ROUTE_PATH
                )
                .await
                .0,
                421,
                "{host}"
            );
        }
    }

    /// The listener serves no media and no control: nothing but the lock
    /// holder's pid answers.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn nothing_but_the_lock_holders_pid_is_served() {
        let served = served_on_a_free_loopback_port();
        let host = served.bound_address().to_string();

        for path in ["/", "/watch/desk/camera", "/api/graph", "/mcp", "/health", "/ws/events"] {
            assert_eq!(
                http_get_naming_the_host(served.bound_address(), &host, path)
                    .await
                    .0,
                404,
                "{path}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn a_listener_bound_off_loopback_is_refused_naming_its_address() {
        let bound_on_every_interface =
            std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let bound_address = bound_on_every_interface.local_addr().unwrap();

        let refusal = serve_the_loopback_http_listener(
            bound_on_every_interface,
            &tokio::runtime::Handle::current(),
        )
        .expect_err("a listener off loopback is refused");

        assert!(
            refusal.to_string().contains(&bound_address.to_string()),
            "{refusal}"
        );
    }

    /// This host's address on its route off the machine, when it has one —
    /// chosen by the kernel for a UDP socket aimed at a documentation address,
    /// sending nothing.
    fn this_hosts_address_off_loopback() -> Option<IpAddr> {
        let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("192.0.2.1:9").ok()?;
        let address = socket.local_addr().ok()?.ip();
        (!address.is_loopback() && !address.is_unspecified()).then_some(address)
    }
}
