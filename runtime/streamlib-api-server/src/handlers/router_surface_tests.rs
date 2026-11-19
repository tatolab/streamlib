// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What [`build_router`] exposes.
//!
//! The control plane is observation-shaped, so the router must expose no
//! route that mutates the graph — a node's graph comes from its code. And
//! no route asks for a credential: file permission on the local API socket
//! is the whole gate.
//!
//! The router is the real one; only the `RuntimeOperations` backend is a
//! stub.

use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
use streamlib::sdk::descriptors::{NodeClassImportPath, NodeClassShortName, NodeDescriptor};
use streamlib::sdk::nodes::NODE_REGISTRY;
use streamlib::sdk::runtime::BoxFuture;
use tower::ServiceExt;

/// Stub runtime backing the router tests: it answers the observation ops.
///
/// Every graph-mutating op is `unreachable!`. `RuntimeOperations` still
/// declares them — the runtime API is not what changed — but no route may
/// reach one, so a route that regrows here fails loudly instead of quietly
/// succeeding against a permissive stub.
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

/// The spec is the contract a generated client is built from, so a mutation
/// route must be absent from it too — not merely unrouted at runtime.
#[tokio::test]
async fn the_openapi_spec_documents_no_graph_mutation_route() {
    let request = Request::builder()
        .method("GET")
        .uri("/api/openapi.json")
        .body(Body::empty())
        .unwrap();
    let spec = json_body_of(request).await;
    let paths = &spec["paths"];

    // Positive control. Indexing a `Value` yields `Null` for a missing key
    // AND for indexing a non-object, so the absence assertions below would
    // pass vacuously against an empty or malformed spec.
    assert!(
        paths["/api/graph"]["get"].is_object(),
        "the spec must still document the observation routes: {spec}"
    );

    for (_, uri) in DELETED_GRAPH_MUTATION_ROUTES {
        // The path-templated routes are documented under their template,
        // not the concrete id the runtime check uses.
        let documented = uri.replace("/some-id", "/{id}");
        assert!(
            paths[documented.as_str()].is_null() && paths[*uri].is_null(),
            "{documented} must not appear in the OpenAPI spec"
        );
    }
}

/// A generated client names a stream through the `stream` query parameter,
/// optional where its absence reads every loaded stream.
#[tokio::test]
async fn the_openapi_spec_declares_the_stream_query_optional_on_reads_of_every_stream() {
    let request = Request::builder()
        .method("GET")
        .uri("/api/openapi.json")
        .body(Body::empty())
        .unwrap();
    let spec = json_body_of(request).await;

    for path in ["/api/graph", "/api/registry"] {
        let parameters = spec["paths"][path]["get"]["parameters"]
            .as_array()
            .unwrap_or_else(|| panic!("{path} must document its parameters: {spec}"));
        let stream_parameter = parameters
            .iter()
            .find(|parameter| parameter["name"] == "stream")
            .unwrap_or_else(|| panic!("{path} must declare `stream`: {parameters:?}"));
        assert_eq!(
            stream_parameter["in"], "query",
            "{path}: {stream_parameter}"
        );
        assert_ne!(
            stream_parameter["required"],
            serde_json::json!(true),
            "{path}: {stream_parameter}"
        );
    }
}

/// The registry's machine-wide answer is documented under the type the
/// schema generator writes, so a generated client parses both forms, each
/// node type's kind and language naming one of theirs; the graph's two
/// forms are documented as one open object, which the control contract's
/// `graph` answer types.
#[test]
fn the_openapi_spec_documents_both_forms_of_the_graph_and_registry_answers() {
    let spec = serde_json::to_value(control_plane_openapi_spec()).unwrap();
    let schemas = &spec["components"]["schemas"];
    let registry_answer = schemas["NodeTypesOfOneStreamOrOfEveryStream"].to_string();
    assert!(
        registry_answer.contains("RegistryResponse")
            && registry_answer.contains("MachineWideRegistryResponse"),
        "{registry_answer}"
    );
    assert!(
        schemas["MachineWideRegistryResponse"].is_object(),
        "MachineWideRegistryResponse: {spec}"
    );
    let descriptor = &schemas["NodeDescriptorOutput"]["properties"];
    assert_eq!(
        (&descriptor["kind"]["enum"], &descriptor["language"]["enum"]),
        (
            &serde_json::json!(["shader", "function", "process", "fragment"]),
            &serde_json::json!(["python", "rust"])
        ),
        "a node type's kind and language name one of theirs: {descriptor}"
    );
    let graph_answer = &spec["paths"]["/api/graph"]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];
    assert_eq!(
        graph_answer,
        &serde_json::json!({ "type": "object" }),
        "one open object holds both forms of the graph's answer, so a validator takes either"
    );
}

/// Without `stream`, `/api/graph` renders every loaded stream under the
/// machine's name, in the shape the schema generator documents.
#[tokio::test]
async fn the_graph_route_without_a_stream_renders_every_loaded_stream_under_the_machines_name() {
    let request = Request::builder()
        .method("GET")
        .uri("/api/graph")
        .body(Body::empty())
        .unwrap();
    let every_stream = json_body_of(request).await;

    assert_eq!(
        every_stream["machine"],
        crate::control_plane_stub_support::STUB_MACHINE_NAME
    );
    assert_eq!(
        every_stream["streams"].as_array().map(Vec::len),
        Some(1),
        "{every_stream}"
    );
}

