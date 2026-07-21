// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Minting fresh `@session/<name>@0.0.N` identities for in-app /
//! session-local processors registered live at runtime.
//!
//! A processor registered live (the in-app authoring path, `add_local`)
//! lives under the [`crate::SESSION_ORG`] org so it never collides with an
//! installed `@org/…` package on the registry key. Each registration is
//! stamped a fresh `0.0.N` version from a process-global monotonic counter,
//! so re-registering the same name after a removal yields a distinct
//! version rather than reusing a stale ident.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::SESSION_ORG;
use crate::error::IdentResult;
use crate::ident::{ModuleIdent, Org, Package};
use crate::semver::{SemVer, SemVerRange};

/// Process-global monotonic counter that stamps the `patch` of each minted
/// `@session/<name>@0.0.N` version. Starts at 0 and only ever advances, so
/// two mints in one process never share a version — even for the same name
/// across an add/remove/add cycle.
///
/// The counter is process-scoped and restarts at 0 each process. So that a
/// restart cannot re-mint `0.0.0` onto a `session-source/<name>/0.0.0/` staging
/// dir surviving from a prior run, the engine reclaims the whole session-source
/// staging tree once at runtime start (before any mint) — see
/// `module_loader::reclaim_session_source_staging_root_once`. Cross-run
/// uniqueness therefore comes from starting each process on a clean staging
/// tree, not from persisting the counter.
static NEXT_SESSION_VERSION_PATCH: AtomicU32 = AtomicU32::new(0);

/// The concrete next `0.0.N` release version from the session counter. The
/// major/minor stay `0.0` by convention (session identities are ephemeral
/// and never published); `N` is the monotonic patch.
pub fn next_session_module_version() -> SemVer {
    let n = NEXT_SESSION_VERSION_PATCH.fetch_add(1, Ordering::Relaxed);
    SemVer::new(0, 0, n)
}

/// A freshly-minted session identity: the `@session/<name>@0.0.N` module
/// ident, its concrete `0.0.N` version, and the validated `@session/<name>`
/// package. Returned by [`mint_session_module_ident`].
#[derive(Debug, Clone)]
pub struct MintedSessionIdent {
    /// `@session/<name>@=0.0.N` — the imperative module ident carried by an
    /// `AddedModule` load handle.
    pub module: ModuleIdent,
    /// The concrete `0.0.N` version stamped from the monotonic counter.
    pub version: SemVer,
    /// The validated `<name>` package segment.
    pub package: Package,
}

/// Mint a fresh `@session/<name>@0.0.N` module identity from a package-name
/// segment (typically a kebab-cased processor type name).
///
/// The `name` must satisfy the package grammar (`[a-z][a-z0-9-]*`) — a
/// malformed name is a loud typed error, never a silently-mangled ident.
/// The version is stamped from the process-global monotonic counter, so two
/// calls (even with the same `name`) never share a version.
pub fn mint_session_module_ident(name: &str) -> IdentResult<MintedSessionIdent> {
    let org = Org::new(SESSION_ORG).expect("SESSION_ORG passes the org grammar by construction");
    let package = Package::new(name)?;
    let version = next_session_module_version();
    let module = ModuleIdent::new(org, package.clone(), SemVerRange::Exact(version));
    Ok(MintedSessionIdent {
        module,
        version,
        package,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_stamps_a_monotonically_increasing_version() {
        // Two mints of the SAME name must produce DISTINCT versions — this is
        // what lets an add/remove/add cycle re-register a name without reusing
        // a stale ident. Revert the `fetch_add` to a constant and the two
        // versions collide, failing this assertion.
        let first = mint_session_module_ident("my-processor").expect("valid name mints");
        let second = mint_session_module_ident("my-processor").expect("valid name mints");
        assert_eq!(first.package.as_str(), "my-processor");
        assert_eq!(first.module.org.as_str(), SESSION_ORG);
        assert!(
            second.version.patch > first.version.patch,
            "the session version counter must be monotonic: {} !> {}",
            second.version.patch,
            first.version.patch
        );
    }

    #[test]
    fn mint_rejects_a_malformed_name() {
        // A leading uppercase / underscore fails the package grammar — the mint
        // surfaces the typed error rather than building an invalid ident.
        assert!(mint_session_module_ident("_bad").is_err());
        assert!(mint_session_module_ident("Bad").is_err());
        assert!(mint_session_module_ident("").is_err());
    }

    #[test]
    fn minted_module_ident_renders_under_the_session_org() {
        let minted = mint_session_module_ident("camera").expect("valid name mints");
        let rendered = minted.module.to_string();
        assert!(
            rendered.starts_with("@session/camera@="),
            "expected @session/camera@=0.0.N, got {rendered}"
        );
    }
}
