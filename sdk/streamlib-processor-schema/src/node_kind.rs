// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The kind of a node type: the contract its code meets, which decides how the
//! runtime runs it.

use serde::{Deserialize, Serialize};

/// The language a function node is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FunctionNodeLanguage {
    Python,
    Rust,
}

/// How a node type's code runs: a shader on the GPU, a function at its
/// reader's read seam, or a class in a process of its own.
///
/// Serialized as `"kind"`, with `"language"` beside it on a function node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum NodeKind {
    /// Images and values in, an Image out, with no process of its own.
    Shader,
    /// A function of its inputs, evaluated where the node reading it reads.
    Function { language: FunctionNodeLanguage },
    /// A class with a lifecycle that may do anything, in its own process.
    #[default]
    Process,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_serializes_as_its_name_and_a_function_its_language_beside_it() {
        assert_eq!(
            serde_json::to_value(NodeKind::Process).unwrap(),
            serde_json::json!({"kind": "process"})
        );
        assert_eq!(
            serde_json::to_value(NodeKind::Shader).unwrap(),
            serde_json::json!({"kind": "shader"})
        );
        assert_eq!(
            serde_json::to_value(NodeKind::Function {
                language: FunctionNodeLanguage::Python
            })
            .unwrap(),
            serde_json::json!({"kind": "function", "language": "python"})
        );
    }

    #[test]
    fn a_function_kind_without_its_language_is_refused() {
        assert!(
            serde_json::from_value::<NodeKind>(serde_json::json!({"kind": "function"})).is_err()
        );
    }
}
