// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Discovery over `/mcp` and the transport it is served on, and the tool catalog: the revision it
//! answers, the hosts it admits, a routine request logging nothing at the default filter,
//! `describe_runtime`, the tools it lists and what each states, `call_operation`, and a call
//! naming a tool it does not list.

use super::*;

/// One `tools/call` of the tool `tool_name` itself, never rerouted through `call_operation`.
async fn tool_call_outcome_of_the_tool(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    tool_name: &'static str,
    arguments: Value,
) -> std::result::Result<Value, McpError> {
    let (_served, client) = connected_mcp_client(runtime).await;
    let request = CallToolRequestParams::new(tool_name)
        .with_arguments(arguments.as_object().cloned().unwrap_or_default());
    wire_outcome(client.call_tool(request).await)
}

async fn listed_tools(runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>) -> Vec<Value> {
    let (_served, client) = connected_mcp_client(runtime).await;
    client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|tool| serde_json::to_value(tool).unwrap())
        .collect()
}

#[tokio::test]
async fn discover_over_post_mcp_answers_the_latest_revision_alone_with_the_tools_resources_and_prompts_capabilities()
 {
    let (_served, client) =
        connected_mcp_client(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;

    let discovered = client
        .discover(RequestMetaObject::with_client_context(
            ProtocolVersion::LATEST,
            Implementation::new("discover-test", "0"),
            ClientCapabilities::default(),
        ))
        .await
        .unwrap();

    assert_eq!(discovered.supported_versions, [ProtocolVersion::LATEST]);
    assert_eq!(
        discovered.server_info().map(|server_info| server_info.name),
        Some(MCP_SERVER_NAME.to_string())
    );
    assert_eq!(
        discovered
            .server_info()
            .map(|server_info| server_info.version),
        Some(the_workspace_package_version())
    );
    assert!(discovered.capabilities.tools.is_some(), "{discovered:?}");
    assert!(
        discovered.capabilities.resources.is_some(),
        "{discovered:?}"
    );
    assert!(discovered.capabilities.prompts.is_some(), "{discovered:?}");
    assert_eq!(
        discovered.instructions.as_deref(),
        Some(LOCAL_API_MCP_SERVER_INSTRUCTIONS)
    );
}

#[tokio::test]
async fn an_initialize_handshake_over_post_mcp_is_refused_with_the_unsupported_version_error_naming_the_latest()
 {
    for handshake_protocol_version in [
        ProtocolVersion::V_2025_06_18,
        ProtocolVersion::LATEST_WITH_INITIALIZE,
    ] {
        let served =
            LocalApiServedOnAFreshSocket::over(Arc::new(ControlPlaneMcpDispatchStubRuntime::new()));
        let refusal = HandshakeRevisionMcpClient {
            handshake_protocol_version: handshake_protocol_version.clone(),
        }
        .serve_with_lifecycle(mcp_transport_to(&served), ClientLifecycleMode::Initialize)
        .await
        .err()
        .unwrap_or_else(|| panic!("{handshake_protocol_version} must not be served"));

        let ClientInitializeError::JsonRpcError(refusal) = refusal else {
            panic!("{handshake_protocol_version}: expected a JSON-RPC refusal, got {refusal}");
        };
        assert_eq!(
            refusal.code,
            rmcp::model::ErrorCode::UNSUPPORTED_PROTOCOL_VERSION
        );
        let data = refusal
            .data
            .expect("the refusal names the served revisions");
        assert_eq!(
            data["supported"],
            json!([ProtocolVersion::LATEST]),
            "{data}"
        );
        assert_eq!(
            data["requested"],
            json!(handshake_protocol_version),
            "{data}"
        );
    }
}

fn serve_one_mcp_tool_call() {
    let request_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime builds");
    request_runtime.block_on(async {
        tool_call_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "graph",
            json!({}),
        )
        .await;
    });
}

