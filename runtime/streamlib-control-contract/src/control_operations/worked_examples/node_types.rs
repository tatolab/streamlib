// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::{
    CLAMP_NODE_TYPE, PROJECT, PROJECT_DIRECTORY, PROJECT_NAME, SOURCE_NODE_TYPE, TREMOLO_NODE_TYPE,
};
use crate::control_operation::ControlOperationExample;

pub(in crate::control_operations) const LIST_PIN_TYPES: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const LIST_NODE_TYPES: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: Some(|| {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "project": PROJECT,
                "project_name": PROJECT_NAME,
                "sources": [
                    {
                        "source": "project",
                        "node_types": [{
                            "node_type": CLAMP_NODE_TYPE,
                            "documentation": "`x` held between `low` and `high`.",
                            "category": "Math",
                            "category_from": "declaration",
                            "kind": "function",
                            "described_from": "source",
                        }],
                        "graph_generators": [],
                        "unreadable_modules": [{
                            "module": "nodes.sketch",
                            "path": "nodes/sketch.py",
                            "line": 7,
                            "column": 5,
                            "why": "invalid syntax",
                        }],
                    },
                    {
                        "source": "built_in",
                        "node_types": [{
                            "node_type": SOURCE_NODE_TYPE,
                            "documentation": "A test pattern of colour bars and a moving sweep.",
                            "category": "Sources",
                            "category_from": "declaration",
                            "kind": "process",
                            "described_from": "built_in",
                        }],
                        "graph_generators": [],
                        "unreadable_modules": [],
                    },
                ],
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "refused",
                "action": "list_node_types",
                "runtime_words": "list_node_types failed: `/home/ada/Tatolab/night-shw` holds no `tatolab-project.json`, so it is no project",
            })
        }),
    };

pub(in crate::control_operations) const DESCRIBE_NODE_TYPES: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "node_types": [CLAMP_NODE_TYPE] }),
        answer: Some(|| {
            json!({
                "node_types": [{
                    "source": "project",
                    "node_type": CLAMP_NODE_TYPE,
                    "described_from": "source",
                    "documentation": "`x` held between `low` and `high`.",
                    "category": "Math",
                    "category_from": "declaration",
                    "kind": "function",
                    "declared_at": { "path": "nodes/clamp.py", "line": 5 },
                    "inputs": [
                        {
                            "name": "x",
                            "documentation": "The number held.",
                            "pin_type": { "kind": "is", "pin_type": { "type": "float" } },
                            "flow": "value",
                            "mode": "wire_or_value",
                            "default_value": 0.0,
                            "lifetime": "kept",
                            "value_json_schema": { "type": "number" },
                        },
                        {
                            "name": "low",
                            "documentation": "The least it is held to.",
                            "pin_type": { "kind": "is", "pin_type": { "type": "float" } },
                            "flow": "value",
                            "mode": "wire_or_value",
                            "default_value": 0.0,
                            "lifetime": "kept",
                            "value_json_schema": { "type": "number" },
                        },
                    ],
                    "outputs": [{
                        "name": "out",
                        "documentation": "",
                        "pin_type": { "kind": "is", "pin_type": { "type": "float" } },
                        "flow": "value",
                    }],
                }],
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "node_type_not_found",
                "action": "describe_node_types",
                "suggested_call": {
                    "operation": "list_node_types",
                    "params": { "project_directory": PROJECT_DIRECTORY },
                },
                "runtime_words": "describe_node_types failed: the project offers no node type `nodes.clamp:Clip`",
            })
        }),
    };

pub(in crate::control_operations) const SET_NODE_TYPE_GROUP: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "node_type": TREMOLO_NODE_TYPE,
                "group": "Audio/Modulation",
            })
        },
        answer: None,
        refusal: None,
    };
