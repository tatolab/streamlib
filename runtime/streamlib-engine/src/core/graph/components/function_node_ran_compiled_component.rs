// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// Whether a function node ran compiled, as the last of its readers' helpers
/// to finish its setup reported it.
#[derive(Debug, Default)]
pub struct HowItsReadersRanAFunctionNode {
    ran_compiled: Mutex<Option<bool>>,
}

impl HowItsReadersRanAFunctionNode {
    /// Note how a reader's helper ran the node: compiled to native code, or as
    /// Python in its interpreter.
    pub fn note_it_ran_compiled(&self, ran_compiled: bool) {
        *self.ran_compiled.lock() = Some(ran_compiled);
    }

    /// Whether the node ran compiled; `None` until a reader's helper has set up.
    pub fn ran_compiled(&self) -> Option<bool> {
        *self.ran_compiled.lock()
    }
}

/// How a function node's readers ran it, rendered by `graph` as `ran_compiled`:
/// `true` compiled, `false` as Python, `null` before any reader has set up.
pub struct FunctionNodeRanCompiledComponent(pub Arc<HowItsReadersRanAFunctionNode>);

impl JsonSerializableComponent for FunctionNodeRanCompiledComponent {
    fn json_key(&self) -> &'static str {
        "ran_compiled"
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::from(self.0.ran_compiled())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_renders_null_until_a_reader_reports_then_how_the_last_ran_it() {
        let how_it_ran = Arc::new(HowItsReadersRanAFunctionNode::default());
        let component = FunctionNodeRanCompiledComponent(Arc::clone(&how_it_ran));
        assert_eq!(component.to_json(), JsonValue::Null);
        how_it_ran.note_it_ran_compiled(false);
        assert_eq!(component.to_json(), JsonValue::Bool(false));
        how_it_ran.note_it_ran_compiled(true);
        assert_eq!(component.to_json(), JsonValue::Bool(true));
    }
}
