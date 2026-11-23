// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What [`build_router`] exposes.
//!
//! A change reaches the runtime only as a control operation, so the router
//! serves none of the graph mutation routes it once had. And no route asks
//! for a credential: file permission on the local API socket is the whole
//! gate.
//!
//! The router is the real one; only the `RuntimeOperations` backend is a
//! stub.

use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
use streamlib::sdk::runtime::{BoxFuture, RuntimeOperations};
use streamlib_runtime_client_contract::local_api_wire_contract::control_rest_operation_route_path;
use tower::ServiceExt;

/// Stub runtime backing the router tests: it answers the observation ops.
///
/// Every graph-mutating op is `unreachable!`: these tests only read, so a
/// test that reaches one fails loudly instead of quietly succeeding against a
/// permissive stub.
#[derive(Clone, Default)]
struct ControlPlaneRouterStubRuntime;

impl RuntimeOperations for ControlPlaneRouterStubRuntime {
    fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>> {
        Box::pin(async { Ok(serde_json::json!({})) })
    }
    fn the_graph_as_answered_async(&self) -> BoxFuture<'_, Result<serde_json::Value>> {
        self.to_json_async()
    }
    fn to_json(&self) -> Result<serde_json::Value> {
        Ok(serde_json::json!({}))
    }
    crate::control_plane_stub_support::every_reserved_slot_reader_is_unreachable!("route");
    crate::control_plane_stub_support::graph_mutation_ops_are_unreachable!("route");
}

crate::control_plane_stub_support::a_stub_runtime_loading_this_stub_as_its_only_stream!(
    ControlPlaneRouterStubRuntime
);

/// The routes this control plane deliberately does not have: every graph
/// mutation the pre-pivot api-server served. Method + path exactly as they
/// were, so this reads as the inventory it is.
const DELETED_GRAPH_MUTATION_ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/node"),
    ("POST", "/api/node/source"),
    ("POST", "/api/node/source/replace"),
    ("DELETE", "/api/nodes/some-id"),
    ("POST", "/api/connections"),
    ("DELETE", "/api/connections/some-id"),
];

fn control_plane_router_over(runtime: ControlPlaneRouterStubRuntime) -> Router {
    build_router(Arc::new(runtime), CancellationToken::new()).local_api_socket_router
}

/// A default stub runtime, for tests that build the router themselves.
pub(crate) fn a_control_plane_router_stub_runtime()
-> Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime> {
    Arc::new(ControlPlaneRouterStubRuntime)
}

/// The real router over a default stub runtime.
pub(crate) fn control_plane_router_over_a_stub_runtime() -> Router {
    control_plane_router_over(ControlPlaneRouterStubRuntime)
}

async fn status_of(request: Request<Body>) -> StatusCode {
    control_plane_router_over_a_stub_runtime()
        .oneshot(request)
        .await
        .unwrap()
        .status()
}

async fn json_body_of(request: Request<Body>) -> serde_json::Value {
    let response = control_plane_router_over_a_stub_runtime()
        .oneshot(request)
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// The load-bearing surface assertion: no graph-mutation route is served.
#[tokio::test]
async fn the_router_serves_no_graph_mutation_route() {
    for (method, uri) in DELETED_GRAPH_MUTATION_ROUTES {
        let request = Request::builder()
            .method(*method)
            .uri(*uri)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let status = status_of(request).await;
        assert!(
            status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
            "{method} {uri} must not be routed; got {status}"
        );
    }
}

/// The description is generated from the contract, so a mutation route is
/// absent from it too — not merely unrouted at runtime.
#[tokio::test]
async fn the_served_description_documents_no_graph_mutation_route() {
    let request = Request::builder()
        .method("GET")
        .uri(CONTROL_REST_OPENAPI_DOCUMENT_ROUTE_PATH)
        .body(Body::empty())
        .unwrap();
    let spec = json_body_of(request).await;
    let paths = &spec["paths"];

    // Positive control. Indexing a `Value` yields `Null` for a missing key
    // AND for indexing a non-object, so the absence assertions below would
    // pass vacuously against an empty or malformed spec.
    assert!(
        paths[control_rest_operation_route_path("graph")]["post"].is_object(),
        "the description documents the operations: {spec}"
    );

    for (_, uri) in DELETED_GRAPH_MUTATION_ROUTES {
        let documented = uri.replace("/some-id", "/{id}");
        assert!(
            paths[documented.as_str()].is_null() && paths[*uri].is_null(),
            "{documented} must not appear in the description"
        );
    }
}

/// The description a client is generated from and the one the runtime
/// serves are one document, generated from the contract.
#[tokio::test]
async fn the_served_description_is_the_one_generated_from_the_contract() {
    let request = Request::builder()
        .method("GET")
        .uri(CONTROL_REST_OPENAPI_DOCUMENT_ROUTE_PATH)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        json_body_of(request).await,
        streamlib_control_contract::control_rest_openapi_document()
    );
}

