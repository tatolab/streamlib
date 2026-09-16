// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::thread::JoinHandle;

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::compiler::ProcessorThreadKind;

/// Thread handle for dedicated-thread processors.
pub struct ThreadHandleComponent {
    pub join_handle: JoinHandle<()>,
    pub kind: ProcessorThreadKind,
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
