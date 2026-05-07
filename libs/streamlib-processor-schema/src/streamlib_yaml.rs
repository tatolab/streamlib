// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Canonical schema-source-of-truth for `streamlib.yaml`.
//!
//! This type composes the typed-identity fields from `streamlib-idents`
//! (`package`, `dependencies`, `schemas`) with the runtime fields owned by
//! the streamlib runtime (`processors`, `env`) into one shape. The
//! `JsonSchema` derive on this type is what `xtask emit-manifest-schema`
//! serialises into `schemas/streamlib.schema.json`, which every
//! `streamlib.yaml` references via the `# yaml-language-server: $schema=...`
//! magic comment.
//!
//! At runtime, `streamlib.yaml` is still parsed by narrower views
//! (`streamlib_idents::Manifest` for the resolver,
//! `streamlib::core::config::ProjectConfig` for the runtime,
//! [`crate::ProjectConfigMinimal`] for the proc-macro). Those parsers
//! tolerate fields outside their narrow view; the schema is the
//! union — the editor's source of truth for what's allowed.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use streamlib_idents::{DependencySpec, PackageMetadata};

use crate::ProcessorSchema;

/// Schema-source-of-truth for `streamlib.yaml`.
///
/// Editor schema only — runtime parsing happens via the narrower views
/// described in the module-level docs. `deny_unknown_fields` makes the
/// emitted JSON Schema set `additionalProperties: false`, so editors lint
/// typos like `procesors:` instead of silently accepting them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StreamlibYaml {
    /// Package metadata. Present on package-flavor manifests (publishable);
    /// absent on project-flavor manifests (consumers like applications or
    /// examples).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<PackageMetadata>,

    /// Dependency declarations, keyed by `@org/name`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, DependencySpec>,

    /// Explicit list of schema YAML files this package owns, relative to the
    /// manifest's directory. When omitted, the resolver auto-discovers
    /// `schemas/*.yaml` in the manifest dir.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schemas: Option<Vec<PathBuf>>,

    /// Inline processor definitions consumed by the runtime.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processors: Vec<ProcessorSchema>,

    /// Environment variables to inject into subprocess runtimes.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}
