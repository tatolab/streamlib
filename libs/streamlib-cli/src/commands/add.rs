// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib add` / `streamlib remove` — single-package adoption, the
//! `npm install <pkg>` of streamlib.
//!
//! Thin CLI wrappers over the programmatic [`streamlib::sdk::runtime::add`] /
//! [`remove`] so the CLI and any embedding host share one adoption flow. `add`
//! resolves a package by version from the registry (or extracts a local
//! `.slpkg`), materializes it into the installed-package cache, records it in
//! `packages.yaml`, and prints a catalog-backed summary of the processors it
//! contributes and their typed ports. `remove` reverses it.
//!
//! This is deliberately NOT `streamlib install`: it touches no app code, no
//! app `streamlib.yaml`, and no application lockfile.

use anyhow::{Context, Result};
use streamlib::sdk::runtime::{
    add as sdk_add, add_slpkg as sdk_add_slpkg, remove as sdk_remove, AddOptions, AddReport,
    BuildEvent, BuildEventSink, BuildStream,
};
use streamlib::sdk::PolyglotBuildOrchestrator;
use streamlib_idents::{CatalogSchemaRef, PackageRef, SemVerRange};

/// Routes the orchestrator's build diagnostics to the CLI's stdout/stderr
/// during `add` (mirrors `install`'s interactive progress).
struct CliBuildSink;

impl BuildEventSink for CliBuildSink {
    fn emit(&self, event: BuildEvent) {
        match event {
            BuildEvent::Started { language } => println!("    [{language}] build started"),
            BuildEvent::Line { stream, line } => match stream {
                BuildStream::Stdout => println!("    {line}"),
                BuildStream::Stderr => eprintln!("    {line}"),
            },
            BuildEvent::Finished { language } => println!("    [{language}] build finished"),
            _ => {}
        }
    }
}

/// Add one package: a registry ref `@org/name[@version-req]` (resolved by
/// version from the registry, with a catalog-backed summary), a local `.slpkg`
/// path, or an HTTP URL.
pub async fn add(spec: &str) -> Result<()> {
    let orchestrator = PolyglotBuildOrchestrator::default();
    let sink = CliBuildSink;

    let report = if spec.starts_with('@') {
        let (pkg_ref, version_req) = parse_registry_ref(spec)?;
        println!("Adding {pkg_ref} ({version_req})…");
        // Zero-env by default: AddOptions::default() resolves the registry from
        // the environment, else the first-party DEFAULT_REGISTRY_URL.
        sdk_add(&pkg_ref, &version_req, &orchestrator, &sink, &AddOptions::default())
            .map_err(|e| anyhow::anyhow!("add failed: {e}"))?
    } else if spec.starts_with("http://") || spec.starts_with("https://") {
        let path = download_to_temp(spec).await?;
        println!("Adding {}…", path.display());
        let report = sdk_add_slpkg(&path, &orchestrator, &sink)
            .map_err(|e| anyhow::anyhow!("add failed: {e}"))?;
        let _ = std::fs::remove_file(&path);
        report
    } else {
        let path = std::path::PathBuf::from(spec);
        if !path.exists() {
            anyhow::bail!("File not found: {spec}");
        }
        println!("Adding {}…", path.display());
        sdk_add_slpkg(&path, &orchestrator, &sink)
            .map_err(|e| anyhow::anyhow!("add failed: {e}"))?
    };

    print_add_report(&report);
    Ok(())
}

/// Remove one installed package by its canonical `@org/name` ref — un-record it
/// from `packages.yaml` and evict its cache slot.
pub fn remove(name: &str) -> Result<()> {
    let pkg_ref = parse_canonical_package_ref(name)?;
    let report = sdk_remove(&pkg_ref).map_err(|e| anyhow::anyhow!("remove failed: {e}"))?;
    println!("Removed {} v{}", report.package, report.version);
    if report.cache_dir_removed {
        println!("  Evicted cache: {}", report.cache_dir.display());
    }
    Ok(())
}

/// Pretty-print the add outcome plus the catalog-backed discovery summary.
fn print_add_report(report: &AddReport) {
    println!();
    let verb = if report.already_present { "Already added" } else { "Added" };
    println!("{verb} {} v{}", report.package, report.version);
    println!("  Cache: {}", report.cache_dir.display());

    match &report.catalog {
        Some(catalog) if !catalog.processors.is_empty() => {
            println!();
            println!("Processors ({}):", catalog.processors.len());
            for proc in &catalog.processors {
                let desc = proc
                    .description
                    .as_deref()
                    .map(|d| format!(" — {d}"))
                    .unwrap_or_default();
                println!("  {}{}  [{:?}]", proc.name, desc, proc.runtime);
                if !proc.inputs.is_empty() {
                    println!("    Inputs:");
                    for port in &proc.inputs {
                        println!("      - {} ({})", port.name, schema_label(&port.schema));
                    }
                }
                if !proc.outputs.is_empty() {
                    println!("    Outputs:");
                    for port in &proc.outputs {
                        println!("      - {} ({})", port.name, schema_label(&port.schema));
                    }
                }
                if let Some(cfg) = &proc.config {
                    println!("    Config: {} ({})", cfg.name, cfg.schema);
                }
            }
        }
        Some(_) => {
            println!();
            println!("Catalog present, but the package declares no processors.");
        }
        None => {
            println!();
            println!("No catalog metadata (this registry publishes no catalog for the package).");
        }
    }
}

