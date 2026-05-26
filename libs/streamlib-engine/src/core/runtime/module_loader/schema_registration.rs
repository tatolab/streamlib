// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Error, Result};

/// Iterate `config.schemas` map entries, registering each `Local` schema
/// (the YAML body keyed by its canonical identifier) with the engine's
/// runtime schema registry. `External` entries are import declarations
/// owned by other packages and are skipped here — the dep's own
/// `register_package_schemas` call handles them when its manifest loads.
/// No-op when the manifest declares no `schemas:` map.
pub(super) fn register_package_schemas(
    project_path: &std::path::Path,
    config: &crate::core::config::ProjectConfig,
) -> Result<()> {
    use crate::core::config::ProjectConfig;
    use crate::core::embedded_schemas;
    use streamlib_idents::SchemaEntry;

    let Some(schemas) = config.schemas.as_ref() else {
        return Ok(());
    };

    if schemas.is_empty() {
        return Ok(());
    }

    let pkg_meta = config.package.as_ref().ok_or_else(|| {
        Error::Configuration(format!(
            "{} at {} declares `schemas:` but is missing a `package:` block. \
             Schema canonical identifiers are composed from \
             `package.{{org, name}}`.",
            ProjectConfig::FILE_NAME,
            project_path.display(),
        ))
    })?;

    for (_name, entry) in schemas {
        let SchemaEntry::Local { file } = entry else {
            continue;
        };
        let schema_path = if file.is_absolute() {
            file.clone()
        } else {
            project_path.join(file)
        };
        let body = std::fs::read_to_string(&schema_path).map_err(|e| {
            Error::Configuration(format!(
                "failed to read schema declared in {}: {}: {}",
                ProjectConfig::FILE_NAME,
                schema_path.display(),
                e
            ))
        })?;
        let canonical = canonical_identifier_for_schema(&body, pkg_meta, &schema_path)?;
        tracing::debug!(
            "registering schema '{}' from {}",
            canonical,
            schema_path.display()
        );
        embedded_schemas::register_schema(canonical, body);
    }
    Ok(())
}

/// Resolve a processor's bare-name config schema reference (#767) to
/// its canonical id string. Walks the manifest's `schemas:` map via
/// the supplied resolver output, locates the owning package + schema
/// file, then reads the schema's `metadata.type` (new shape) or
/// `metadata.name` (legacy reverse-DNS) to compose the canonical id.
///
/// `resolved` is `None` only when the project's processors all lack a
/// `config:` block — reaching this function with `None` is a runtime
/// bug.
pub(super) fn resolve_config_schema_canonical_id(
    project_path: &std::path::Path,
    config: &streamlib_processor_schema::ProcessorConfigSchema,
    resolved: &Option<streamlib_idents::ResolvedPackages>,
) -> std::result::Result<String, String> {
    let resolved = resolved.as_ref().ok_or_else(|| {
        format!(
            "internal error: config-schema resolution requested but \
             dependency graph for {} was not pre-resolved",
            project_path.display()
        )
    })?;

    let (owner, schema_path) = streamlib_idents::resolve_bare_schema_name(
        resolved,
        &resolved.root,
        &config.schema,
    )
    .map_err(|e| format!("bare-name resolution failed: {}", e))?;

    let owner_pkg = owner
        .manifest
        .package
        .as_ref()
        .ok_or_else(|| "owning package has no `package:` block".to_string())?;

    // Read the schema's metadata to determine the canonical id form.
    let body = std::fs::read_to_string(&schema_path)
        .map_err(|e| format!("failed to read schema {}: {}", schema_path.display(), e))?;
    let value: serde_yaml::Value = serde_yaml::from_str(&body)
        .map_err(|e| format!("failed to parse schema {}: {}", schema_path.display(), e))?;
    let metadata = value
        .get("metadata")
        .ok_or_else(|| format!("schema {} missing `metadata` block", schema_path.display()))?;

    if let Some(type_str) = metadata.get("type").and_then(|t| t.as_str()) {
        Ok(format!(
            "@{}/{}/{}@{}",
            owner_pkg.org.as_str(),
            owner_pkg.name.as_str(),
            type_str,
            owner_pkg.version,
        ))
    } else if let Some(name_str) = metadata.get("name").and_then(|n| n.as_str()) {
        // Legacy reverse-DNS form — append the owning package's semver.
        Ok(format!("{}@{}", name_str, owner_pkg.version))
    } else {
        Err(format!(
            "schema {} declares neither `metadata.type` nor `metadata.name`",
            schema_path.display()
        ))
    }
}

/// Resolve a schema's canonical (unversioned) lookup key from its YAML
/// body + the enclosing package's metadata. Mirrors
/// `build.rs::resolve_canonical_identifier` so the engine const path
/// and the runtime registration path produce identical keys.
fn canonical_identifier_for_schema(
    body: &str,
    pkg_meta: &crate::core::config::PackageMetadata,
    schema_path: &std::path::Path,
) -> Result<String> {
    let value: serde_yaml::Value = serde_yaml::from_str(body).map_err(|e| {
        Error::Configuration(format!(
            "failed to parse schema {}: {}",
            schema_path.display(),
            e
        ))
    })?;

    let metadata = value.get("metadata").ok_or_else(|| {
        Error::Configuration(format!(
            "schema {} missing `metadata` block",
            schema_path.display()
        ))
    })?;

    if let Some(type_name) = metadata.get("type").and_then(|t| t.as_str()) {
        return Ok(format!(
            "@{}/{}/{}",
            pkg_meta.org.as_str(),
            pkg_meta.name.as_str(),
            type_name
        ));
    }

    if let Some(name) = metadata.get("name").and_then(|n| n.as_str()) {
        return Ok(strip_legacy_semver_suffix(name).to_string());
    }

    Err(Error::Configuration(format!(
        "schema {} must declare either `metadata.type` (new shape) or \
         `metadata.name` (legacy reverse-DNS) — required for runtime \
         registration",
        schema_path.display()
    )))
}

/// Strip a trailing `@MAJOR.MINOR.PATCH` semver suffix. Mirrors
/// `embedded_schemas::strip_semver_suffix` (kept private here to avoid
/// introducing a cross-module dep on a tiny string helper).
fn strip_legacy_semver_suffix(name: &str) -> &str {
    if let Some(at_pos) = name.rfind('@') {
        let suffix = &name[at_pos + 1..];
        let parts: Vec<&str> = suffix.split('.').collect();
        if parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            return &name[..at_pos];
        }
    }
    name
}
