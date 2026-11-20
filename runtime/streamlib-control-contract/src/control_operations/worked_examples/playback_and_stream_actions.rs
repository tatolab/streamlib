// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;

use super::{PROJECT_DIRECTORY, STREAM, STREAM_ACTION};
use crate::control_operation::ControlOperationExample;

pub(in crate::control_operations) const START_PLAYBACK: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const STOP_PLAYBACK: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const PAUSE_PLAYBACK: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const RESUME_PLAYBACK: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "project_directory": PROJECT_DIRECTORY }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const DESCRIBE_STREAM_ACTION: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "action": STREAM_ACTION }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const CANCEL_STREAM_ACTION: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "action": STREAM_ACTION }),
        answer: None,
        refusal: None,
    };

pub(in crate::control_operations) const SET_STREAM_CLOCK_SCALE: ControlOperationExample =
    ControlOperationExample {
        params: || json!({ "stream": STREAM, "scale": 0.5 }),
        answer: None,
        refusal: None,
    };
