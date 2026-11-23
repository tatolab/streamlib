// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::{
    COMPOSITOR_NODE, COMPOSITOR_NODE_TYPE, PROJECT, PROJECT_DIRECTORY, SOURCE_NODE,
    SOURCE_NODE_TYPE, STREAM, STREAM_ACTION, STREAM_ASSET,
};
use crate::control_operation::ControlOperationExample;

pub(in crate::control_operations) const DESCRIBE_RUNTIME: ControlOperationExample =
    ControlOperationExample {
        params: || json!({}),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const LIST_STREAMS: ControlOperationExample =
    ControlOperationExample {
        params: || json!({}),
        answer: Some(|| {
            json!({
                "streams": [{
                    "stream": STREAM,
                    "display_name": "Main",
                    "location": {
                        "project_directory": PROJECT_DIRECTORY,
                        "project": PROJECT,
                        "stream_asset": STREAM_ASSET,
                    },
                    "state": "running",
                    "node_count": 2,
                    "capture_devices_whose_access_was_never_asked_for": [],
                }],
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "params_do_not_match_schema",
                "action": "list_streams",
                "refused_params_pointer": "/stream",
                "suggested_call": {
                    "operation": "describe_operations",
                    "params": { "operations": ["list_streams"] },
                },
                "runtime_words": "list_streams failed: the params do not match its params schema at `/stream`: unknown field `stream`, it takes no params",
            })
        }),
    };

pub(in crate::control_operations) const GRAPH: ControlOperationExample = ControlOperationExample {
    params: || json!({ "stream": STREAM }),
    answer: Some(|| {
        json!({
            "asset": STREAM_ASSET,
            "display_name": "Main",
            "stream": STREAM,
            "machine": "ada-workstation",
            "nodes": [
                {
                    "id": SOURCE_NODE,
                    "type": SOURCE_NODE_TYPE,
                    "kind": "process",
                    "name": "source",
                    "ports": {
                        "inputs": [],
                        "outputs": [{
                            "name": "video",
                            "pin_type": { "type": "texture" },
                            "flow": "stream",
                        }],
                    },
                    "components": {
                        "state": "Running",
                        "failed_because": null,
                        "process_raising_because": null,
                        "not_ready_because": null,
                    },
                },
                {
                    "id": COMPOSITOR_NODE,
                    "type": COMPOSITOR_NODE_TYPE,
                    "kind": "process",
                    "name": "compositor",
                    "ports": {
                        "inputs": [{
                            "name": "layers.0.picture",
                            "pin_type": { "type": "texture" },
                            "flow": "stream",
                        }],
                        "outputs": [{
                            "name": "video",
                            "pin_type": { "type": "texture" },
                            "flow": "stream",
                        }],
                    },
                    "components": {
                        "state": "Running",
                        "failed_because": null,
                        "process_raising_because": null,
                        "not_ready_because": null,
                    },
                },
            ],
            "links": [{
                "id": "0e5b9d3f-2c7a-4f1e-8b6d-4a9c1e3f5b70",
                "source": { "node": SOURCE_NODE, "port": "video" },
                "target": { "node": COMPOSITOR_NODE, "port": "layers.0.picture" },
                "state": "wired",
                "components": {},
            }],
        })
    }),
    refusal: Some(|| {
        json!({
            "refusal_code": "stream_not_found",
            "action": "graph",
            "suggested_call": { "operation": "list_streams", "params": {} },
            "runtime_words": format!("graph failed: the runtime holds no stream named `night-show-preview`; it holds `{STREAM}`"),
        })
    }),
};

pub(in crate::control_operations) const LOGS: ControlOperationExample = ControlOperationExample {
    params: || {
        json!({
            "project_directory": PROJECT_DIRECTORY,
            "stream": STREAM,
            "action": STREAM_ACTION,
            "count": 50,
        })
    },
    answer: None,
    refusal: None,
};