/// The stub stream's node catalog is the process-global native registry,
/// so this registers a probe under a path no other test names and asserts
/// on that path alone.
/// There is no teardown: a registration is for the life of the process, and
/// the registry refuses a second one of the same path.
///
/// `/api/registry` is where an agent learns which keys a node type's
/// config takes, so it serves the descriptor's schema document itself —
/// each field's type, its description and its default — rather than a
/// name the agent would have to look up somewhere the node does not serve.
#[tokio::test]
async fn the_registry_serves_a_registered_node_types_config_schema_document() {
    let config_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "width": { "type": "integer", "description": "Frame width in pixels.", "default": 1280 },
            "height": { "type": "integer", "description": "Frame height in pixels.", "default": 720 },
        },
        "required": [],
    });
    let class_import_path = "streamlib_api_server::registry_rendering_probe::TestPatternProbe";
    NODE_REGISTRY
        .register_descriptor_only(
            NodeDescriptor::new(
                NodeClassShortName::new("TestPatternProbe").unwrap(),
                NodeClassImportPath::new(class_import_path).unwrap(),
                "a registry-rendering probe",
            )
            .with_config_schema(config_schema.clone()),
        )
        .expect("the probe's path is registered by this test alone");

    for uri in [
        "/api/registry".to_string(),
        format!(
            "/api/registry?stream={}",
            crate::control_plane_stub_support::STUB_STREAM_ID
        ),
    ] {
        let request = Request::builder()
            .method("GET")
            .uri(&uri)
            .body(Body::empty())
            .unwrap();
        let served = json_body_of(request).await;

        let probe = served["nodes"]
            .as_array()
            .expect("a node type list")
            .iter()
            .find(|entry| entry["type"] == class_import_path)
            .unwrap_or_else(|| panic!("{uri}: the probe the test registered"));
        assert_eq!(probe["config_schema"], config_schema, "{uri}");
    }
}

/// The spec a client is generated from and the spec the node serves must be
/// one document. They were two hand-maintained declarations once; the copy
/// drifted, kept publishing routes the server had dropped, and nothing went
/// red because each test read its own side.
#[tokio::test]
async fn the_generated_spec_and_the_served_spec_are_the_same_document() {
    let request = Request::builder()
        .method("GET")
        .uri("/api/openapi.json")
        .body(Body::empty())
        .unwrap();
    let served = json_body_of(request).await;
    let generated =
        serde_json::to_value(control_plane_openapi_spec()).expect("the generated spec serializes");
    assert_eq!(
        served, generated,
        "`generate_openapi` and the served `/api/openapi.json` must not diverge"
    );
}

#[tokio::test]
async fn the_observation_routes_answer_ok() {
    for uri in [
        "/health",
        "/api/graph",
        "/api/graph?stream=stub-stream",
        "/api/registry",
        "/api/registry?stream=stub-stream",
        "/api/openapi.json",
    ] {
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

/// A route reading one stream refuses a stream the runtime does not load
/// with `404`, naming the streams it does.
#[tokio::test]
async fn a_route_naming_a_stream_not_loaded_is_refused_naming_the_loaded_ones() {
    for uri in [
        "/api/graph?stream=elsewhere",
        "/api/registry?stream=elsewhere",
    ] {
        let response = control_plane_router_over_a_stub_runtime()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "GET {uri}");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let refusal = body["error"].as_str().unwrap_or_default();
        assert!(
            refusal.contains("elsewhere")
                && refusal.contains(crate::control_plane_stub_support::STUB_STREAM_ID),
            "GET {uri}: {refusal}"
        );
    }
}

/// File permission on the local API socket is the whole gate, so a request
/// carrying no credential reaches every route rather than a 401 or 403.
/// `/mcp` is reached the same way by every MCP wire test's client.
#[tokio::test]
async fn no_route_asks_for_a_credential() {
    let requests_carrying_no_credential = [
        ("GET", "/api/graph".to_string(), ""),
        (
            "GET",
            format!(
                "/api/registry?stream={}",
                crate::control_plane_stub_support::STUB_STREAM_ID
            ),
            "",
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

/// The runtime stops as a service does, or by a signal in its terminal:
/// the spec documents reads alone, every operation a `GET`, and names no
/// type that stops it.
#[test]
fn no_documented_route_shuts_the_runtime_down() {
    let spec = serde_json::to_value(control_plane_openapi_spec()).unwrap();
    let mut documented_operations: Vec<(String, String)> = spec["paths"]
        .as_object()
        .expect("the spec documents its paths")
        .iter()
        .flat_map(|(path, path_item)| {
            path_item
                .as_object()
                .expect("each path documents its operations")
                .keys()
                .filter(|key| {
                    [
                        "get", "put", "post", "delete", "patch", "options", "head", "trace",
                    ]
                    .contains(&key.as_str())
                })
                .map(move |method| (path.clone(), method.clone()))
        })
        .collect();
    documented_operations.sort_unstable();

    assert_eq!(
        documented_operations,
        [
            ("/api/graph", "get"),
            ("/api/registry", "get"),
            ("/health", "get"),
        ]
        .map(|(path, method)| (path.to_owned(), method.to_owned()))
    );
    assert!(
        !spec.to_string().contains("Shutdown"),
        "the spec names no shutdown type"
    );
}

#[tokio::test]
async fn the_router_serves_no_mutating_method_and_nothing_under_api_runtime() {
    let spec = serde_json::to_value(control_plane_openapi_spec()).unwrap();
    let documented_paths = spec["paths"]
        .as_object()
        .expect("the spec documents its paths")
        .keys()
        .map(|path| {
            format!(
                "{}?stream={}",
                path,
                crate::control_plane_stub_support::STUB_STREAM_ID
            )
        });
    let routes_outside_the_spec = [
        CONTROL_JSON_RPC_WEBSOCKET_REQUEST_TARGET,
        "/api/openapi.json",
        MCP_STDIO_UPGRADE_REQUEST_TARGET,
    ]
    .map(str::to_owned);
    let served_paths: Vec<String> = documented_paths.chain(routes_outside_the_spec).collect();
    for path in &served_paths {
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
