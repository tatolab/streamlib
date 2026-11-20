// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};

use super::{
    CLAMP_NODE, CLAMP_NODE_TYPE, COMMIT, COMPOSITOR_NODE, GROUP, NOTE, PROJECT_DIRECTORY,
    SOURCE_NODE, SOURCE_NODE_TYPE, STREAM, TRANSACTION,
};
use crate::control_operation::ControlOperationExample;

/// The node the transact example adds.
const ADDED_SOURCE_NODE: &str = "5e1a7c3d-9f2b-4d8e-a6c4-2b0d8f6e4a19";

/// A link from the source's video output into the compositor's back layer.
fn the_source_into_the_compositor() -> Value {
    json!({
        "source": { "node": SOURCE_NODE, "port": "video" },
        "target": { "node": COMPOSITOR_NODE, "port": "layer_0" },
    })
}

/// A link from the source's video output into the clamp's number input, whose pin types do not
/// connect.
fn the_source_into_the_clamp() -> Value {
    json!({
        "source": { "node": SOURCE_NODE, "port": "video" },
        "target": { "node": CLAMP_NODE, "port": "x" },
    })
}

/// `params` with the stream added.
fn on_the_stream(params: Value) -> Value {
    let mut params = params;
    params["stream"] = json!(STREAM);
    params
}

/// The refusal of a link from the source's video output into the clamp's number input.
fn the_refusal_of_the_source_into_the_clamp(action: &str) -> Value {
    json!({
        "refusal_code": "link_pin_types_do_not_connect",
        "action": action,
        "refused_link": the_source_into_the_clamp(),
        "runtime_words": format!("{action} failed: the output `video` of `source` carries Texture, and the input `x` of `clamp` takes Float; no conversion bridges them. To fix: link a Float output into `x`, or find the inputs `video` fits with `check_link_candidates`"),
    })
}

pub(in crate::control_operations) const TRANSACT: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "operations": [
                    {
                        "operation": "add_node",
                        "stream": STREAM,
                        "type": SOURCE_NODE_TYPE,
                        "as": "source",
                        "pins": { "width": 320, "height": 180 },
                    },
                    {
                        "operation": "connect",
                        "stream": STREAM,
                        "source": { "node": "source", "port": "video" },
                        "target": { "node": COMPOSITOR_NODE, "port": "layer_0" },
                    },
                ],
                "author": { "kind": "agent", "name": "Assistant" },
            })
        },
        answer: Some(|| {
            json!({
                "mode": "commit",
                "transaction": TRANSACTION,
                "commit": COMMIT,
                "operations": [
                    {
                        "operation": "add_node",
                        "stream": STREAM,
                        "node": ADDED_SOURCE_NODE,
                        "type": SOURCE_NODE_TYPE,
                        "name": "source",
                    },
                    {
                        "operation": "connect",
                        "stream": STREAM,
                        "source": { "node": ADDED_SOURCE_NODE, "port": "video" },
                        "target": { "node": COMPOSITOR_NODE, "port": "layer_0" },
                    },
                ],
                "added_nodes": [{
                    "stream": STREAM,
                    "node": ADDED_SOURCE_NODE,
                    "name": "source",
                    "as": "source",
                }],
                "connected_links": [{
                    "stream": STREAM,
                    "source": { "node": ADDED_SOURCE_NODE, "port": "video" },
                    "target": { "node": COMPOSITOR_NODE, "port": "layer_0" },
                    "state": "wired",
                }],
            })
        }),
        refusal: Some(|| {
            let mut refusal = the_refusal_of_the_source_into_the_clamp("transact");
            refusal["refused_operation_index"] = json!(1);
            refusal
        }),
    };

