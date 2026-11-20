// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::{ASSET, PROJECT_DIRECTORY};
use crate::control_operation::ControlOperationExample;

pub(in crate::control_operations) const OPEN_PROJECT: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const RELOAD_PROJECT: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const CLOSE_PROJECT: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const REMOVE_PROJECT: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const RENAME_PROJECT: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "name": "Night show" }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const SET_PROJECT_DESCRIPTION: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "description": "The test pattern over the camera, for the night show.",
            })
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const SET_PROJECT_COVER: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "cover": ASSET }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const DUPLICATE_PROJECT: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "into": "/home/ada/Tatolab/night-show-rehearsal",
            })
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const DELETE_PROJECT: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": "/home/ada/Tatolab/night-show-rehearsal" }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const IMPORT_PROJECT: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "source": "/home/ada/Downloads/stage-looks-main.zip",
                "project_directory": "/home/ada/Tatolab/stage-looks",
            })
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const CREATE_PROJECT: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": "/home/ada/Tatolab/rehearsal" }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const LIST_SAMPLES: ControlOperationExample =
    ControlOperationExample {
        params: || json!({}),
        answer: None,
        refusal: None,
    };
