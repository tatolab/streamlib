// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{SchemaError, SchemaResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProcessorClassShortName(String);

impl ProcessorClassShortName {
    /// Build a short name, refusing one that names no class.
    ///
    /// Emptiness is the whole rule: neither Python nor Rust enforces PascalCase
    /// on a class name, so any stricter grammar would refuse a legal class.
    pub fn new(short_name: impl Into<String>) -> SchemaResult<Self> {
        let short_name = short_name.into();
        if short_name.trim().is_empty() {
            return Err(SchemaError::InvalidName {
                name: short_name,
                reason: "an instance's display name defaults to its class's short name, so \
                         the name has to name one — this one is blank. It is derived \
                         mechanically and never authored: the `#[processor]` macro reads the \
                         struct's identifier in Rust, and the wheel reads `__name__` off the \
                         class in Python"
                    .to_string(),
            });
        }
        Ok(Self(short_name))
    }

    /// The short name as the authoring surface read it off the class.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProcessorClassShortName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProcessorClassShortName {
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

    #[test]
    fn any_non_blank_class_name_is_admitted_verbatim() {
        for name in [
            "BlurProcessor",
            "blur_processor",
            "Blur2",
            "_Private",
            "Ünicode",
        ] {
            assert_eq!(
                ProcessorClassShortName::new(name).unwrap().as_str(),
                name,
                "the class name must survive construction byte for byte"
            );
        }
    }

    #[test]
    fn a_blank_name_is_refused_naming_where_it_comes_from() {
        for blank in ["", "   ", "\t\n"] {
            let refusal = ProcessorClassShortName::new(blank)
                .expect_err("a blank short name names no class")
                .to_string();
            assert!(
                refusal.contains("__name__") && refusal.contains("#[processor]"),
                "the refusal must name both derivation sites: {refusal}"
            );
        }
    }

    /// Mental-revert guard: swap the manual impl for `#[derive(Deserialize)]`
    /// and a wire descriptor smuggles `""` past the constructor, silently
    /// blanking every default display name it feeds.
    #[test]
    fn deserialize_refuses_what_the_constructor_refuses() {
        assert!(serde_json::from_str::<ProcessorClassShortName>("\"\"").is_err());
        assert_eq!(
            serde_json::from_str::<ProcessorClassShortName>("\"BlurProcessor\"")
                .unwrap()
                .as_str(),
            "BlurProcessor"
        );
    }

    #[test]
    fn round_trips_through_json_transparently() {
        let name = ProcessorClassShortName::new("BlurProcessor").unwrap();
        let encoded = serde_json::to_string(&name).unwrap();
        assert_eq!(
            encoded, "\"BlurProcessor\"",
            "the wire form is a bare string"
        );
        assert_eq!(
            serde_json::from_str::<ProcessorClassShortName>(&encoded).unwrap(),
            name
        );
    }
}
