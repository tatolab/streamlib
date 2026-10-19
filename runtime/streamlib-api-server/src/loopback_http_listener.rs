// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The runtime's HTTP on loopback, carrying what is not real time: `GET /`
//! lists the machine's loaded streams and the output ports each exposes, and
//! `GET /watch/<machine>/<stream>[/<node>/<port>]` serves the preview page from
//! its directory in the runtime unit. It serves no media and binds nothing but
//! `127.0.0.1`; what a port carries is read as a track at a relay.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use streamlib::sdk::descriptors::CatalogEntry;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::graph::OutputPortExposureLevel;
use streamlib::sdk::runtime::{
    ExposedOutputPortOfALoadedStream, OperationsOnTheStreamsLoadedInThisRuntime,
};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;
use tower_http::services::{ServeDir, ServeFile};

use crate::axum_router_served_until_stopped::AxumRouterServedUntilStopped;
use crate::handlers::error_response;

/// The port the loopback HTTP listener binds when nothing names another.
pub const LOOPBACK_HTTP_LISTENER_DEFAULT_PORT: u16 = 4446;

/// The route prefix the preview page is served under.
const WATCH_PAGE_ROUTE_PREFIX: &str = "/watch";

/// The file every `/watch` path naming no file of the page answers with.
const WATCH_PAGE_ENTRY_FILE_NAME: &str = "index.html";

/// `GET /`: the machine and every loaded stream with the output ports it
/// exposes.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct LoopbackHttpListingOfTheMachinesStreams {
    /// This machine's name, the first part of every address.
    machine: String,
    /// Every loaded stream, in the order the runtime lists them.
    streams: Vec<LoopbackHttpListingOfOneStream>,
}

/// One loaded stream in the listing.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct LoopbackHttpListingOfOneStream {
    /// The stream's cast name.
    name: String,
    /// The output ports it exposes, private or public; an internal port is not
    /// exposed and is not listed.
    exposed_output_ports: Vec<LoopbackHttpListingOfOneExposedOutputPort>,
}

/// One exposed output port in the listing.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct LoopbackHttpListingOfOneExposedOutputPort {
    /// The node's name.
    node: String,
    /// The port's cast name.
    port: String,
    /// Private or public.
    level: OutputPortExposureLevel,
    /// The port's catalog entry, `null` when it carries none.
    catalog_entry: Option<CatalogEntry>,
}

impl From<ExposedOutputPortOfALoadedStream> for LoopbackHttpListingOfOneExposedOutputPort {
    fn from(exposed_output_port: ExposedOutputPortOfALoadedStream) -> Self {
        Self {
            node: exposed_output_port.node_name,
            port: exposed_output_port.port_name,
            level: exposed_output_port.level,
            catalog_entry: exposed_output_port.catalog_entry,
        }
    }
}

/// What the listener's routes read.
#[derive(Clone)]
struct LoopbackHttpListenerState {
    operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    watch_page_directory: PathBuf,
    watch_page_files: ServeDir<ServeFile>,
}

/// The listener's router over `operations_on_the_loaded_streams`, serving the
/// preview page out of `watch_page_directory`.
fn build_loopback_http_router(
    operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    watch_page_directory: PathBuf,
) -> axum::Router {
    // A directory answers the entry file rather than a redirect, whose location
    // would leave the `/watch` route the request arrived under.
    let watch_page_files = ServeDir::new(&watch_page_directory)
        .append_index_html_on_directories(false)
        .fallback(ServeFile::new(
            watch_page_directory.join(WATCH_PAGE_ENTRY_FILE_NAME),
        ));
    axum::Router::new()
        .route("/", get(the_listing_of_the_machines_streams))
        .route(WATCH_PAGE_ROUTE_PREFIX, get(a_file_of_the_watch_page))
        .route(
            &format!("{WATCH_PAGE_ROUTE_PREFIX}/{{*watch_page_path}}"),
            get(a_file_of_the_watch_page),
        )
        .with_state(LoopbackHttpListenerState {
            operations_on_the_loaded_streams,
            watch_page_directory,
            watch_page_files,
        })
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

async fn the_listing_of_the_machines_streams(
    State(state): State<LoopbackHttpListenerState>,
) -> Response {
    let operations_on_the_loaded_streams = Arc::clone(&state.operations_on_the_loaded_streams);
    // A stream's graph lock is held across a compile, so the read waits off
    // the async workers.
    let listed = tokio::task::spawn_blocking(move || {
        the_listing_of_the_machines_streams_now(operations_on_the_loaded_streams.as_ref())
    })
    .await;
    match listed {
        Ok(listing) => Json(listing).into_response(),
        Err(read_failure) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format_args!("the machine's streams could not be read: {read_failure}"),
        ),
    }
}