fn rmcp_trace_targets_under(env_filter_directives: &str) -> Vec<&'static str> {
    crate::control_plane_stub_support::CapturedTracingTargets::captured_from_the_second_of_two_runs(
        env_filter_directives,
        serve_one_mcp_tool_call,
    )
    .into_iter()
    .filter(|target| target.starts_with("rmcp"))
    .collect()
}

/// At the engine's default filter a node's control plane adds nothing to
/// the app's own log, MCP included.
#[test]
#[serial_test::serial]
fn a_routine_mcp_request_says_nothing_at_the_engines_default_filter() {
    assert!(
        !rmcp_trace_targets_under("info").is_empty(),
        "the SDK speaks at info, so the default has to hold it"
    );
    let targets =
        rmcp_trace_targets_under(streamlib::sdk::logging::ENGINE_DEFAULT_TRACING_FILTER_DIRECTIVES);
    assert!(
        targets.is_empty(),
        "an MCP request must be silent at the engine's default filter, got: {targets:?}"
    );
}

/// The settings that make the endpoint the latest revision's alone: no
/// session, every request carrying its own revision, and no `Host`
/// allowlist on a socket no browser can dial.
#[test]
fn the_endpoint_is_stateless_demands_per_request_metadata_and_admits_any_host() {
    let service = local_api_mcp_streamable_http_service(LocalApiMcpServerHandler::new(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        CancellationToken::new(),
    ));
    assert!(!service.config.legacy_session_mode);
    assert!(service.config.stateless_protocol_metadata_required);
    assert!(
        service.config.allowed_hosts.is_empty(),
        "{:?}",
        service.config.allowed_hosts
    );
}

#[tokio::test]
async fn a_client_naming_any_host_over_the_socket_is_answered() {
    let served =
        LocalApiServedOnAFreshSocket::over(Arc::new(ControlPlaneMcpDispatchStubRuntime::new()));
    let client = ()
        .serve_with_lifecycle(
            mcp_transport_naming(&served, "http://streamlib-node/mcp"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::LATEST],
            },
        )
        .await
        .expect("the socket's file mode is the gate, not the Host header");
    assert!(client.list_all_tools().await.is_ok());
}

#[tokio::test]
async fn a_client_speaking_only_a_handshake_revision_finds_no_compatible_revision_over_post_mcp() {
    let served =
        LocalApiServedOnAFreshSocket::over(Arc::new(ControlPlaneMcpDispatchStubRuntime::new()));
    let refusal = ()
        .serve_with_lifecycle(
            mcp_transport_to(&served),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2025_06_18],
            },
        )
        .await
        .expect_err("a handshake revision must not be served");
    let ClientInitializeError::NoCompatibleProtocolVersion {
        server_supported, ..
    } = refusal
    else {
        panic!("expected the unsupported-version refusal, got {refusal}");
    };
    assert_eq!(server_supported, [ProtocolVersion::LATEST]);
}

/// A host that holds `subscriptions/listen` open must not hold the node's
/// shutdown: stopping the local API ends the stream with its final result.
#[tokio::test]
async fn a_listen_stream_ends_gracefully_when_the_local_api_stops_serving() {
    let (mut served, client) =
        connected_mcp_client(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
    let mut subscription = client
        .listen(SubscriptionFilter::default())
        .await
        .expect("the node acknowledges a listen");
    assert_eq!(
        serde_json::to_value(subscription.acknowledged()).unwrap(),
        json!({}),
        "no list ever changes under a host, so nothing is subscribed"
    );

    served.stop_serving();

    let ended = tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("the stream ends once the local API stops");
    assert!(matches!(ended, Ok(None)), "{ended:?}");
    assert!(
        matches!(subscription.end(), Some(SubscriptionEnd::Graceful(_))),
        "the stream closes with the listen request's own final result"
    );
}

/// `[workspace.package] version` in the workspace manifest: the version every runtime crate
/// takes.
fn the_workspace_package_version() -> String {
    let workspace_manifest: toml::Table = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"),
    )
    .expect("the workspace manifest reads")
    .parse()
    .expect("the workspace manifest is TOML");
    workspace_manifest["workspace"]["package"]["version"]
        .as_str()
        .expect("the workspace manifest names a package version")
        .to_owned()
}

