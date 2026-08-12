// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A processor class's fully-qualified import path — the one string a
//! processor is identified by.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{SchemaError, SchemaResult};

/// The fully-qualified import path of the class a processor is —
/// `my_app.filters:BlurProcessor` from Python, `my_app::filters::BlurProcessor`
/// from Rust.
///
/// Two spellings of one concept, discriminated by the descriptor's sibling
/// `runtime` field and by nothing in the string itself. Stored verbatim and
/// never parsed — splitting on `:` or `::` to recover a short name re-invents
/// the identity grammar this type replaced.
///
/// Every inhabitant is valid: the inner string is private, [`Self::new`] is the
/// only constructor, there is no `Default`, and [`Deserialize`] validates
/// rather than inheriting `String`'s.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(transparent)]
pub struct ProcessorClassImportPath(String);

impl ProcessorClassImportPath {
    /// Build a path, refusing one that names no class.
    ///
    /// Emptiness is the whole rule: the path holds two grammars at once, so
    /// there is no single one to check it against.
    pub fn new(import_path: impl Into<String>) -> SchemaResult<Self> {
        let import_path = import_path.into();
        if import_path.trim().is_empty() {
            return Err(SchemaError::InvalidName {
                name: import_path,
                reason: "a processor's identity is the import path of the class it is, so the \
                         path has to name one — this one is blank. It is derived mechanically \
                         and never authored: the `#[processor]` macro captures it at the \
                         expansion site in Rust, and the wheel reads `__module__` and \
                         `__qualname__` off the class in Python"
                    .to_string(),
            });
        }
        Ok(Self(import_path))
    }

    /// The path as written by the authoring surface.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProcessorClassImportPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProcessorClassImportPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PYTHON_PATH: &str = "my_app.filters:BlurProcessor";
    const RUST_PATH: &str = "my_app::filters::BlurProcessor";

    /// Both spellings ride through unparsed and unaltered. A validator that
    /// admitted only one grammar would refuse half the processors in a graph.
    #[test]
    fn both_grammars_are_admitted_verbatim() {
        for path in [PYTHON_PATH, RUST_PATH, "app:P", "crate::P"] {
            assert_eq!(
                ProcessorClassImportPath::new(path).unwrap().as_str(),
                path,
                "the path must survive construction byte for byte"
            );
        }
    }

    #[test]
    fn a_blank_path_names_no_class_and_is_refused() {
        for blank in ["", " ", "\t", "\n", "   \t\n  "] {
            let refusal = ProcessorClassImportPath::new(blank)
                .expect_err("a path that names no class must be refused");
            assert!(
                refusal.to_string().contains("derived mechanically"),
                "the refusal must say where the path comes from, since the author never \
                 typed it: {refusal}"
            );
        }
    }

    /// Interior and surrounding whitespace is preserved, not trimmed: the
    /// engine stores the authoring surface's own string, and a silent trim
    /// would be the first normalization.
    #[test]
    fn a_path_with_whitespace_around_a_name_is_kept_verbatim() {
        assert_eq!(
            ProcessorClassImportPath::new(" app:P ").unwrap().as_str(),
            " app:P ",
            "construction must not normalize — only blankness is judged"
        );
    }

    #[test]
    fn it_serializes_as_a_plain_json_string() {
        let value = serde_json::to_value(ProcessorClassImportPath::new(PYTHON_PATH).unwrap())
            .expect("serialize");
        assert_eq!(
            value,
            serde_json::Value::String(PYTHON_PATH.to_string()),
            "the wire form is the bare string, not an object wrapping one"
        );
    }

    /// The hazard a plain newtype would have left open: `derive(Deserialize)`
    /// on a wrapper around `String` inherits `String`'s impl, which accepts
    /// `""` and reconstructs the invalid value the constructor refuses.
    #[test]
    fn deserialize_refuses_what_the_constructor_refuses() {
        for blank in [r#""""#, r#""   ""#] {
            let refused: Result<ProcessorClassImportPath, _> = serde_json::from_str(blank);
            assert!(
                refused.is_err(),
                "deserializing {blank} must not smuggle in a path the constructor rejects"
            );
        }
        assert!(
            rmp_serde::from_slice::<ProcessorClassImportPath>(
                &rmp_serde::to_vec_named(&"").unwrap()
            )
            .is_err(),
            "the msgpack wire must refuse it too — validation belongs to the type, not to one \
             serializer"
        );
    }

    #[test]
    fn it_round_trips_over_json_and_msgpack() {
        for path in [PYTHON_PATH, RUST_PATH] {
            let original = ProcessorClassImportPath::new(path).unwrap();

            let back: ProcessorClassImportPath =
                serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
            assert_eq!(original, back, "lost equality over JSON");

            let back: ProcessorClassImportPath =
                rmp_serde::from_slice(&rmp_serde::to_vec_named(&original).unwrap()).unwrap();
            assert_eq!(original, back, "lost equality over msgpack");
        }
    }

    /// Registry behavior, at the type: distinct paths are distinct keys and the
    /// same path is the same key, with no case folding or separator
    /// equivalence in between.
    #[test]
    fn equality_is_exact_so_the_registry_key_is_the_path_itself() {
        let path = |p: &str| ProcessorClassImportPath::new(p).unwrap();

        assert_eq!(path(PYTHON_PATH), path(PYTHON_PATH));
        assert_ne!(path(PYTHON_PATH), path(RUST_PATH));
        assert_ne!(
            path("my_app.filters:BlurProcessor"),
            path("my_app.filters:blurprocessor"),
            "case is significant — Python's names are"
        );
        assert_ne!(
            path("my_app.filters:Blur"),
            path("my_app::filters::Blur"),
            "the two grammars are different strings, never canonicalized into one"
        );
    }

    #[test]
    fn display_renders_the_path_alone() {
        assert_eq!(
            ProcessorClassImportPath::new(PYTHON_PATH)
                .unwrap()
                .to_string(),
            PYTHON_PATH
        );
    }
}
