// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::{
    COMMIT, PROJECT, PROJECT_DIRECTORY, PROJECT_NAME, SOURCE_NODE, STREAM, TRANSACTION,
    TREMOLO_NODE_TYPE,
};
use crate::control_operation::ControlOperationExample;

/// The node file the agent writes.
const TREMOLO_PATH: &str = "nodes/tremolo.py";
/// The control the agent writes.
const KNOB_PATH: &str = "node_ui/knob.ts";
/// The node UI manifest that registers it.
const NODE_UI_MANIFEST_PATH: &str = "node_ui/tatolab_node_ui.json";
/// The SHA-256 of `nodes/tremolo.py` as the template writes it.
const TREMOLO_TEMPLATE_SHA256: &str =
    "9dc1b0111c3183e3aff1942bd345b18525ca48bf4f8b1177ba5b1719223774bd";
/// The SHA-256 of `nodes/tremolo.py` once the agent has written it.
const TREMOLO_WRITTEN_SHA256: &str =
    "dee38b6e167bbb9c5c32c30cce07d5845cf553971ce025df3f0c6f4bd7e3ffea";
/// The Tremolo node the agent writes.
const TREMOLO_SOURCE: &str = "from tatolab.stream import node\n\n\n@node\ndef Tremolo(level: float = 1.0, depth: float = 0.5) -> float:\n    \"\"\"`level` dipped by `depth`.\"\"\"\n    return level * (1.0 - depth)\n";

pub(in crate::control_operations) const LIST_PROJECT_FILES: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "under": "nodes" }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const READ_PROJECT_FILE: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "path": TREMOLO_PATH }),
        answer: Some(|| {
            json!({
                "path": TREMOLO_PATH,
                "byte_len": 138,
                "content_base64": "ZnJvbSB0YXRvbGFiLnN0cmVhbSBpbXBvcnQgbm9kZQoKCkBub2RlCmRlZiBUcmVtb2xvKGxldmVsOiBmbG9hdCA9IDEuMCkgLT4gZmxvYXQ6CiAgICAiIiJEZXNjcmliZSB3aGF0IHRoZSBub2RlIGRvZXMuIiIiCiAgICByZXR1cm4gbGV2ZWwK",
                "content_sha256": TREMOLO_TEMPLATE_SHA256,
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "file_not_found",
                "action": "read_project_file",
                "refused_file": { "path": "nodes/tremelo.py" },
                "suggested_call": {
                    "operation": "list_project_files",
                    "params": { "project_directory": PROJECT_DIRECTORY, "under": "nodes" },
                },
                "runtime_words": "read_project_file failed: the project has no file `nodes/tremelo.py`",
            })
        }),
    };

pub(in crate::control_operations) const CREATE_NODE_FILE: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "path": TREMOLO_PATH }),
        answer: Some(|| {
            json!({
                "path": TREMOLO_PATH,
                "node_type": TREMOLO_NODE_TYPE,
                "transaction": TRANSACTION,
                "commit": COMMIT,
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "refused",
                "action": "create_node_file",
                "refused_file": { "path": TREMOLO_PATH },
                "runtime_words": "create_node_file failed: `nodes/tremolo.py` already exists; read it with `read_project_file`, or name another file",
            })
        }),
    };

pub(in crate::control_operations) const CREATE_NODE_UI_ELEMENT: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "element": { "kind": "control", "pin_type": "float", "control": "knob" },
                "script": "knob.ts",
            })
        },
        answer: Some(|| {
            json!({
                "script_path": KNOB_PATH,
                "manifest_path": NODE_UI_MANIFEST_PATH,
                "template_from": "/opt/tatolab/extension-sdk-kit/templates/control.ts",
                "transaction": TRANSACTION,
                "commit": COMMIT,
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "extension_sdk_not_found",
                "action": "create_node_ui_element",
                "runtime_words": "create_node_ui_element failed: this runtime was given no extension SDK kit to take the template from. To fix: start the runtime with `TATOLAB_EXTENSION_SDK_KIT` set to the kit's folder, or write the script with `write_project_file`",
            })
        }),
    };

