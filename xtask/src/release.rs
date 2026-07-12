// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Release-closure emission + release-manifest publish.
//!
//! Two verbs back the atomic-release publish flow:
//!
//! - `release-closure --json` — the single canonical definition of "the
//!   crates a release publishes," emitted for `cargo xtask static-registry emit --cargo-closure`
//!   to consume so the closure is defined once, in code, not in a script.
//! - `release-manifest-publish` — writes the [`ReleaseManifest`] to the
//!   registry **last**, after every crate / SDK / package has landed. Its
//!   presence marks the release complete; a consumer resolving against a
//!   half-published registry detects the gap up front.

use std::path::Path;

use anyhow::{Context, Result};
use streamlib_idents::{
    RegistryClient, RegistryConfig, ReleaseManifest, ReleaseManifestMember,
};
use streamlib_pack::compute_release_closure;

/// Read `[workspace.package].version` from the workspace `Cargo.toml`.
fn workspace_version(workspace_root: &Path) -> Result<String> {
    let path = workspace_root.join("Cargo.toml");
    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let doc: toml::Value =
        toml::from_str(&body).with_context(|| format!("parsing {}", path.display()))?;
    doc.get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .context("[workspace.package].version missing from workspace Cargo.toml")
}

/// Apply the `-dev.N` suffix to a base version when `dev` is set.
fn target_version(base: &str, dev: Option<u32>) -> String {
    match dev {
        Some(n) => format!("{base}-dev.{n}"),
        None => base.to_string(),
    }
}

/// The registry org the release lives under (`STREAMLIB_REGISTRY_ORG`, default `tatolab`).
fn registry_org() -> String {
    std::env::var("STREAMLIB_REGISTRY_ORG")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "tatolab".to_string())
}

/// Emit the release closure as JSON on stdout for the publish scripts.
/// Shape: `{"release_version","crates":[{"name","version","manifest_dir"}]}`,
/// with `crates` in topological publish order.
pub fn emit_closure_json(workspace_root: &Path, dev: Option<u32>) -> Result<()> {
    let base = workspace_version(workspace_root)?;
    let target = target_version(&base, dev);
    let closure = compute_release_closure(workspace_root)?;
    let crates: Vec<serde_json::Value> = closure
        .crates
        .iter()
        .map(|c| {
            // All closure crates inherit the workspace version, so a --dev
            // publish stamps every one at the target; record what's published.
            let version = if dev.is_some() { target.clone() } else { c.version.clone() };
            serde_json::json!({
                "name": c.name,
                "version": version,
                "manifest_dir": c.manifest_dir,
            })
        })
        .collect();
    let out = serde_json::json!({
        "release_version": target,
        "crates": crates,
    });
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// A polyglot package's release member (`@org/name` @ version), read from its
/// `streamlib.yaml`.
fn enumerate_packages(workspace_root: &Path) -> Result<Vec<ReleaseManifestMember>> {
    #[derive(serde::Deserialize)]
    struct Pkg {
        org: String,
        name: String,
        version: String,
    }
    #[derive(serde::Deserialize)]
    struct Yaml {
        package: Option<Pkg>,
    }

    let packages_dir = workspace_root.join("packages");
    let mut members = Vec::new();
    if !packages_dir.is_dir() {
        return Ok(members);
    }
    for entry in std::fs::read_dir(&packages_dir)
        .with_context(|| format!("reading {}", packages_dir.display()))?
    {
        let entry = entry?;
        let yaml_path = entry.path().join("streamlib.yaml");
        if !yaml_path.is_file() {
            continue;
        }
        let body = std::fs::read_to_string(&yaml_path)
            .with_context(|| format!("reading {}", yaml_path.display()))?;
        let parsed: Yaml = serde_yaml::from_str(&body)
            .with_context(|| format!("parsing {}", yaml_path.display()))?;
        if let Some(p) = parsed.package {
            members.push(ReleaseManifestMember::new(
                format!("@{}/{}", p.org, p.name),
                p.version,
            ));
        }
    }
    members.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(members)
}

/// Options for [`publish_manifest`], mirroring the publish scripts' `SKIP_*`
/// guards so the manifest records exactly what was published.
pub struct PublishManifestOptions {
    pub dev: Option<u32>,
    pub skip_python: bool,
    pub skip_deno: bool,
    pub skip_packages: bool,
}

/// Build the [`ReleaseManifest`] for the current workspace and publish it to
/// the configured registry — the atomicity flip run **last** in the release
/// sequence.
pub fn publish_manifest(workspace_root: &Path, opts: &PublishManifestOptions) -> Result<()> {
    let base = workspace_version(workspace_root)?;
    let target = target_version(&base, opts.dev);

    let closure = compute_release_closure(workspace_root)?;
    let crate_members: Vec<ReleaseManifestMember> = closure
        .crates
        .iter()
        .map(|c| {
            let version = if opts.dev.is_some() { target.clone() } else { c.version.clone() };
            ReleaseManifestMember::new(c.name.clone(), version)
        })
        .collect();

    let mut manifest = ReleaseManifest::new(target.clone(), crate_members);
    if !opts.skip_python {
        manifest.python = Some(target.clone());
    }
    if !opts.skip_deno {
        manifest.deno = Some(target.clone());
    }
    if !opts.skip_packages {
        manifest.packages = enumerate_packages(workspace_root)?;
    }

    let config = RegistryConfig::from_env().context(
        "no registry configured — set STREAMLIB_REGISTRY_URL (or GITEA_URL) and \
         STREAMLIB_REGISTRY_TOKEN to publish the release manifest",
    )?;
    let org = registry_org();
    let url = RegistryClient::new(&config)
        .upload_release_manifest(&org, &manifest)
        .context("uploading release manifest")?;
    tracing::info!(
        release_version = %target,
        crates = manifest.crates.len(),
        packages = manifest.packages.len(),
        url = %url,
        "published release manifest (release marked complete)"
    );
    Ok(())
}
