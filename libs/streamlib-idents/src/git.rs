// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Git checkout helper for `git:` dependency / patch sources.
//!
//! Shared between the build-time resolver (`streamlib_idents::resolve`)
//! and the runtime's consumer-patch resolution path
//! (`streamlib::sdk::runtime::runtime`). Single source of truth for
//! "clone a pinned-rev git URL into a content-addressed cache dir";
//! both contexts go through this so they share the same checkout
//! locations and errors.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{ResolverError, ResolverResult};
use crate::manifest::Manifest;

/// Clone `url` at `rev` into `cache_dir/git/<safe-url>_<rev>` and return
/// the absolute path. Idempotent — when the target dir already contains
/// a `streamlib.yaml` (a previous fetch succeeded), returns immediately
/// without re-cloning.
///
/// `name` is the canonical `@org/name` of the dep being fetched, used
/// only in error messages.
pub fn fetch_git(name: &str, url: &str, rev: &str, cache_dir: &Path) -> ResolverResult<PathBuf> {
    let safe = url
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect::<String>();
    let target = cache_dir.join("git").join(format!("{}_{}", safe, rev));

    let manifest_path = target.join(Manifest::FILE_NAME);
    if manifest_path.exists() {
        return Ok(target);
    }

    std::fs::create_dir_all(&target).map_err(|e| ResolverError::Io {
        path: target.clone(),
        source: e,
    })?;

    let clone = Command::new("git")
        .args(["clone", "--quiet", url, "."])
        .current_dir(&target)
        .output()
        .map_err(|e| ResolverError::GitDependencyFailed {
            name: name.to_string(),
            url: url.to_string(),
            message: format!("git clone invocation failed: {e}"),
        })?;
    if !clone.status.success() {
        return Err(ResolverError::GitDependencyFailed {
            name: name.to_string(),
            url: url.to_string(),
            message: String::from_utf8_lossy(&clone.stderr).trim().to_string(),
        });
    }

    let checkout = Command::new("git")
        .args(["checkout", "--quiet", rev])
        .current_dir(&target)
        .output()
        .map_err(|e| ResolverError::GitDependencyFailed {
            name: name.to_string(),
            url: url.to_string(),
            message: format!("git checkout invocation failed: {e}"),
        })?;
    if !checkout.status.success() {
        return Err(ResolverError::GitDependencyFailed {
            name: name.to_string(),
            url: url.to_string(),
            message: format!(
                "git checkout {} failed: {}",
                rev,
                String::from_utf8_lossy(&checkout.stderr).trim()
            ),
        });
    }

    Ok(target)
}
