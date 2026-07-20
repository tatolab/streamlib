// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Publish-time processor/port/schema catalog — the queryable metadata a
//! client browses to answer "what processors exist and how do they wire
//! together" without downloading and compiling every package.
//!
//! The catalog is a deterministic function of a package's `streamlib.yaml`
//! (built at publish time by `streamlib-pack`) written into the static
//! registry tree alongside the artifacts:
//!
//! - Per-package `slpkg/<name>/<version>/<name>.catalog.json` — the
//!   [`PackageCatalog`] for one published package.
//! - Per-schema `slpkg/<name>/<version>/schemas/<Type>.jtd.json` — the JSON
//!   Type Definition for each schema the package OWNS (deduped by ownership;
//!   a package emits only the schemas it declares locally).
//! - Registry-wide `catalog/index.ndjson` — one [`CatalogIndexLine`] per
//!   processor across every published package (the node-palette aggregate).
//!
//! Port and config schema references are RESOLVED to release-core
//! [`SchemaIdent`]s at publish time (bare `Named` refs are looked up against
//! the manifest's `schemas:` map), so a client never re-implements
//! resolution — it reads the fully-qualified ident and, if it wants the
//! field-level shape, fetches the matching `.jtd.json`.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{ResolverError, ResolverResult};
use crate::ident::{PackageRef, SchemaIdent, TypeName};
use crate::semver::SemVer;

/// Tree-relative path of the registry-wide processor index (NDJSON).
pub const CATALOG_INDEX_PATH: &str = "catalog/index.ndjson";

/// The per-package catalog filename for a package named `pkg_name`
/// (`<name>.catalog.json`), written into the package's version directory
/// beside its `.slpkg`.
pub fn package_catalog_file_name(pkg_name: &str) -> String {
    format!("{pkg_name}.catalog.json")
}

/// The per-schema JTD filename for a type (`<Type>.jtd.json`), written under
/// the owning package's `schemas/` subdirectory.
pub fn schema_jtd_file_name(type_name: &TypeName) -> String {
    format!("{}.jtd.json", type_name.as_str())
}

/// Processor runtime language recorded in the catalog. A lean mirror of the
/// authored `runtime` field, decoupled from the engine / processor-schema
/// types so the catalog stays engine-free and registry-local.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogRuntime {
    Rust,
    Python,
    #[serde(alias = "deno")]
    TypeScript,
}

/// A resolved schema reference on a catalog port: either the `any` wildcard
/// or a fully-qualified release-core [`SchemaIdent`].
///
/// Wire form mirrors the authoring convention: the literal string `"any"`
/// for the wildcard, or the structured four-field `SchemaIdent` map for a
/// specific type. Unambiguous by construction — a string is always the
/// wildcard, a map is always a concrete ident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogSchemaRef {
    /// Wildcard — the port accepts any payload.
    Any,
    /// A concrete, release-core-projected schema identity.
    Schema(SchemaIdent),
}

impl CatalogSchemaRef {
    /// The inner [`SchemaIdent`] when this is a concrete reference.
    pub fn schema(&self) -> Option<&SchemaIdent> {
        match self {
            Self::Schema(ident) => Some(ident),
            Self::Any => None,
        }
    }
}

impl Serialize for CatalogSchemaRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Any => serializer.serialize_str("any"),
            Self::Schema(ident) => ident.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for CatalogSchemaRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Wildcard(String),
            Schema(SchemaIdent),
        }
        match Repr::deserialize(deserializer)? {
            Repr::Wildcard(s) if s == "any" => Ok(Self::Any),
            Repr::Wildcard(s) => Err(serde::de::Error::custom(format!(
                "catalog port schema must be the literal \"any\" or a structured \
                 schema-ident map; got the string \"{s}\""
            ))),
            Repr::Schema(ident) => Ok(Self::Schema(ident)),
        }
    }
}

/// An input or output port of a catalog processor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPort {
    /// Port name (e.g. `video_in`).
    pub name: String,
    /// Human-readable description, when the manifest declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Resolved schema flowing through this port.
    pub schema: CatalogSchemaRef,
    /// Declared delivery-profile override for an input port (e.g. `lossless`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_profile: Option<String>,
}

/// The config binding of a catalog processor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogConfig {
    /// Config field name (e.g. `config`).
    pub name: String,
    /// Resolved config schema identity (always concrete — config refs are
    /// never wildcards).
    pub schema: SchemaIdent,
}

