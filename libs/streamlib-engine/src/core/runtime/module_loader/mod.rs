// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Module-loading subsystem: typed [`ModuleResolverStrategy`] enum
//! covering every historical loader behavior as a named variant,
//! plus the `Runner` public API surface that routes every entry
//! point through the unified [`add_module_with`] flow.
//!
//! Files in this directory:
//!
//! - [`errors`] — `AddModuleError`, `RemoveModuleError`,
//!   `LoadWorkspacePackagesError` typed enums.
//! - [`strategy`] — the [`ModuleResolverStrategy`] enum + the
//!   `(strategy, package_ref) → (manifest_dir, version)` resolver.
//! - [`recursive_walker`] — the recursive dep walker, cycle
//!   detection, and the dep-spec → strategy derivation.
//! - [`processor_registration`] — manifest-driven processor
//!   registration (Python venv prep, Rust cdylib dlopen,
//!   `PROCESSOR_REGISTRY.register_dynamic`).
//! - [`schema_registration`] — manifest-driven schema registration
//!   + bare-name config-schema resolution.
//! - [`slpkg`] — `.slpkg` archive extraction.
//! - [`workspace`] — `@org/name` parsing + workspace-root resolution.
//!
//! [`add_module_with`]: super::Runner::add_module_with

use std::collections::HashSet;

use super::Runner;
use crate::core::Result;

mod errors;
mod processor_registration;
mod recursive_walker;
mod schema_registration;
mod slpkg;
mod strategy;
mod workspace;

#[cfg(test)]
mod tests;

pub use errors::{AddModuleError, LoadWorkspacePackagesError, RemoveModuleError};
pub use processor_registration::host_target_triple;
pub use slpkg::extract_slpkg_to_cache;
pub use strategy::ModuleResolverStrategy;

impl Runner {
    // =========================================================================
    // Module Loading — public API surface
    // =========================================================================
    //
    // Every module-loading entry point on `Runner` routes through
    // `add_module_with(ident, strategy)`. The three deprecated methods
    // (`load_project` / `load_package` / `load_workspace_packages`) stay
    // functional as one-line wrappers that construct the matching
    // `ModuleResolverStrategy` variant and dispatch through the unified
    // resolver. They will move behind a non-public visibility (or be
    // deleted) in a later cleanup; the body lives in the strategy
    // resolver below.

    /// Extract a `.slpkg` archive, then load the manifest it contains.
    ///
    /// One-line wrapper around [`Self::add_module_with`] with
    /// [`ModuleResolverStrategy::SlpkgArchive`]. The strategy's resolver
    /// extracts the archive to the package cache, reads the embedded
    /// manifest, then drives the unified module-load flow.
    pub fn load_package(&self, slpkg_path: impl AsRef<std::path::Path>) -> Result<()> {
        let slpkg_path = slpkg_path.as_ref().to_path_buf();
        // Pre-read the manifest so we can build the strict
        // `ModuleIdent` the unified flow requires. The `.slpkg`'s
        // embedded `streamlib.yaml` is authoritative for the identity
        // — `add_module_with` then re-verifies that the extracted
        // manifest matches what we declared here, so the round-trip is
        // self-checking.
        let ident = strategy::read_module_ident_from_slpkg(&slpkg_path)?;
        self.add_module_with(
            ident,
            ModuleResolverStrategy::SlpkgArchive { path: slpkg_path },
        )?;
        Ok(())
    }

    /// Load the manifest at a directory containing `streamlib.yaml`.
    ///
    /// One-line wrapper around [`Self::add_module_with`] with
    /// [`ModuleResolverStrategy::ManifestDirectory`]. The strategy's
    /// resolver returns the directory as-is; the manifest's own
    /// `[package]` block supplies the identity the unified flow
    /// validates against.
    pub fn load_project(&self, project_path: impl AsRef<std::path::Path>) -> Result<()> {
        let project_path = project_path.as_ref().to_path_buf();
        let ident = strategy::read_module_ident_from_manifest_dir(&project_path)?;
        self.add_module_with(
            ident,
            ModuleResolverStrategy::ManifestDirectory { path: project_path },
        )?;
        Ok(())
    }

