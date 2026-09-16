// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::thread::JoinHandle;

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// Thread handle for dedicated-thread processors.
pub struct ThreadHandleComponent {
    pub join_handle: JoinHandle<()>,
    /// Whether the thread hosts a helper process, and so walks that helper's
    /// shutdown ladder before it returns.
    pub hosts_a_helper_process: bool,
}

impl JsonSerializableComponent for ThreadHandleComponent {
    fn json_key(&self) -> &'static str {
        "thread_handle"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "attached": true,
            "thread_id": format!("{:?}", self.join_handle.thread().id()),
            "thread_name": self.join_handle.thread().name().unwrap_or("<unnamed>")
        })
    }
}
