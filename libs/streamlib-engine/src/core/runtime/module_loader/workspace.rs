// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::errors::LoadWorkspacePackagesError;

/// Parsed `@<org>/<name>` canonical id. Only the slice references are
/// kept — the caller's input must outlive this struct.
#[derive(Debug)]
pub(super) struct CanonicalPackageId<'a> {
    pub(super) org_str: &'a str,
    pub(super) name_str: &'a str,
}

pub(super) fn parse_canonical_package_id(
    name: &str,
) -> std::result::Result<CanonicalPackageId<'_>, LoadWorkspacePackagesError> {
    // Strip the leading '@', split on first '/', then route the
    // halves through the typed `streamlib-idents` validators so
    // charset / leading-letter / length rules apply here too. We
    // don't surface the typed-parser's stringy diagnostic — the
    // typed `InvalidPackageId` variant is what callers match on —
    // but using the validators means a name like `@TaToLaB/CAMERA`
    // fails fast here rather than at the filesystem-lookup stage.
    let stripped = name
        .strip_prefix('@')
        .ok_or_else(|| LoadWorkspacePackagesError::InvalidPackageId(name.to_string()))?;
    let (org, pkg) = stripped
        .split_once('/')
        .ok_or_else(|| LoadWorkspacePackagesError::InvalidPackageId(name.to_string()))?;
    if org.is_empty() || pkg.is_empty() || pkg.contains('/') {
        return Err(LoadWorkspacePackagesError::InvalidPackageId(name.to_string()));
    }
    streamlib_idents::Org::new(org)
        .map_err(|_| LoadWorkspacePackagesError::InvalidPackageId(name.to_string()))?;
    streamlib_idents::Package::new(pkg)
        .map_err(|_| LoadWorkspacePackagesError::InvalidPackageId(name.to_string()))?;
    Ok(CanonicalPackageId { org_str: org, name_str: pkg })
}

pub(super) fn resolve_workspace_root(
) -> std::result::Result<std::path::PathBuf, LoadWorkspacePackagesError> {
    // Env-var override wins when set AND the path resolves — the env
    // var IS the user's intent, so a typo'd path should surface as a
    // precise error rather than silently falling through to cargo.
    if let Ok(env_root) = std::env::var("STREAMLIB_WORKSPACE_ROOT") {
        let path = std::path::PathBuf::from(&env_root);
        return if path.is_dir() {
            Ok(path)
        } else {
            Err(LoadWorkspacePackagesError::WorkspaceRootNotFound)
        };
    }

    let output = std::process::Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .map_err(|_| LoadWorkspacePackagesError::WorkspaceRootNotFound)?;
    if !output.status.success() {
        return Err(LoadWorkspacePackagesError::WorkspaceRootNotFound);
    }
    let manifest_path = String::from_utf8(output.stdout)
        .map_err(|_| LoadWorkspacePackagesError::WorkspaceRootNotFound)?;
    let trimmed = manifest_path.trim();
    if trimmed.is_empty() {
        return Err(LoadWorkspacePackagesError::WorkspaceRootNotFound);
    }
    std::path::PathBuf::from(trimmed)
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or(LoadWorkspacePackagesError::WorkspaceRootNotFound)
}