/// One processor described for the node palette: its identity, runtime,
/// entrypoint, config binding, and typed input/output ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogProcessor {
    /// Processor short name (e.g. `Camera`).
    pub name: String,
    /// Human-readable description, when the manifest declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Runtime language.
    pub runtime: CatalogRuntime,
    /// Entrypoint for non-Rust runtimes (e.g. `src.blur:BlurProcessor`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    /// Config binding, when the processor declares one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<CatalogConfig>,
    /// Input ports.
    #[serde(default)]
    pub inputs: Vec<CatalogPort>,
    /// Output ports.
    #[serde(default)]
    pub outputs: Vec<CatalogPort>,
}

/// The catalog for one published package — its processors, resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageCatalog {
    /// The `@org/name` this catalog describes.
    pub package: PackageRef,
    /// The published version.
    pub version: SemVer,
    /// Processors this package contributes.
    #[serde(default)]
    pub processors: Vec<CatalogProcessor>,
}

/// One line of the registry-wide `catalog/index.ndjson` — a self-contained
/// per-processor record. The full node-palette / wiring graph is
/// reconstructable from the aggregate alone, without fetching any
/// per-package catalog or `.slpkg`.
///
/// Extra fields a future index might carry are ignored on read (serde's
/// default), so the shape can grow without breaking older readers — the
/// same forward-compat contract as the version index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogIndexLine {
    /// The owning `@org/name`.
    pub package: PackageRef,
    /// The published version of the owning package.
    pub version: SemVer,
    /// The processor record.
    pub processor: CatalogProcessor,
}

/// Render index lines as NDJSON (one JSON object per line, trailing newline) —
/// the byte shape written to `catalog/index.ndjson`. Mirrors the version-index
/// NDJSON pattern in the registry client.
pub fn render_catalog_index_ndjson(lines: &[CatalogIndexLine]) -> String {
    let mut out = String::new();
    for line in lines {
        // A struct of owned, already-validated data serializes infallibly.
        out.push_str(&serde_json::to_string(line).expect("serialize catalog index line"));
        out.push('\n');
    }
    out
}

/// Parse NDJSON index bytes into catalog lines. Blank and unparseable lines
/// are skipped, so a partially corrupt index degrades to "fewer processors"
/// rather than a hard failure — parity with the version-index reader.
pub fn parse_catalog_index_ndjson(body: &[u8]) -> Vec<CatalogIndexLine> {
    String::from_utf8_lossy(body)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<CatalogIndexLine>(l).ok())
        .collect()
}

/// Tokenless read client over a static registry tree's catalog surface.
///
/// Points at the tree ROOT (the directory that holds `slpkg/`, `cargo/`,
/// `catalog/`, …) — `file://<root>` for a local / CI tree or `http(s)://…`
/// for a static mount. Every read is anonymous; a configured `token` is sent
/// only for private mounts that gate generic reads behind auth. Serves both
/// the editor's node palette (via [`Self::fetch_processor_index`]) and a
/// browse UI (via [`Self::fetch_package_catalog`] +
/// [`Self::fetch_schema_type_definition`]).
pub struct CatalogClient {
    base_url: String,
    token: Option<String>,
}

