// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The relay tools, the relay-adapter catalog resource and
//! `/api/relay-adapters`, each driven through the real router over its socket
//! into a recording stand-in for the machine's relay instances.

use std::sync::Arc;

use rmcp::RoleClient;
use rmcp::model::{CallToolRequestParams, ProtocolVersion, ReadResourceRequestParams};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RunningService};
use serde_json::{Value, json};

use crate::control_plane_stub_support::{
    LocalApiServedOnAFreshSocket, RelayInstancesOfThisMachineStub, RelayInstancesStubCall,
    STUB_RELAY_ADAPTER_DISPLAY_ICON_DATA_URI, STUB_RELAY_ADAPTER_DISPLAY_NAME,
    STUB_RELAY_ADAPTER_KIND,
};
use crate::mcp::tests::{
    ControlPlaneMcpDispatchStubRuntime, first_text_block_json, mcp_transport_to,
};
use crate::mcp_resources::RELAY_ADAPTER_CATALOG_RESOURCE_URI;

struct RelayToolsServed {
    relay_instances: Arc<RelayInstancesOfThisMachineStub>,
    served: LocalApiServedOnAFreshSocket,
    client: RunningService<RoleClient, ()>,
}

impl RelayToolsServed {
    async fn over_a_relay_stub() -> Self {
        let relay_instances = Arc::new(RelayInstancesOfThisMachineStub::default());
        let served = LocalApiServedOnAFreshSocket::over_with_relay_instances(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            Arc::clone(&relay_instances)
                as Arc<dyn streamlib_relay_adapter_sdk::RelayInstancesOfThisMachine>,
        );
        let client = ()
            .serve_with_lifecycle(
                mcp_transport_to(&served),
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::LATEST],
                },
            )
            .await
            .expect("the node answers `server/discover`");
        Self {
            relay_instances,
            served,
            client,
        }
    }

    async fn tool_call(&self, tool_name: &str, arguments: Value) -> Value {
        let request = CallToolRequestParams::new(tool_name.to_string())
            .with_arguments(arguments.as_object().cloned().unwrap_or_default());
        serde_json::to_value(self.client.call_tool(request).await.unwrap()).unwrap()
    }

    fn calls(&self) -> Vec<RelayInstancesStubCall> {
        self.relay_instances.calls.lock().clone()
    }
}

