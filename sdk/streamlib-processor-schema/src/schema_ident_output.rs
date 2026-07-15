// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wire-output DTOs for a structured schema identifier and its semantic
//! version.
//!
//! [`SchemaIdentOutput`] is the four-field JSON-wire projection of a
//! [`SchemaIdent`] (`{ org, package, type, version: { major, minor, patch } }`)
//! per the architecture's structured-everywhere rule — the joined
//! `@org/pkg/Type@v` form is render-only and never round-trips through a parser
//! at the structured boundary. These live in the engine-free
//! `streamlib-processor-schema` crate (alongside [`SchemaIdent`] / `SemVer`) so
//! the engine's API layer, the MoQ catalog, and any other consumer share one
//! definition without pulling in the engine.
//!
//! The `openapi` cargo feature adds a `utoipa::ToSchema` derive so the
//! host-side API server can register these DTOs in its OpenAPI document. It is
//! off by default: a plugin `.slpkg` built against the engine-free authoring
//! SDK reaches these types through this crate but must not compile the
//! host-only OpenAPI machinery.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{PortSchemaSpec, SchemaIdent};

/// Semantic version (major.minor.patch).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SemanticVersionOutput {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// Structured schema identifier — `@org/package/Type@version` rendered as
/// four typed fields per the architecture's structured-everywhere rule. The
/// joined `@org/pkg/Type@v` form is render-only — it never round-trips back
/// through a parser at the structured boundary.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SchemaIdentOutput {
    /// Org segment (e.g., `tatolab`).
    pub org: String,
    /// Package segment (e.g., `core`).
    pub package: String,
    /// Type-name segment (PascalCase, e.g., `VideoFrame`).
    #[serde(rename = "type")]
    pub type_name: String,
    /// Semantic version of the package the type belongs to.
    pub version: SemanticVersionOutput,
}

impl From<&SchemaIdent> for SchemaIdentOutput {
    fn from(ident: &SchemaIdent) -> Self {
        Self {
            org: ident.org.as_str().to_string(),
            package: ident.package.as_str().to_string(),
            type_name: ident.r#type.as_str().to_string(),
            version: SemanticVersionOutput {
                major: ident.version.major,
                minor: ident.version.minor,
                patch: ident.version.patch,
            },
        }
    }
}

impl SchemaIdentOutput {
    /// Resolve a structured port schema spec into the JSON-wire output
    /// shape. `Any` ports yield `None` (the field is omitted on the wire).
    pub fn from_port_spec(spec: &PortSchemaSpec) -> Option<Self> {
        match spec {
            PortSchemaSpec::Any => None,
            PortSchemaSpec::Specific(ident) => Some(Self::from(ident)),
            // `Named` should never reach this site — runtime startup +
            // proc-macro expansion both resolve bare-name port refs to
            // `Specific(SchemaIdent)` against the enclosing manifest's
            // `schemas:` map. A `Named` here is a runtime bug.
            PortSchemaSpec::Named(name) => panic!(
                "PortSchemaSpec::Named(`{}`) reached json-schema render — \
                 must be resolved before this site",
                name.as_str()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Org, Package, SemVer, TypeName};

    fn ident(org: &str, pkg: &str, ty: &str, v: SemVer) -> SchemaIdent {
        SchemaIdent::new(
            Org::new(org).unwrap(),
            Package::new(pkg).unwrap(),
            TypeName::new(ty).unwrap(),
            v,
        )
    }

    #[test]
    fn from_port_spec_resolves_specific_to_structured() {
        let spec =
            PortSchemaSpec::Specific(ident("tatolab", "core", "VideoFrame", SemVer::new(1, 0, 0)));
        let s = SchemaIdentOutput::from_port_spec(&spec)
            .expect("Specific must yield a structured output");
        assert_eq!(s.org, "tatolab");
        assert_eq!(s.package, "core");
        assert_eq!(s.type_name, "VideoFrame");
        assert_eq!(s.version.major, 1);
        assert_eq!(s.version.minor, 0);
        assert_eq!(s.version.patch, 0);
    }

    #[test]
    fn from_port_spec_returns_none_for_any() {
        // `Any` is the wildcard for ports accepting arbitrary payloads.
        // The JSON wire shape is `null` (skip_serializing_if = Option::is_none).
        assert!(SchemaIdentOutput::from_port_spec(&PortSchemaSpec::Any).is_none());
    }

    #[test]
    fn schema_ident_output_serializes_with_renamed_type_field() {
        // The `type` field name is reserved; the struct uses `type_name`
        // internally and renames to "type" on the wire.
        let s = SchemaIdentOutput {
            org: "tatolab".to_string(),
            package: "core".to_string(),
            type_name: "VideoFrame".to_string(),
            version: SemanticVersionOutput {
                major: 1,
                minor: 0,
                patch: 0,
            },
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["org"], "tatolab");
        assert_eq!(json["package"], "core");
        assert_eq!(json["type"], "VideoFrame"); // renamed from type_name
        assert_eq!(json["version"]["major"], 1);
    }
}
