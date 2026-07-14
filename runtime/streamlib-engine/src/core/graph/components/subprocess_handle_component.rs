// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;
use std::process::Child;

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// Subprocess handle for Python (and future non-Rust) processors.
pub struct SubprocessHandleComponent {
    pub child: Child,
    pub config_path: PathBuf,
}

impl JsonSerializableComponent for SubprocessHandleComponent {
    fn json_key(&self) -> &'static str {
        "subprocess_handle"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "attached": true,
            "pid": self.child.id(),
            "config_path": self.config_path.display().to_string()
        })
    }
}
