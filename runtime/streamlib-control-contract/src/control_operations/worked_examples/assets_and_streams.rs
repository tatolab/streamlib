// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::{ASSET, PROJECT_DIRECTORY, STREAM_ASSET};
use crate::control_operation::ControlOperationExample;

pub(in crate::control_operations) const LIST_ASSETS: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "kind": "media" }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const DESCRIBE_ASSET: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "asset": ASSET }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const READ_ASSET_PREVIEW: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "asset": ASSET }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const IMPORT_ASSET: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "source": "/home/ada/Videos/opening.mp4",
                "asset_path": "media/opening.mp4",
            })
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const RELOCATE_ASSET: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "asset": ASSET,
                "to_path": "archive/opening.mp4",
            })
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const DELETE_ASSET: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "asset": ASSET }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const SET_ASSET_SETTINGS: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "asset": ASSET,
                "import_settings": { "loop": true },
            })
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const REPLACE_ASSET: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "asset": ASSET,
                "source": "/home/ada/Videos/opening-v2.mp4",
            })
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const CREATE_STREAM: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "path": "streams/preview.graph.json",
                "display_name": "Preview",
            })
        },
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const DELETE_STREAM: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY, "stream_asset": STREAM_ASSET }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const SET_STREAM_DISPLAY_NAME: ControlOperationExample =
    ControlOperationExample {
        params: || {
            json!({
                "project_directory": PROJECT_DIRECTORY,
                "stream_asset": STREAM_ASSET,
                "display_name": "Main show",
            })
        },
        answer: None,
        refusal: None,
    };
