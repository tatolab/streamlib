// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::thread::JoinHandle;

use serde_json::Value as JsonValue;

use super::JsonComponent;

/// Thread handle for dedicated-thread processors.
pub struct ThreadHandleComponent(pub JoinHandle<()>);

impl JsonComponent for ThreadHandleComponent {
    fn json_key(&self) -> &'static str {
        "thread_handle"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "attached": true,
            "thread_id": format!("{:?}", self.0.thread().id()),
            "thread_name": self.0.thread().name().unwrap_or("<unnamed>")
        })
    }
}