    /// Look up workspace-staged packages by canonical id and route each
    /// through [`Self::add_module_with`] with
    /// [`ModuleResolverStrategy::WorkspaceStaged`].
    ///
    /// `cargo xtask build-plugins` must have run first — the strategy's
    /// resolver errors with [`LoadWorkspacePackagesError::PackageNotStaged`]
    /// when a name's staged dir is missing.
    ///
    /// Workspace root resolution: `STREAMLIB_WORKSPACE_ROOT` env var
    /// when set (and the path exists), otherwise
    /// `cargo locate-project --workspace`.
    pub fn load_workspace_packages<I, S>(
        &self,
        names: I,
    ) -> std::result::Result<(), LoadWorkspacePackagesError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        // Eagerly validate every id BEFORE touching the workspace root —
        // a typo'd id should surface as `InvalidPackageId` immediately
        // rather than masquerading as `WorkspaceRootNotFound` when the
        // env var is unset.
        let parsed: Vec<(String, streamlib_idents::PackageRef)> = names
            .into_iter()
            .map(|name| {
                let name_str = name.as_ref().to_string();
                let parsed = workspace::parse_canonical_package_id(&name_str)?;
                let pkg_ref = streamlib_idents::PackageRef::new(
                    streamlib_idents::Org::new(parsed.org_str).map_err(|_| {
                        LoadWorkspacePackagesError::InvalidPackageId(name_str.clone())
                    })?,
                    streamlib_idents::Package::new(parsed.name_str).map_err(|_| {
                        LoadWorkspacePackagesError::InvalidPackageId(name_str.clone())
                    })?,
                );
                Ok::<_, LoadWorkspacePackagesError>((name_str, pkg_ref))
            })
            .collect::<std::result::Result<_, _>>()?;

        // Surface workspace-root failure once, up front, so the error
        // mode matches the historical helper's contract (single
        // `WorkspaceRootNotFound` rather than one-per-id).
        let _ = workspace::resolve_workspace_root()?;

        for (name_str, pkg_ref) in parsed {
            // Keep the typed segments around for the back-compat error
            // translation below — moving them into `ModuleIdent::any`
            // would consume the values one statement before we need
            // them for the `PackageIdentityMismatch` arm.
            let req_org = pkg_ref.org.as_str().to_string();
            let req_name = pkg_ref.name.as_str().to_string();
            let ident = streamlib_idents::ModuleIdent::any(pkg_ref.org, pkg_ref.name);
            self.add_module_with(ident, ModuleResolverStrategy::WorkspaceStaged)
                .map_err(|err| match err {
                    AddModuleError::WorkspaceStageMiss { expected_path, .. } => {
                        LoadWorkspacePackagesError::PackageNotStaged {
                            name: name_str.clone(),
                            expected_path,
                        }
                    }
                    AddModuleError::ManifestIdentityMismatch {
                        source_path, actual, ..
                    } => {
                        let (actual_org, actual_name) =
                            strategy::split_canonical_for_legacy_error(&actual);
                        LoadWorkspacePackagesError::PackageIdentityMismatch {
                            staged_path: source_path,
                            requested_org: req_org.clone(),
                            requested_name: req_name.clone(),
                            actual_org,
                            actual_name,
                        }
                    }
                    AddModuleError::CdylibMissingForRustImpl { expected_path, .. } => {
                        LoadWorkspacePackagesError::CdylibMissing {
                            name: name_str.clone(),
                            expected_path,
                        }
                    }
                    AddModuleError::LoadProjectFailed { source, .. } => {
                        LoadWorkspacePackagesError::LoadProjectFailed {
                            name: name_str.clone(),
                            source,
                        }
                    }
                    AddModuleError::WorkspaceRootInvalid { .. } => {
                        LoadWorkspacePackagesError::WorkspaceRootNotFound
                    }
                    other => LoadWorkspacePackagesError::LoadProjectFailed {
                        name: name_str.clone(),
                        source: Box::new(other.into()),
                    },
                })?;
        }