#[tokio::test]
async fn describe_runtime_answers_the_runtimes_description_and_names_no_stream() {
    let described = tool_call_result(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "describe_runtime",
        json!({}),
    )
    .await;
    let described: DescribeRuntimeToolResult =
        serde_json::from_value(first_text_block_json(&described)).unwrap();
    assert_eq!(
        described,
        crate::control_plane_stub_support::the_stub_runtimes_description()
    );
}

#[tokio::test]
async fn tools_list_is_exactly_what_the_contract_lists_each_tool_as_the_contract_states_it() {
    let tools = listed_tools(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
    assert_names_exactly_the_tools_the_contract_lists(
        tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect(),
    );
    for (tool, contract_tool) in tools.iter().zip(the_tools_the_contract_lists()) {
        let tool_name = &tool["name"];
        assert_eq!(
            tool["description"], contract_tool["description"],
            "{tool_name}"
        );
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool_name}");
        let (read_only_hint, destructive_hint) = match contract_tool["x-effect"].as_str() {
            Some("reads_only") => (json!(true), Value::Null),
            Some("changes") => (json!(false), json!(false)),
            Some("destroys") => (json!(false), json!(true)),
            effect => panic!("{tool_name} states the effect {effect:?}"),
        };
        assert_eq!(
            tool["annotations"]["readOnlyHint"], read_only_hint,
            "{tool_name}"
        );
        assert_eq!(
            tool["annotations"]
                .get("destructiveHint")
                .cloned()
                .unwrap_or(Value::Null),
            destructive_hint,
            "{tool_name}"
        );
    }
}

