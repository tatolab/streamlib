// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::{IMPORTED_PROJECT, PROJECT_DIRECTORY, STREAM};
use crate::control_operation::ControlOperationExample;

pub(in crate::control_operations) const ADD_DEPENDENCY: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "source": "/home/ada/Tatolab/stage-looks",
            })
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const UPDATE_DEPENDENCY: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "project": IMPORTED_PROJECT }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const LIST_DEPENDENCIES: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "check_for_newer": true }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const REMOVE_DEPENDENCY: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "project": IMPORTED_PROJECT }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const USE_DEPENDENCY_STREAM: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "stream": STREAM,
                "project": IMPORTED_PROJECT,
                "dependency_stream": "c7a2e5f8-1b4d-4e9a-8c3f-6d0b2e5a8c14",
                "name": "warm_look",
            })
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const REFRESH_FRAGMENTS: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "changed_files": ["generators/looks.py"],
            })
        },
        answer: None,
        refusal: None,
    };