pub(in crate::control_operations) const ADD_NODE: ControlOperationExample =
    ControlOperationExample {
        params: || {
            on_the_stream(json!({
                "type": CLAMP_NODE_TYPE,
                "name": "clamp",
                "pins": { "low": 0.0, "high": 1.0 },
            }))
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const CONNECT: ControlOperationExample =
    ControlOperationExample {
        params: || on_the_stream(the_source_into_the_compositor()),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const CHECK_LINK: ControlOperationExample =
    ControlOperationExample {
        params: || on_the_stream(the_source_into_the_clamp()),
        answer: Some(|| {
            json!({
                "outcome": "refused",
                "refusal": the_refusal_of_the_source_into_the_clamp("connect"),
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "port_not_found",
                "action": "check_link",
                "refused_node": CLAMP_NODE,
                "suggested_call": {
                    "operation": "describe_node_types",
                    "params": {
                        "project_directory": PROJECT_DIRECTORY,
                        "node_types": [CLAMP_NODE_TYPE],
                    },
                },
                "runtime_words": "check_link failed: `clamp` has no input `value`; its inputs are `x`, `low` and `high`",
            })
        }),
    };

pub(in crate::control_operations) const CHECK_LINK_CANDIDATES: ControlOperationExample =
    ControlOperationExample {
        params: || {
            on_the_stream(json!({
                "from": { "output": { "node": SOURCE_NODE, "port": "video" } },
                "among": {
                    "ends": [
                        { "kind": "placed_port", "node": COMPOSITOR_NODE, "port": "layer_0" },
                        { "kind": "placed_port", "node": CLAMP_NODE, "port": "x" },
                    ],
                },
            }))
        },
        answer: Some(|| {
            json!({
                "candidates": [
                    {
                        "end": { "kind": "placed_port", "node": COMPOSITOR_NODE, "port": "layer_0" },
                        "check": { "outcome": "connects" },
                    },
                    {
                        "end": { "kind": "placed_port", "node": CLAMP_NODE, "port": "x" },
                        "check": {
                            "outcome": "refused",
                            "refusal": the_refusal_of_the_source_into_the_clamp("connect"),
                        },
                    },
                ],
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "node_not_found",
                "action": "check_link_candidates",
                "refused_node": "8b3e6d1a-0c4f-4a7b-9e2d-5f8a1c4e7b30",
                "suggested_call": { "operation": "graph", "params": { "stream": STREAM } },
                "runtime_words": format!("check_link_candidates failed: the stream `{STREAM}` holds no node `8b3e6d1a-0c4f-4a7b-9e2d-5f8a1c4e7b30`"),
            })
        }),
    };

pub(in crate::control_operations) const IMPORT_GRAPH_GENERATOR: ControlOperationExample =
    ControlOperationExample {
        params: || {
            on_the_stream(json!({
                "generator": "generators.looks:night_look",
                "name": "night_look",
                "config": { "warmth": 0.3 },
            }))
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const REMOVE_NODE: ControlOperationExample =
    ControlOperationExample {
        params: || on_the_stream(json!({ "node": CLAMP_NODE })),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const DISCONNECT: ControlOperationExample =
    ControlOperationExample {
        params: || on_the_stream(the_source_into_the_compositor()),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const SET_NODE_INPUT_VALUE: ControlOperationExample =
    ControlOperationExample {
        params: || on_the_stream(json!({ "node": CLAMP_NODE, "input": "high", "value": 0.8 })),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const RENAME_NODE: ControlOperationExample =
    ControlOperationExample {
        params: || on_the_stream(json!({ "node": CLAMP_NODE, "name": "brightness_clamp" })),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const SET_STUDIO_FIELDS: ControlOperationExample =
    ControlOperationExample {
        params: || {
            on_the_stream(json!({
                "node": CLAMP_NODE,
                "studio": { "position": [420.0, 160.0], "comment": "Keeps the level under 1" },
            }))
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const SET_EXPOSED_CONFIG_FIELDS: ControlOperationExample =
    ControlOperationExample {
        params: || {
            on_the_stream(
                json!({ "node": SOURCE_NODE, "exposed_config_fields": ["width", "height"] }),
            )
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const ADD_GROUP: ControlOperationExample =
    ControlOperationExample {
        params: || {
            on_the_stream(json!({
                "title": "Picture",
                "colour": "#3b82f6",
                "nodes": [SOURCE_NODE, COMPOSITOR_NODE],
            }))
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const SET_GROUP_FIELDS: ControlOperationExample =
    ControlOperationExample {
        params: || on_the_stream(json!({ "group": GROUP, "title": "Picture in" })),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const COLLAPSE_SELECTION_INTO_SUBGRAPH: ControlOperationExample =
    ControlOperationExample {
        params: || {
            on_the_stream(json!({ "nodes": [SOURCE_NODE, CLAMP_NODE], "name": "picture_in" }))
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const UNLOCK_SUBGRAPH: ControlOperationExample =
    ControlOperationExample {
        params: || on_the_stream(json!({ "fragment": "a3f6c9e2-5b8d-4e1a-9c7f-2d4b6e8a0c13" })),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const ADD_NOTE: ControlOperationExample =
    ControlOperationExample {
        params: || {
            on_the_stream(json!({
                "text": "The compositor's back layer is the camera.",
                "position": [80.0, 40.0],
                "size": [240.0, 96.0],
            }))
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const SET_NOTE_FIELDS: ControlOperationExample =
    ControlOperationExample {
        params: || {
            on_the_stream(json!({ "note": NOTE, "text": "The back layer is the test pattern." }))
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const REMOVE_NOTE: ControlOperationExample =
    ControlOperationExample {
        params: || on_the_stream(json!({ "note": NOTE })),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const REMOVE_GROUP: ControlOperationExample =
    ControlOperationExample {
        params: || on_the_stream(json!({ "group": GROUP })),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const UNDO: ControlOperationExample = ControlOperationExample {
    params: || {
        json!({
            "project_directory": PROJECT_DIRECTORY,
            "author": { "kind": "agent", "name": "Assistant" },
        })
    },
    answer: Some(|| {
        json!({
            "format_version": 2,
            "transaction": "7c1e4a9d-3b6f-4d2e-8a5c-9f0b3d6e1a47",
            "commit": "9d3b6e1f4a7c0e2b5d8f1a4c7e0b3d6f9a2c5e8b",
            "parent_commit": COMMIT,
            "author_kind": "agent",
            "author_name": "Assistant",
            "operations": [{ "operation": "remove_node", "stream": STREAM, "node": SOURCE_NODE }],
            "inverse_operations": [{
                "operation": "add_node",
                "stream": STREAM,
                "node": SOURCE_NODE,
                "type": SOURCE_NODE_TYPE,
                "name": "source",
            }],
            "undoes": TRANSACTION,
            "applied_at": "2026-11-20T09:41:07Z",
        })
    }),
    refusal: Some(|| {
        json!({
            "refusal_code": "refused",
            "action": "undo",
            "runtime_words": format!("undo failed: Assistant has nothing to undo in `{PROJECT_DIRECTORY}`"),
        })
    }),
};

pub(in crate::control_operations) const REDO: ControlOperationExample = ControlOperationExample {
    params: || {
        json!({
            "project_directory": PROJECT_DIRECTORY,
            "author": { "kind": "agent", "name": "Assistant" },
        })
    },
    answer: None,
    refusal: None,
};