#[tokio::test]
async fn a_listed_tool_with_one_result_shape_states_it_and_answers_it_as_structured_content() {
    let tools = listed_tools(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
    let list_streams = tools
        .iter()
        .find(|tool| tool["name"] == "list_streams")
        .expect("`list_streams` is listed");
    assert_eq!(list_streams["outputSchema"]["type"], "object");

    let answered = tool_call_result(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "list_streams",
        json!({}),
    )
    .await;
    assert_eq!(
        answered["structuredContent"],
        first_text_block_json(&answered)
    );
}

#[tokio::test]
async fn the_discovery_tools_list_and_describe_every_operation_the_contract_serves_to_agents() {
    let runtime = || Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
    let served_to_agents = the_operations_the_contract_serves_to_agents();

    let grouped = first_text_block_json(
        &tool_call_result(runtime(), "list_operation_groups", json!({})).await,
    );
    let mut listed_in_a_group: Vec<(Value, Value)> = Vec::new();
    for group in grouped["groups"].as_array().expect("the groups") {
        for operation in group["operations"].as_array().expect("its operations") {
            listed_in_a_group.push((operation["name"].clone(), group["group"].clone()));
        }
    }
    let mut contract_operations_by_group: Vec<(Value, Value)> = served_to_agents
        .iter()
        .map(|method| (method["name"].clone(), method["x-group"].clone()))
        .collect();
    let by_name = |operation: &(Value, Value)| operation.0.to_string();
    listed_in_a_group.sort_by_key(by_name);
    contract_operations_by_group.sort_by_key(by_name);
    assert_eq!(listed_in_a_group, contract_operations_by_group);

    for method in &served_to_agents {
        let described = first_text_block_json(
            &tool_call_result(
                runtime(),
                "describe_operations",
                json!({ "operations": [method["name"]] }),
            )
            .await,
        );
        let [operation] = described["operations"]
            .as_array()
            .expect("the operations")
            .as_slice()
        else {
            panic!("one operation was asked for: {described}");
        };
        assert_eq!(operation["name"], method["name"]);
        assert_eq!(operation["usage"], method["description"]);
        assert_eq!(operation["effect"], method["x-effect"]);
        assert_eq!(operation["params_schema"]["type"], "object", "{operation}");
    }
}

#[tokio::test]
async fn call_operation_reaches_an_operation_that_has_no_tool_of_its_own() {
    let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
    let recorded = runtime.recorded_graph_mutations.clone();
    let add_node_params = json!({
        "stream": STUB_STREAM_ASSET,
        "type": "effects:Blur",
        "mode": "live",
    });

    let answered_directly =
        tool_call_outcome_of_the_tool(runtime.clone(), "add_node", add_node_params.clone())
            .await
            .expect("a tool result");
    let refused = the_refusal_of_a_tool_result(&answered_directly);
    assert!(
        refused.runtime_words.contains("`call_operation`"),
        "{answered_directly}"
    );
    assert_eq!(
        refused
            .suggested_call
            .map(|call| (call.operation, Value::Object(call.params))),
        Some(("add_node".to_owned(), add_node_params.clone())),
        "the suggestion is exactly the arguments `call_operation` takes"
    );
    assert!(recorded_stream_graph_operations(&recorded).is_empty());

    let answered_through_call_operation = tool_call_outcome_of_the_tool(
        runtime,
        "call_operation",
        json!({ "operation": "add_node", "params": add_node_params }),
    )
    .await
    .expect("a tool result");
    assert_eq!(
        answered_through_call_operation["isError"], false,
        "{answered_through_call_operation}"
    );
    assert_eq!(recorded_stream_graph_operations(&recorded).len(), 1);
}

#[tokio::test]
async fn call_operation_refuses_an_operation_the_agent_surface_does_not_serve() {
    let answered = tool_call_outcome_of_the_tool(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "call_operation",
        json!({ "operation": "discover" }),
    )
    .await
    .expect("a tool result");
    let refused = the_refusal_of_a_tool_result(&answered);
    assert!(refused.runtime_words.contains("`discover`"), "{answered}");
    assert_eq!(
        answered["structuredContent"]["refusal"]["refusal_code"],
        "operation_not_found"
    );
    assert_eq!(
        refused.suggested_call.map(|call| call.operation).as_deref(),
        Some("list_operation_groups")
    );
}

#[tokio::test]
async fn params_that_do_not_match_are_refused_naming_where_and_suggesting_their_description() {
    for (tool_name, arguments, refused_action) in [
        (
            "call_operation",
            json!({ "operation": "list_streams", "params": { "unexpected": true } }),
            "list_streams",
        ),
        (
            "list_streams",
            json!({ "unexpected": true }),
            "list_streams",
        ),
    ] {
        let answered = tool_call_outcome_of_the_tool(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            tool_name,
            arguments,
        )
        .await
        .expect("a tool result");

        let refused = the_refusal_of_a_tool_result(&answered);
        assert_eq!(
            answered["structuredContent"]["refusal"]["refusal_code"], "params_do_not_match_schema",
            "{answered}"
        );
        assert_eq!(refused.action, refused_action);
        assert_eq!(
            refused.refused_params_pointer.as_deref(),
            Some("/unexpected")
        );
        assert_eq!(
            refused
                .suggested_call
                .map(|call| (call.operation, Value::Object(call.params))),
            Some((
                "describe_operations".to_owned(),
                json!({ "operations": ["list_streams"] })
            ))
        );
    }

    let answered = tool_call_outcome_of_the_tool(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "describe_operations",
        json!({ "operations": "graph" }),
    )
    .await
    .expect("a tool result");
    let refused = the_refusal_of_a_tool_result(&answered);
    assert_eq!(refused.action, "describe_operations");
    assert_eq!(
        refused.refused_params_pointer.as_deref(),
        Some("/operations")
    );
}

async fn tool_call_refusal(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    tool_name: &str,
    arguments: Value,
) -> McpError {
    match tool_call_outcome(runtime, tool_name, arguments).await {
        Ok(result) => panic!("`{tool_name}` answered a result: {result}"),
        Err(refusal) => refusal,
    }
}

#[tokio::test]
async fn tools_call_unknown_tool_is_refused_as_invalid_params() {
    let refusal = tool_call_refusal(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "does_not_exist",
        json!({}),
    )
    .await;
    assert_eq!(refusal.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}