pub(in crate::control_operations) const WRITE_PROJECT_FILE: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "path": TREMOLO_PATH,
                "content": TREMOLO_SOURCE,
                "expected_sha256": TREMOLO_TEMPLATE_SHA256,
            })
        },
        answer: Some(|| {
            json!({
                "path": TREMOLO_PATH,
                "content_sha256": TREMOLO_WRITTEN_SHA256,
                "transaction": TRANSACTION,
                "commit": COMMIT,
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "file_changed_since_it_was_read",
                "action": "write_project_file",
                "refused_file": { "path": TREMOLO_PATH },
                "suggested_call": {
                    "operation": "read_project_file",
                    "params": { "project_directory": PROJECT_DIRECTORY, "path": TREMOLO_PATH },
                },
                "runtime_words": format!("write_project_file failed: `nodes/tremolo.py` changed since it was read: its SHA-256 is {TREMOLO_WRITTEN_SHA256}, not {TREMOLO_TEMPLATE_SHA256}. To fix: read it again and write over what it holds now"),
            })
        }),
    };

pub(in crate::control_operations) const CHECK_PROJECT_CODE: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "paths": [TREMOLO_PATH] }),
        answer: Some(|| {
            json!({
                "verdict": "failed",
                "checks": [{ "check": "python_node_declarations", "state": "failed" }],
                "findings": [{
                    "check": "python_node_declarations",
                    "finding_code": "value_input_has_no_default",
                    "severity": "error",
                    "words": "the value input `level` of `Tremolo` has no default. To fix: give it one, as `level: float = 1.0`",
                    "path": TREMOLO_PATH,
                    "line": 5,
                    "node_type": TREMOLO_NODE_TYPE,
                    "port": "level",
                }],
                "files_checked": [{ "path": TREMOLO_PATH, "content_sha256": TREMOLO_WRITTEN_SHA256 }],
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "file_not_found",
                "action": "check_project_code",
                "refused_file": { "path": "nodes/tremelo.py" },
                "suggested_call": {
                    "operation": "list_project_files",
                    "params": { "project_directory": PROJECT_DIRECTORY, "under": "nodes" },
                },
                "runtime_words": "check_project_code failed: `nodes/tremelo.py` is no file of the project",
            })
        }),
    };

pub(in crate::control_operations) const LIST_PROJECT_VIEWS: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const LIST_PROJECT_NODE_UI_ELEMENTS: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: Some(|| {
            let module = json!({ "project": PROJECT, "project_name": PROJECT_NAME });
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "project": PROJECT,
                "project_name": PROJECT_NAME,
                "elements": [{
                    "module": module,
                    "script_path": KNOB_PATH,
                    "element": { "kind": "control", "pin_type": "float", "control": "knob" },
                }],
                "manifests": [{
                    "module": module,
                    "manifest_path": NODE_UI_MANIFEST_PATH,
                    "manifest": {
                        "contract": "1.1",
                        "controls": [{ "type": "float", "control": "knob", "script": "knob.ts" }],
                    },
                }],
                "unreadable_manifests": [],
            })
        }),
        refusal: Some(|| {
            json!({
                "refusal_code": "refused",
                "action": "list_project_node_ui_elements",
                "runtime_words": "list_project_node_ui_elements failed: `/home/ada/Tatolab/night-shw` holds no `tatolab-project.json`, so it is no project",
            })
        }),
    };

pub(in crate::control_operations) const OPEN_VIEWER: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "stream": STREAM, "node_id": SOURCE_NODE, "port": "video" }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const CLOSE_VIEWER: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "viewer": "viewer-2a7c9e41-video" }),
        answer: None,
        refusal: None,
    };
