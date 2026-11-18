// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A client built against another contract is refused naming the older side, with one record on
//! every way it connects.

use serde_json::{Value, json};
use streamlib_control_contract::{CONTROL_CONTRACT_REVISION, CONTROL_CONTRACT_VERSION};

use super::tests::ControlServedWithAnMcpClient;
use crate::control_plane_stub_support::the_stub_runtimes_description;

/// The record a runtime refuses a client of `client_version` and `client_revision` with.
fn the_mismatch_of_a_client(client_version: u32, client_revision: u32, older_side: &str) -> Value {
    json!({
        "client_contract_version": client_version,
        "runtime_contract_version": CONTROL_CONTRACT_VERSION,
        "client_contract_revision": client_revision,
        "runtime_contract_revision": CONTROL_CONTRACT_REVISION,
        "older_side": older_side,
        "runtime_version": the_stub_runtimes_description().version,
    })
}

/// The contract headers of a client of `version` and `revision`, as one request's lines and as
/// the WebSocket upgrade's pairs.
fn contract_headers(version: u32, revision: u32) -> (String, [(&'static str, String); 2]) {
    (
        format!(
            "tatolab-control-contract-version: {version}\r\n\
             tatolab-control-contract-revision: {revision}\r\n"
        ),
        [
            ("tatolab-control-contract-version", version.to_string()),
            ("tatolab-control-contract-revision", revision.to_string()),
        ],
    )
}

/// The record each way a client connects refuses it with (`discover`, a request's headers and
/// the WebSocket upgrade's 409 body), and the runtime's words for it.
async fn the_mismatch_on_every_way_a_client_connects(
    served: &ControlServedWithAnMcpClient,
    client_version: u32,
    client_revision: u32,
) -> ([Value; 3], String) {
    let discovered = served
        .control_call(
            "discover",
            json!({ "contract_version": client_version, "contract_revision": client_revision }),
        )
        .await;
    assert_eq!(discovered["error"]["code"], 3, "{discovered}");
    let (request_lines, upgrade_headers) = contract_headers(client_version, client_revision);
    let (_, requested) = served
        .control_post_with_headers(
            &request_lines,
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "list_streams" }).to_string(),
        )
        .await;
    assert_eq!(requested["error"]["code"], 3, "{requested}");
    assert_eq!(
        requested["error"]["message"], discovered["error"]["message"],
        "a request and `discover` carry the same words"
    );
    let Err((status, upgrade_refusal)) = served
        .control_websocket_with_headers(&upgrade_headers)
        .await
    else {
        panic!("the runtime opened the control WebSocket for a client of another contract");
    };
    assert_eq!(status, 409);
    (
        [
            discovered["error"]["data"].clone(),
            requested["error"]["data"].clone(),
            upgrade_refusal,
        ],
        discovered["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    )
}

#[tokio::test]
async fn a_client_of_a_higher_revision_is_refused_naming_the_runtime_older_on_every_way_it_connects()
 {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let higher_revision = CONTROL_CONTRACT_REVISION + 1;
    let expected = the_mismatch_of_a_client(CONTROL_CONTRACT_VERSION, higher_revision, "runtime");
    let (refusals, message) = the_mismatch_on_every_way_a_client_connects(
        &served,
        CONTROL_CONTRACT_VERSION,
        higher_revision,
    )
    .await;
    for refused in refusals {
        assert_eq!(refused, expected);
    }
    assert!(message.contains("update the runtime"), "{message}");
    assert!(
        message.contains(&the_stub_runtimes_description().version),
        "the words name the runtime's release: {message}"
    );
}

#[tokio::test]
async fn a_client_of_another_version_is_refused_naming_the_side_of_the_lower_version() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    for (client_version, older_side) in [
        (CONTROL_CONTRACT_VERSION + 1, "runtime"),
        (CONTROL_CONTRACT_VERSION - 1, "client"),
    ] {
        let expected = the_mismatch_of_a_client(client_version, 0, older_side);
        let (refusals, _) =
            the_mismatch_on_every_way_a_client_connects(&served, client_version, 0).await;
        for refused in refusals {
            assert_eq!(refused, expected);
        }
    }
}

#[tokio::test]
async fn a_client_of_a_lower_revision_or_naming_no_contract_is_served() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let lower_revision = CONTROL_CONTRACT_REVISION.saturating_sub(1);
    let discovered = served
        .control_call(
            "discover",
            json!({ "contract_version": CONTROL_CONTRACT_VERSION, "contract_revision": lower_revision }),
        )
        .await;
    assert_eq!(
        discovered["result"]["contract_revision"], CONTROL_CONTRACT_REVISION,
        "{discovered}"
    );
    let served_by_version_alone = served
        .control_call(
            "discover",
            json!({ "contract_version": CONTROL_CONTRACT_VERSION }),
        )
        .await;
    assert!(
        served_by_version_alone.get("result").is_some(),
        "{served_by_version_alone}"
    );
    let (request_lines, upgrade_headers) =
        contract_headers(CONTROL_CONTRACT_VERSION, lower_revision);
    let (_, requested) = served
        .control_post_with_headers(
            &request_lines,
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "list_streams" }).to_string(),
        )
        .await;
    assert!(requested.get("result").is_some(), "{requested}");
    served
        .control_websocket_with_headers(&upgrade_headers)
        .await
        .unwrap_or_else(|refused| panic!("the runtime refused a lower revision: {refused:?}"));
    let unversioned = served.control_call("discover", json!({})).await;
    assert!(unversioned.get("result").is_some(), "{unversioned}");
}

#[tokio::test]
async fn a_contract_revision_header_that_is_not_a_whole_number_is_an_invalid_request() {
    let served = ControlServedWithAnMcpClient::over_the_stub().await;
    let (_, refused) = served
        .control_post_with_headers(
            "tatolab-control-contract-revision: one\r\n",
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "list_streams" }).to_string(),
        )
        .await;
    assert_eq!(refused["error"]["code"], -32600, "{refused}");
    assert_eq!(
        refused["error"]["message"],
        "`tatolab-control-contract-revision` must be a contract revision, a whole number"
    );
}