fn a_map(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn the_error_text_of(tool_result: &Value) -> String {
    assert_eq!(tool_result["isError"], true, "{tool_result}");
    tool_result["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn add_relay_hands_the_adapter_its_settings_and_answers_the_relay_id() {
    let served = RelayToolsServed::over_a_relay_stub().await;
    let result = served
        .tool_call(
            "add_relay",
            json!({
                "adapter": STUB_RELAY_ADAPTER_KIND,
                "settings": {"region": "us", "token": "s3cr3t"}
            }),
        )
        .await;
    assert_eq!(
        first_text_block_json(&result),
        json!({"relay_id": "stub-provider-1"})
    );
    assert_eq!(
        served.calls(),
        [RelayInstancesStubCall::Added {
            adapter: STUB_RELAY_ADAPTER_KIND.to_string(),
            settings: a_map(json!({"region": "us", "token": "s3cr3t"})),
        }]
    );
}

#[tokio::test]
async fn add_relay_naming_no_adapter_the_runtime_has_is_refused_naming_the_adapters() {
    let served = RelayToolsServed::over_a_relay_stub().await;
    let refusal = the_error_text_of(
        &served
            .tool_call("add_relay", json!({"adapter": "red5"}))
            .await,
    );
    assert!(
        refusal.contains("`red5`") && refusal.contains("`stub-provider`"),
        "{refusal}"
    );
    assert!(served.calls().is_empty());
}

#[tokio::test]
async fn configure_relay_hands_over_exactly_the_settings_named() {
    let served = RelayToolsServed::over_a_relay_stub().await;
    let result = served
        .tool_call(
            "configure_relay",
            json!({"relay_id": "stub-provider-1", "settings": {"region": null, "token": "rotated"}}),
        )
        .await;
    assert_eq!(
        first_text_block_json(&result),
        json!({"relay_id": "stub-provider-1"})
    );
    assert_eq!(
        served.calls(),
        [RelayInstancesStubCall::Configured {
            relay_id: "stub-provider-1".to_string(),
            settings: a_map(json!({"region": null, "token": "rotated"})),
        }]
    );
}

#[tokio::test]
async fn remove_relay_on_a_built_in_instance_is_refused_by_name() {
    let served = RelayToolsServed::over_a_relay_stub().await;
    let refusal = the_error_text_of(
        &served
            .tool_call("remove_relay", json!({"relay_id": "local-loopback"}))
            .await,
    );
    assert!(
        refusal.contains("`local-loopback`") && refusal.contains("built in"),
        "{refusal}"
    );

    let result = served
        .tool_call("remove_relay", json!({"relay_id": "stub-provider-1"}))
        .await;
    assert_eq!(
        first_text_block_json(&result),
        json!({"relay_id": "stub-provider-1"})
    );
}

#[tokio::test]
async fn list_relays_names_each_secret_set_or_unset_and_each_connection_state() {
    let served = RelayToolsServed::over_a_relay_stub().await;
    let listed = first_text_block_json(&served.tool_call("list_relays", json!({})).await);
    let relays = listed["relays"].as_array().unwrap();
    assert_eq!(
        relays
            .iter()
            .map(|relay| relay["relay_id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["local-loopback", "local-every-interface", "stub-provider-1"]
    );
    assert_eq!(relays[0]["built_in"], true);
    assert_eq!(relays[0]["connection_state"], json!({"state": "joined"}));
    assert_eq!(relays[2]["secrets"], json!({"token": "set"}));
    assert_eq!(relays[2]["settings"], json!({"region": "us"}));
    assert_eq!(relays[2]["connection_state"], json!({"state": "joining"}));
}

#[tokio::test]
async fn the_relay_adapter_catalog_resource_lists_each_adapter_with_its_secrets_write_only() {
    let served = RelayToolsServed::over_a_relay_stub().await;
    let read = served
        .client
        .read_resource(ReadResourceRequestParams::new(
            RELAY_ADAPTER_CATALOG_RESOURCE_URI,
        ))
        .await
        .unwrap();
    let read = serde_json::to_value(read).unwrap();
    let catalog: Value =
        serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
    let adapters = catalog["adapters"].as_array().unwrap();
    assert_eq!(
        adapters
            .iter()
            .map(|adapter| adapter["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["local", STUB_RELAY_ADAPTER_KIND]
    );
    assert!(adapters.iter().all(|adapter| adapter["doc"].is_string()));
    assert_eq!(
        adapters
            .iter()
            .map(|adapter| adapter["display_name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Local relay", STUB_RELAY_ADAPTER_DISPLAY_NAME]
    );
    assert!(adapters.iter().all(
        |adapter| adapter["display_icon_data_uri"] == STUB_RELAY_ADAPTER_DISPLAY_ICON_DATA_URI
    ));
    assert_eq!(
        adapters[1]["settings_schema"]["properties"]["token"]["writeOnly"],
        true
    );
}

#[tokio::test]
async fn get_relay_adapters_serves_the_same_catalog_as_the_resource() {
    let served = RelayToolsServed::over_a_relay_stub().await;
    let (status_line, body) =
        crate::control_plane_stub_support::http_get_over_the_local_api_socket(
            &served.served.local_api_socket_path,
            "/api/relay-adapters",
        )
        .await;
    assert!(status_line.contains(" 200 "), "{status_line}");
    let catalog: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(catalog["adapters"][0]["kind"], "local");
    assert_eq!(
        catalog["adapters"][1]["display_name"],
        STUB_RELAY_ADAPTER_DISPLAY_NAME
    );
    assert_eq!(
        catalog["adapters"][1]["display_icon_data_uri"],
        STUB_RELAY_ADAPTER_DISPLAY_ICON_DATA_URI
    );
    assert_eq!(
        catalog["adapters"][1]["settings_schema"]["properties"]["token"]["writeOnly"],
        true
    );
}