#[tokio::test]
async fn health_and_the_description_answer_ok() {
    for uri in ["/health", CONTROL_REST_OPENAPI_DOCUMENT_ROUTE_PATH] {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status_of(request).await,
            StatusCode::OK,
            "GET {uri} must answer 200"
        );
    }
}

/// The routes each read served before every REST route was an operation's
/// are gone: no route answers but an operation's own.
#[tokio::test]
async fn the_routes_of_the_reads_before_the_operations_are_not_served() {
    for uri in [
        "/api/graph",
        "/api/graph?stream=stub-stream",
        "/api/registry",
        "/api/registry?stream=stub-stream",
    ] {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status_of(request).await,
            StatusCode::NOT_FOUND,
            "GET {uri} must not be served"
        );
    }
}

/// File permission on the local API socket is the whole gate, so a request
/// carrying no credential reaches every route rather than a 401 or 403.
/// `/mcp` is reached the same way by every MCP wire test's client.
#[tokio::test]
async fn no_route_asks_for_a_credential() {
    let requests_carrying_no_credential = [
        (
            "POST",
            control_rest_operation_route_path("graph"),
            "{}".to_owned(),
        ),
        (
            "POST",
            control_rest_operation_route_path("graph"),
            serde_json::json!({ "stream": crate::control_plane_stub_support::STUB_STREAM_ID })
                .to_string(),
        ),
    ];
    for (method, uri, body) in requests_carrying_no_credential {
        let request = Request::builder()
            .method(method)
            .uri(&uri)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let status = status_of(request).await;
        assert!(
            ![
                StatusCode::UNAUTHORIZED,
                StatusCode::FORBIDDEN,
                StatusCode::NOT_FOUND
            ]
            .contains(&status),
            "{method} {uri} must reach its handler with no credential; got {status}"
        );
    }
}

/// The runtime stops as a service does, or by a signal in its terminal: no
/// operation the description documents stops it.
#[test]
fn no_documented_route_shuts_the_runtime_down() {
    let spec = streamlib_control_contract::control_rest_openapi_document();
    let documented_routes: Vec<&String> = spec["paths"]
        .as_object()
        .expect("the description documents its paths")
        .keys()
        .collect();
    assert!(!documented_routes.is_empty());
    for route in documented_routes {
        assert!(
            ["shutdown", "quit", "exit", "terminate", "stop_runtime"]
                .iter()
                .all(|stopping| !route.contains(stopping)),
            "{route} stops the runtime"
        );
    }
}

#[tokio::test]
async fn the_router_serves_nothing_under_api_runtime_and_no_mutating_method_beside_the_operations()
{
    for path in [
        "/health",
        CONTROL_REST_OPENAPI_DOCUMENT_ROUTE_PATH,
        CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET,
        MCP_STDIO_UPGRADE_REQUEST_TARGET,
    ] {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let status = status_of(request).await;
            assert!(
                !status.is_success(),
                "{method} {path} must not be served; got {status}"
            );
        }
    }
    for method in ["PUT", "PATCH", "DELETE"] {
        let path = control_rest_operation_route_path("graph");
        let request = Request::builder()
            .method(method)
            .uri(&path)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(
            status_of(request).await,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}: an operation is called with POST alone"
        );
    }

    for path in [
        "/api/runtime",
        "/api/runtime/",
        "/api/runtime/stop",
        "/api/runtime/quit",
        "/api/runtime/exit",
        "/api/runtime/terminate",
    ] {
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap();
            assert_eq!(
                status_of(request).await,
                StatusCode::NOT_FOUND,
                "{method} {path} must not be served"
            );
        }
    }
}