/// A human-readable label for a catalog port schema: `any` for the wildcard,
/// the fully-qualified `@org/name/Type@version` for a concrete schema.
fn schema_label(schema: &CatalogSchemaRef) -> String {
    match schema {
        CatalogSchemaRef::Any => "any".to_string(),
        CatalogSchemaRef::Schema(ident) => ident.to_string(),
    }
}

/// Parse `@org/name[@version-req]` → `(PackageRef, SemVerRange)`. A missing
/// version requirement means [`SemVerRange::Any`] (`*`, the newest release).
fn parse_registry_ref(spec: &str) -> Result<(PackageRef, SemVerRange)> {
    let body = &spec[1..]; // strip the leading '@'
    let (ref_str, version_req) = match body.split_once('@') {
        Some((r, v)) => (
            format!("@{r}"),
            SemVerRange::from_str(v)
                .map_err(|e| anyhow::anyhow!("invalid version '{v}' in '{spec}': {e}"))?,
        ),
        None => (format!("@{body}"), SemVerRange::Any),
    };
    Ok((parse_canonical_package_ref(&ref_str)?, version_req))
}

/// Convert a canonical-form string (`@org/name`) into a typed [`PackageRef`]
/// via the official Deserialize path, wrapping the round-trip with a
/// CLI-friendly error.
fn parse_canonical_package_ref(arg: &str) -> Result<PackageRef> {
    serde_yaml::from_value::<PackageRef>(serde_yaml::Value::String(arg.to_string())).with_context(
        || {
            format!(
                "Invalid canonical package reference '{arg}'. Expected `@org/name` form \
                 (e.g. `@tatolab/core`)."
            )
        },
    )
}

/// Download an HTTP(S) `.slpkg` to a temp file and return its path.
async fn download_to_temp(url: &str) -> Result<std::path::PathBuf> {
    println!("Downloading {url}…");
    let response = reqwest::get(url)
        .await
        .with_context(|| format!("Failed to download {url}"))?;
    if !response.status().is_success() {
        anyhow::bail!("Download failed: HTTP {} for {url}", response.status());
    }
    let bytes = response
        .bytes()
        .await
        .with_context(|| "Failed to read response body")?;
    let temp_path = std::env::temp_dir().join(unique_download_temp_filename());
    std::fs::write(&temp_path, &bytes)
        .with_context(|| format!("Failed to write temp file {}", temp_path.display()))?;
    Ok(temp_path)
}

/// A unique temp filename for a URL-add download — pid + a process-local
/// counter so two concurrent URL-adds (even in the same process) never write
/// to the same path and clobber each other.
fn unique_download_temp_filename() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static DOWNLOAD_SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "streamlib-add-download-{}-{}.slpkg",
        std::process::id(),
        DOWNLOAD_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_registry_ref_without_version_is_any() {
        let (pkg_ref, req) = parse_registry_ref("@tatolab/foo").unwrap();
        assert_eq!(pkg_ref.org.as_str(), "tatolab");
        assert_eq!(pkg_ref.name.as_str(), "foo");
        // No `@version` suffix ⇒ the newest release.
        assert_eq!(req, SemVerRange::Any);
    }

    #[test]
    fn parse_registry_ref_with_version_parses_range() {
        let (pkg_ref, req) = parse_registry_ref("@tatolab/foo@^1.2.0").unwrap();
        assert_eq!(pkg_ref.name.as_str(), "foo");
        assert_eq!(req, SemVerRange::from_str("^1.2.0").unwrap());
    }

    #[test]
    fn parse_registry_ref_rejects_bad_version() {
        // A malformed version requirement must fail loud, not silently become Any.
        assert!(parse_registry_ref("@tatolab/foo@not-a-version").is_err());
    }

    #[test]
    fn parse_canonical_package_ref_rejects_bare_name() {
        // The canonical `@org/name` form is required — a bare short name is
        // ambiguous across orgs and must be rejected.
        assert!(parse_canonical_package_ref("foo").is_err());
        assert!(parse_canonical_package_ref("@tatolab/foo").is_ok());
    }

    #[test]
    fn unique_download_temp_filename_is_distinct_and_pid_scoped() {
        let a = unique_download_temp_filename();
        let b = unique_download_temp_filename();
        // Two successive calls must never collide — the whole point of the fix
        // (two concurrent URL-adds must not clobber one download temp file).
        // Mentally-revert to a fixed name and this fails.
        assert_ne!(a, b, "concurrent URL-add downloads must not share a temp filename");
        let pid = std::process::id().to_string();
        assert!(
            a.contains(&pid) && b.contains(&pid),
            "temp filename must be pid-scoped: {a} / {b}"
        );
        assert!(a.ends_with(".slpkg") && b.ends_with(".slpkg"));
    }

    #[test]
    fn schema_label_renders_any_and_concrete() {
        assert_eq!(schema_label(&CatalogSchemaRef::Any), "any");
        let ident = streamlib_idents::SchemaIdent::new(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new("core").unwrap(),
            streamlib_idents::TypeName::new("VideoFrame").unwrap(),
            streamlib_idents::SemVer::new(1, 0, 0),
        );
        assert_eq!(
            schema_label(&CatalogSchemaRef::Schema(ident)),
            "@tatolab/core/VideoFrame@1.0.0"
        );
    }
}