/// The listing of the machine's streams as they stand. Blocks while a stream
/// compiles.
fn the_listing_of_the_machines_streams_now(
    operations_on_the_loaded_streams: &dyn OperationsOnTheStreamsLoadedInThisRuntime,
) -> LoopbackHttpListingOfTheMachinesStreams {
    LoopbackHttpListingOfTheMachinesStreams {
        machine: operations_on_the_loaded_streams
            .this_machines_name()
            .name()
            .to_string(),
        streams: operations_on_the_loaded_streams
            .every_loaded_stream_and_the_output_ports_it_exposes()
            .into_iter()
            .map(|loaded_stream| LoopbackHttpListingOfOneStream {
                name: loaded_stream.stream_name,
                exposed_output_ports: loaded_stream
                    .exposed_output_ports
                    .into_iter()
                    .map(LoopbackHttpListingOfOneExposedOutputPort::from)
                    .collect(),
            })
            .collect(),
    }
}

/// A file of the preview page: the one the path names under `/watch`, else the
/// page's entry file, which reads the address out of the path itself.
async fn a_file_of_the_watch_page(
    State(state): State<LoopbackHttpListenerState>,
    mut request: Request,
) -> Response {
    if !state.watch_page_directory.is_dir() {
        return (
            StatusCode::NOT_FOUND,
            format!(
                "this runtime unit carries no preview page: {} is not a directory",
                state.watch_page_directory.display()
            ),
        )
            .into_response();
    }
    let path_below_the_watch_route = request
        .uri()
        .path()
        .strip_prefix(WATCH_PAGE_ROUTE_PREFIX)
        .filter(|path_below| !path_below.is_empty())
        .unwrap_or("/");
    let Ok(uri_below_the_watch_route) = path_below_the_watch_route.parse::<Uri>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    *request.uri_mut() = uri_below_the_watch_route;
    match state.watch_page_files.oneshot(request).await {
        Ok(served) => served.into_response(),
        Err(infallible) => match infallible {},
    }
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

/// Serve the loopback HTTP listener over `operations_on_the_loaded_streams` on
/// `127.0.0.1:port` until the returned holder is dropped, on `tokio_handle`,
/// serving the preview page out of `watch_page_directory`. Port 0 binds a free
/// port, which [`LoopbackHttpListenerServedForAnEngine::bound_address`] names.
/// A port another process holds is refused here.
pub fn serve_the_loopback_http_listener(
    operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    port: u16,
    watch_page_directory: PathBuf,
    tokio_handle: &tokio::runtime::Handle,
) -> Result<LoopbackHttpListenerServedForAnEngine> {
    let requested_address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    let refused = |failure: std::io::Error| {
        Error::Runtime(format!(
            "the loopback HTTP listener cannot listen on {requested_address}: {failure}"
        ))
    };
    let bound_listener = std::net::TcpListener::bind(requested_address).map_err(refused)?;
    bound_listener.set_nonblocking(true).map_err(refused)?;
    let bound_address = bound_listener.local_addr().map_err(refused)?;
    let loopback_http_listener = {
        let _entered_tokio_runtime = tokio_handle.enter();
        tokio::net::TcpListener::from_std(bound_listener).map_err(refused)?
    };
    let served = AxumRouterServedUntilStopped::serve(
        loopback_http_listener,
        build_loopback_http_router(operations_on_the_loaded_streams, watch_page_directory),
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
    use std::net::{IpAddr, UdpSocket};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::handlers::router_surface_tests::a_control_plane_router_stub_runtime;

    /// One `GET path` answered at `address`: its status code and body.
    pub(crate) async fn http_get(address: SocketAddr, path: &str) -> (u16, String) {
        http_get_naming_the_host(address, &address.to_string(), path).await
    }

    /// One `GET path` sent to `address` with `host` as its `Host`.
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

    /// The listener over the stub runtime on a free port, serving the page out
    /// of `watch_page_directory`.
    fn served_over_the_stub_runtime(
        watch_page_directory: PathBuf,
    ) -> LoopbackHttpListenerServedForAnEngine {
        serve_the_loopback_http_listener(
            a_control_plane_router_stub_runtime(),
            0,
            watch_page_directory,
            &tokio::runtime::Handle::current(),
        )
        .expect("a free loopback port binds")
    }

    /// A page directory holding an entry file and one asset.
    fn a_watch_page_directory() -> tempfile::TempDir {
        let watch_page_directory = tempfile::tempdir().unwrap();
        std::fs::write(
            watch_page_directory.path().join(WATCH_PAGE_ENTRY_FILE_NAME),
            "<!doctype html><title>watch</title>",
        )
        .unwrap();
        std::fs::write(watch_page_directory.path().join("main.js"), "watch();").unwrap();
        watch_page_directory
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn the_listener_binds_loopback_alone() {
        let watch_page_directory = a_watch_page_directory();
        let served = served_over_the_stub_runtime(watch_page_directory.path().to_path_buf());

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

    /// Both forms of the address answer the page's entry file, which reads the
    /// address out of the path; a path naming one of the page's files answers
    /// that file.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn every_watch_address_answers_the_page_and_a_named_file_answers_itself() {
        let watch_page_directory = a_watch_page_directory();
        let served = served_over_the_stub_runtime(watch_page_directory.path().to_path_buf());

        for watch_address in [
            "/watch/desk/camera",
            "/watch/desk/camera/encoder/encoded_video",
            "/watch",
        ] {
            assert_eq!(
                http_get(served.bound_address(), watch_address).await,
                (200, "<!doctype html><title>watch</title>".to_string()),
                "{watch_address}"
            );
        }
        assert_eq!(
            http_get(served.bound_address(), "/watch/main.js").await,
            (200, "watch();".to_string())
        );
    }

    /// A directory of the page answers the entry file rather than a redirect
    /// whose location would leave the `/watch` route.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn a_directory_of_the_page_answers_its_entry_file_rather_than_a_redirect() {
        let watch_page_directory = a_watch_page_directory();
        std::fs::create_dir(watch_page_directory.path().join("assets")).unwrap();
        let served = served_over_the_stub_runtime(watch_page_directory.path().to_path_buf());

        assert_eq!(
            http_get(served.bound_address(), "/watch/assets").await,
            (200, "<!doctype html><title>watch</title>".to_string())
        );
    }

    /// Only a request naming loopback as its host is answered, so a page that
    /// reached `127.0.0.1` through a rebound name of its own reads nothing.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn a_request_naming_another_host_is_refused() {
        let watch_page_directory = a_watch_page_directory();
        let served = served_over_the_stub_runtime(watch_page_directory.path().to_path_buf());
        let port = served.bound_address().port();

        for (host, expected_status_code) in [
            (format!("localhost:{port}"), 200),
            (format!("127.0.0.1:{port}"), 200),
            (format!("attacker.example:{port}"), 421),
            ("attacker.example".to_string(), 421),
        ] {
            assert_eq!(
                http_get_naming_the_host(served.bound_address(), &host, "/")
                    .await
                    .0,
                expected_status_code,
                "{host}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn a_unit_carrying_no_page_answers_not_found_naming_where_it_looked() {
        let no_page_here = tempfile::tempdir().unwrap().path().join("watch");
        let served = served_over_the_stub_runtime(no_page_here.clone());

        let (status_code, body) = http_get(served.bound_address(), "/watch/desk/camera").await;

        assert_eq!(status_code, 404);
        assert!(body.contains(&no_page_here.display().to_string()), "{body}");
    }

    /// The listener serves no media and no control: nothing but the listing and
    /// the page answers.
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn nothing_but_the_listing_and_the_page_is_served() {
        let watch_page_directory = a_watch_page_directory();
        let served = served_over_the_stub_runtime(watch_page_directory.path().to_path_buf());

        for served_path in ["/", "/watch/desk/camera"] {
            assert_eq!(
                http_get(served.bound_address(), served_path).await.0,
                200,
                "{served_path}"
            );
        }
        for path in ["/api/graph", "/mcp", "/health", "/ws/events", "/watchdesk"] {
            assert_eq!(
                http_get(served.bound_address(), path).await.0,
                404,
                "{path}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial(tcp_listeners_of_this_test_process)]
    async fn a_port_another_process_holds_is_refused_naming_it() {
        let held = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let held_port = held.local_addr().unwrap().port();

        let refusal = serve_the_loopback_http_listener(
            a_control_plane_router_stub_runtime(),
            held_port,
            tempfile::tempdir().unwrap().path().to_path_buf(),
            &tokio::runtime::Handle::current(),
        )
        .expect_err("a held port is refused");

        assert!(
            refusal
                .to_string()
                .contains(&format!("127.0.0.1:{held_port}")),
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