impl CatalogClient {
    /// Build a client over a tree-root `base_url` (`file://…` or
    /// `http(s)://…`). `token` is optional and only used for private mounts.
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token,
        }
    }

    fn is_file_scheme(&self) -> bool {
        self.base_url.starts_with("file://")
    }

    fn file_root(&self) -> PathBuf {
        PathBuf::from(self.base_url.trim_start_matches("file://"))
    }

    /// Read a tree-relative file. `Ok(None)` when it is absent (missing file /
    /// HTTP 404); `Err` only on a real transport failure.
    fn fetch_relative(&self, rel: &str) -> ResolverResult<Option<Vec<u8>>> {
        if self.is_file_scheme() {
            let path = self.file_root().join(rel);
            match std::fs::read(&path) {
                Ok(b) => Ok(Some(b)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(ResolverError::RegistryFetchFailed {
                    name: rel.to_string(),
                    detail: format!("reading {} : {e}", path.display()),
                }),
            }
        } else {
            let url = format!("{}/{}", self.base_url, rel);
            crate::registry::http_get_optional(&url, self.token.as_deref()).map_err(|detail| {
                ResolverError::RegistryFetchFailed {
                    name: rel.to_string(),
                    detail: format!("fetching {url}: {detail}"),
                }
            })
        }
    }

    /// Fetch the registry-wide processor index (`catalog/index.ndjson`). An
    /// absent index yields an empty list — parity with the version index's
    /// missing-file case.
    pub fn fetch_processor_index(&self) -> ResolverResult<Vec<CatalogIndexLine>> {
        match self.fetch_relative(CATALOG_INDEX_PATH)? {
            Some(body) => Ok(parse_catalog_index_ndjson(&body)),
            None => Ok(Vec::new()),
        }
    }

    /// Fetch the per-package catalog for an exact `(package, version)`.
    /// `Ok(None)` when no catalog is published for that pair.
    pub fn fetch_package_catalog(
        &self,
        package: &PackageRef,
        version: &SemVer,
    ) -> ResolverResult<Option<PackageCatalog>> {
        let rel = format!(
            "slpkg/{}/{}/{}",
            package.name.as_str(),
            version,
            package_catalog_file_name(package.name.as_str()),
        );
        let Some(body) = self.fetch_relative(&rel)? else {
            return Ok(None);
        };
        let catalog =
            serde_json::from_slice(&body).map_err(|e| ResolverError::RegistryFetchFailed {
                name: rel,
                detail: format!("parsing package catalog JSON: {e}"),
            })?;
        Ok(Some(catalog))
    }

    /// Fetch the JSON Type Definition for a schema `ident`, from the OWNING
    /// package's version directory. `Ok(None)` when no JTD is published for
    /// that ident. Returned as a [`serde_json::Value`] — the JTD is a
    /// self-describing document the caller renders / validates against.
    pub fn fetch_schema_type_definition(
        &self,
        ident: &SchemaIdent,
    ) -> ResolverResult<Option<serde_json::Value>> {
        let rel = format!(
            "slpkg/{}/{}/schemas/{}",
            ident.package.as_str(),
            ident.version,
            schema_jtd_file_name(&ident.r#type),
        );
        let Some(body) = self.fetch_relative(&rel)? else {
            return Ok(None);
        };
        let value =
            serde_json::from_slice(&body).map_err(|e| ResolverError::RegistryFetchFailed {
                name: rel,
                detail: format!("parsing schema JTD JSON: {e}"),
            })?;
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ident::{Org, Package};

    fn ident(pkg: &str, ty: &str, v: SemVer) -> SchemaIdent {
        SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new(pkg).unwrap(),
            TypeName::new(ty).unwrap(),
            v,
        )
    }

    fn pkg_ref(name: &str) -> PackageRef {
        PackageRef::new(Org::new("tatolab").unwrap(), Package::new(name).unwrap())
    }

    fn sample_processor() -> CatalogProcessor {
        CatalogProcessor {
            name: "Camera".into(),
            description: Some("Captures video".into()),
            runtime: CatalogRuntime::Rust,
            entrypoint: None,
            config: Some(CatalogConfig {
                name: "config".into(),
                schema: ident("camera", "CameraConfig", SemVer::new(1, 0, 0)),
            }),
            inputs: vec![],
            outputs: vec![CatalogPort {
                name: "video".into(),
                description: Some("Live frames".into()),
                schema: CatalogSchemaRef::Schema(ident("core", "VideoFrame", SemVer::new(1, 0, 0))),
                delivery_profile: None,
            }],
        }
    }

    #[test]
    fn schema_ref_any_serializes_as_string_and_round_trips() {
        let any = CatalogSchemaRef::Any;
        let json = serde_json::to_string(&any).unwrap();
        assert_eq!(json, "\"any\"");
        let back: CatalogSchemaRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CatalogSchemaRef::Any);
    }

    #[test]
    fn schema_ref_specific_serializes_as_ident_map_and_round_trips() {
        let r = CatalogSchemaRef::Schema(ident("core", "VideoFrame", SemVer::new(1, 2, 3)));
        let json = serde_json::to_string(&r).unwrap();
        // Structured four-field map — never a joined shorthand string.
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["org"], "tatolab");
        assert_eq!(v["package"], "core");
        assert_eq!(v["type"], "VideoFrame");
        assert_eq!(v["version"], "1.2.3");
        let back: CatalogSchemaRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn schema_ref_rejects_non_any_string() {
        // A bare string that is NOT "any" is a resolution leak — reject it so
        // an unresolved bare name can never masquerade as a catalog schema.
        let err = serde_json::from_str::<CatalogSchemaRef>("\"VideoFrame\"").unwrap_err();
        assert!(err.to_string().contains("any"), "msg: {err}");
    }

    #[test]
    fn package_catalog_round_trips_through_json() {
        let catalog = PackageCatalog {
            package: pkg_ref("camera"),
            version: SemVer::new(1, 0, 0),
            processors: vec![sample_processor()],
        };
        let json = serde_json::to_string_pretty(&catalog).unwrap();
        let back: PackageCatalog = serde_json::from_str(&json).unwrap();
        assert_eq!(back, catalog);
    }

    #[test]
    fn index_ndjson_render_parse_round_trip() {
        let lines = vec![
            CatalogIndexLine {
                package: pkg_ref("camera"),
                version: SemVer::new(1, 0, 0),
                processor: sample_processor(),
            },
            CatalogIndexLine {
                package: pkg_ref("display"),
                version: SemVer::new(1, 0, 0),
                processor: CatalogProcessor {
                    name: "Display".into(),
                    description: None,
                    runtime: CatalogRuntime::Rust,
                    entrypoint: None,
                    config: None,
                    inputs: vec![CatalogPort {
                        name: "video_in".into(),
                        description: None,
                        schema: CatalogSchemaRef::Any,
                        delivery_profile: Some("latest".into()),
                    }],
                    outputs: vec![],
                },
            },
        ];
        let rendered = render_catalog_index_ndjson(&lines);
        assert_eq!(rendered.lines().count(), 2);
        assert!(rendered.ends_with('\n'));
        let parsed = parse_catalog_index_ndjson(rendered.as_bytes());
        assert_eq!(parsed, lines);
    }

    #[test]
    fn index_ndjson_ignores_unknown_fields_and_garbage_lines() {
        // Forward-compat: a line carrying an extra top-level field parses
        // fine (unknown fields ignored); blank + non-JSON lines are skipped.
        let base = CatalogIndexLine {
            package: pkg_ref("camera"),
            version: SemVer::new(1, 0, 0),
            processor: sample_processor(),
        };
        let mut obj: serde_json::Value = serde_json::to_value(&base).unwrap();
        obj["future_field_added_later"] = serde_json::json!({"whatever": 42});
        let ndjson = format!("\n{}\nnot-json\n\n", serde_json::to_string(&obj).unwrap());
        let parsed = parse_catalog_index_ndjson(ndjson.as_bytes());
        assert_eq!(parsed, vec![base]);
    }

    #[test]
    fn catalog_client_file_scheme_reads_index_and_package_and_jtd() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path();
        // Aggregate index.
        let line = CatalogIndexLine {
            package: pkg_ref("camera"),
            version: SemVer::new(1, 0, 0),
            processor: sample_processor(),
        };
        std::fs::create_dir_all(base.join("catalog")).unwrap();
        std::fs::write(
            base.join(CATALOG_INDEX_PATH),
            render_catalog_index_ndjson(std::slice::from_ref(&line)),
        )
        .unwrap();
        // Per-package catalog.
        let ver_dir = base.join("slpkg/camera/1.0.0");
        std::fs::create_dir_all(ver_dir.join("schemas")).unwrap();
        let catalog = PackageCatalog {
            package: pkg_ref("camera"),
            version: SemVer::new(1, 0, 0),
            processors: vec![sample_processor()],
        };
        std::fs::write(
            ver_dir.join(package_catalog_file_name("camera")),
            serde_json::to_vec_pretty(&catalog).unwrap(),
        )
        .unwrap();
        // Owned JTD (camera owns CameraConfig).
        let jtd = serde_json::json!({"metadata": {"type": "CameraConfig"}, "properties": {}});
        std::fs::write(
            ver_dir.join("schemas").join("CameraConfig.jtd.json"),
            serde_json::to_vec(&jtd).unwrap(),
        )
        .unwrap();

        let client = CatalogClient::new(format!("file://{}", base.display()), None);
        assert_eq!(client.fetch_processor_index().unwrap(), vec![line]);
        assert_eq!(
            client
                .fetch_package_catalog(&pkg_ref("camera"), &SemVer::new(1, 0, 0))
                .unwrap(),
            Some(catalog)
        );
        let fetched_jtd = client
            .fetch_schema_type_definition(&ident("camera", "CameraConfig", SemVer::new(1, 0, 0)))
            .unwrap()
            .unwrap();
        assert_eq!(fetched_jtd["metadata"]["type"], "CameraConfig");
    }

    #[test]
    fn catalog_client_missing_index_and_package_are_empty_not_error() {
        let root = tempfile::tempdir().unwrap();
        let client = CatalogClient::new(format!("file://{}", root.path().display()), None);
        assert!(client.fetch_processor_index().unwrap().is_empty());
        assert!(
            client
                .fetch_package_catalog(&pkg_ref("nope"), &SemVer::new(9, 9, 9))
                .unwrap()
                .is_none()
        );
        assert!(
            client
                .fetch_schema_type_definition(&ident("nope", "Nope", SemVer::new(9, 9, 9)))
                .unwrap()
                .is_none()
        );
    }
}