        Ok(())
    }

    /// Load a `streamlib.yaml`-packaged module by typed
    /// [`streamlib_idents::ModuleIdent`]. Routes through the default
    /// resolver chain (workspace stage → installed-package cache).
    ///
    /// Imperative complement to the yaml-driven path: both this and
    /// [`Self::load_project`] drive into the same internal
    /// module-loading machinery; the yaml form is for declarative
    /// deployment manifests, the imperative form is for REST endpoints,
    /// hot-reload tools, test setup, and composition-library wrapping.
    ///
    /// Transitive dependencies declared in the loaded module's
    /// `streamlib.yaml` are recursively routed through the same flow,
    /// each picking the dep's appropriate strategy (declared `path:` →
    /// [`ModuleResolverStrategy::ManifestDirectory`]; consumer `patch:`
    /// override of any kind likewise; bare registry/git declarations →
    /// [`ModuleResolverStrategy::DefaultChain`]). Cycles are detected
    /// per-call and surfaced as
    /// [`AddModuleError::DependencyCycleDetected`].
    ///
    /// Calls are idempotent at the registry layer: re-loading a module
    /// whose processors / schemas are already registered surfaces no
    /// error and re-runs the dylib's plugin callback (which the engine
    /// already tolerates per `register_dynamic`'s dedup semantics).
    pub fn add_module(
        &self,
        module: streamlib_idents::ModuleIdent,
    ) -> std::result::Result<(), AddModuleError> {
        self.add_module_with(module, ModuleResolverStrategy::DefaultChain)
    }

    /// Load a module via an explicit [`ModuleResolverStrategy`].
    ///
    /// In-code equivalent of the manifest's `patch:` table — callers
    /// can pin a dep to a workspace path, a `.slpkg` archive, or a
    /// specific resolver tier without editing yaml. See
    /// [`ModuleResolverStrategy`] for the variant catalog.
    #[tracing::instrument(name = "runtime.add_module_with", skip(self), fields(module = %module, strategy = ?strategy))]
    pub fn add_module_with(
        &self,
        module: streamlib_idents::ModuleIdent,
        strategy: ModuleResolverStrategy,
    ) -> std::result::Result<(), AddModuleError> {
        // Two collections threaded through the recursion:
        // - `seen` is O(1) cycle membership lookup
        // - `path` preserves insertion order so cycle errors carry the
        //   full recursion edge (`A → B → A` rather than just `A`)
        let mut seen: HashSet<streamlib_idents::PackageRef> = HashSet::new();
        let mut path: Vec<streamlib_idents::PackageRef> = Vec::new();
        recursive_walker::add_module_recursively(
            &self.iceoryx2_node,
            module,
            strategy,
            &mut seen,
            &mut path,
        )
    }

    /// Unload a previously-added module.
    ///
    /// **Not yet implemented.** Module-level unload requires the
    /// hot-reload lifecycle work that's explicitly out of scope for
    /// the current All-Dynamic Package Loading milestone — see the
    /// milestone's "Explicitly out of scope (deferred to later
    /// milestones)" section ("`unload_package` / hot-reload
    /// lifecycle. Load-only, runtime-lifetime registration this
    /// milestone."). The method exists as an explicit boundary
    /// marker (rather than being absent) so AI agents and other
    /// callers reaching for it from the `add_module` counterpart get
    /// a typed error pointing at the milestone gap instead of a
    /// `method not found` diagnostic.
    ///
    /// Calling this returns
    /// [`RemoveModuleError::HotReloadLifecycleNotYetImplemented`]
    /// without altering any runtime state.
    pub fn remove_module(
        &self,
        module: streamlib_idents::ModuleIdent,
    ) -> std::result::Result<(), RemoveModuleError> {
        Err(RemoveModuleError::HotReloadLifecycleNotYetImplemented { module })
    }
}
