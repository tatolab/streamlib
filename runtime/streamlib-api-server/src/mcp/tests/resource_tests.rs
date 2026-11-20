// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Resources: the server instructions, and the catalog and live-graph resources.

use super::*;
use streamlib::sdk::descriptors::{NodeClassShortName, NodeDescriptor, PortDescriptor};
use streamlib::sdk::nodes::NODE_REGISTRY;

/// The control vocabulary tells an agent to confirm `wired` in `graph`. A
/// link onto a helper is not wired when `connect` returns, so the same text
/// has to say what `pending` and `error` mean or the confirmation is a
/// guess.
#[test]
fn the_instructions_say_what_pending_and_error_mean() {
    assert!(
        LOCAL_API_MCP_SERVER_INSTRUCTIONS.contains("pending")
            && LOCAL_API_MCP_SERVER_INSTRUCTIONS.contains("error_reason"),
        "the instructions tell an agent to confirm a link's state but not what a link left \
         `pending` by a helper that has not answered, or refused with a reason, \
         means:\n{LOCAL_API_MCP_SERVER_INSTRUCTIONS}"
    );
}

async fn listed_resources_result(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
) -> Value {
    let (_served, client) = connected_mcp_client(runtime).await;
    wire_outcome(client.list_resources(None).await).unwrap()
}

async fn listed_resource_templates_result(
    runtime: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
) -> Value {
    let (_served, client) = connected_mcp_client(runtime).await;
    wire_outcome(client.list_resource_templates(None).await).unwrap()
}

#[tokio::test]
async fn resources_list_names_the_node_catalog_the_live_graph_and_the_actions_under_way() {
    let result = listed_resources_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;

    let uris: Vec<&str> = result["resources"]
        .as_array()
        .expect("a resources array")
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap())
        .collect();
    assert_eq!(
        uris,
        [
            "streamlib://node-catalog",
            "streamlib://graph",
            "streamlib://stream-actions"
        ]
    );
    for resource in result["resources"].as_array().unwrap() {
        assert_eq!(resource["mimeType"], "application/json", "{resource}");
        assert!(
            resource["description"]
                .as_str()
                .is_some_and(|d| !d.is_empty())
        );
    }

    let templates =
        listed_resource_templates_result(Arc::new(ControlPlaneMcpDispatchStubRuntime::new())).await;
    let uri_templates: Vec<&str> = templates["resourceTemplates"]
        .as_array()
        .expect("a resource templates array")
        .iter()
        .map(|template| template["uriTemplate"].as_str().unwrap())
        .collect();
    assert_eq!(
        uri_templates,
        [
            "streamlib://project/{project}/transactions",
            "streamlib://stream-action/{action}"
        ],
        "{templates}"
    );
}

/// Mental revert: render the catalog once at startup and cache it, and the
/// second read misses the type registered after the first.
#[tokio::test]
async fn the_catalog_resource_renders_the_registry_as_it_stands_at_each_read() {
    let class_import_path = "streamlib_api_server::catalog_resource_probe::RegisteredBetweenReads";
    let entry_for_the_probe = |catalog: &Value| {
        catalog["nodes"]
            .as_array()
            .expect("a node type list")
            .iter()
            .find(|entry| entry["type"] == class_import_path)
            .cloned()
    };

    let before = resource_document(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "streamlib://node-catalog",
    )
    .await;
    assert!(entry_for_the_probe(&before).is_none());

    let config_schema = json!({
        "type": "object",
        "properties": { "strength": { "type": "number", "default": 0.5 } }
    });
    NODE_REGISTRY
        .register_descriptor_only(
            NodeDescriptor::new(
                NodeClassShortName::new("RegisteredBetweenReads").unwrap(),
                NodeClassImportPath::new(class_import_path).unwrap(),
                "registered after the first read",
            )
            .with_config_schema(config_schema.clone())
            .with_input(
                PortDescriptor::new(
                    "video_from_upstream",
                    "",
                    true,
                    streamlib::sdk::descriptors::PortFamilyDeclaration::of_family(
                        streamlib::sdk::descriptors::PortFamily::Image,
                    ),
                )
                .with_delivery_profile("newest"),
            ),
        )
        .expect("the probe's path is registered by this test alone");

    let after = resource_document(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "streamlib://node-catalog",
    )
    .await;
    let entry = entry_for_the_probe(&after).expect("the type registered between reads");
    assert_eq!(entry["config_schema"], config_schema);
    assert_eq!(entry["inputs"][0]["name"], "video_from_upstream");
    assert_eq!(entry["inputs"][0]["delivery_profile"], "newest");
    assert_eq!(
        after["streams"],
        json!([{ "stream": STUB_STREAM_ID, "nodes": [], "installed": [] }]),
        "each loaded stream lists the types its own interpreter described"
    );
}

/// The graph resource is every loaded stream's graph under the runtime's
/// name, rendered at each read.
#[tokio::test]
async fn the_graph_resource_renders_every_loaded_streams_graph_as_it_stands_at_each_read() {
    let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
    let exported_graph = runtime.exported_graph.clone();

    let before = resource_document(runtime.clone(), "streamlib://graph").await;
    assert_eq!(
        before,
        json!({
            "machine": STUB_MACHINE_NAME,
            "streams": [{ "nodes": [], "links": [] }],
        })
    );

    *exported_graph.lock() = two_linked_nodes_graph();
    let after = resource_document(runtime, "streamlib://graph").await;
    assert_eq!(
        after,
        json!({
            "machine": STUB_MACHINE_NAME,
            "streams": [as_a_graph_call_answers_it(two_linked_nodes_graph())],
        })
    );
}

#[tokio::test]
async fn reading_a_resource_the_node_does_not_serve_is_refused_naming_the_uri() {
    let refusal = resource_read_outcome(
        Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
        "streamlib://contracts",
    )
    .await
    .expect_err("an unserved uri is refused");
    assert_eq!(refusal.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(
        refusal.message.contains("streamlib://contracts"),
        "{refusal:?}"
    );
}
