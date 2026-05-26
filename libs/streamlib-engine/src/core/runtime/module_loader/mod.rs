// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Module-loading subsystem: typed [`ModuleResolverStrategy`] enum
//! covering every loader behavior as a named variant, plus the
//! [`Runner::add_module`] / [`Runner::add_module_with`] public API
//! surface.
//!
//! Files in this directory:
//!
//! - [`errors`] — `AddModuleError`, `RemoveModuleError` typed enums.
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
//! - [`workspace`] — workspace-root resolution.
//!
//! [`Runner::add_module`]: super::Runner::add_module
//! [`Runner::add_module_with`]: super::Runner::add_module_with

use std::collections::HashSet;

use super::Runner;

mod errors;
mod processor_registration;
mod recursive_walker;
mod schema_registration;
mod slpkg;
mod strategy;
mod workspace;

#[cfg(test)]
mod tests;

pub use errors::{AddModuleError, RemoveModuleError};
pub use processor_registration::host_target_triple;
pub use slpkg::extract_slpkg_to_cache;
pub use strategy::ModuleResolverStrategy;

impl Runner {
    // =========================================================================
    // Module Loading — public API surface
    // =========================================================================
    //
    // Every module-loading entry point on `Runner` routes through
    // `add_module_with(ident, strategy)`. The default
    // [`Runner::add_module`] form is the bare-ident path that
    // dispatches through [`ModuleResolverStrategy::DefaultChain`]
    // (workspace stage → installed-package cache); the
    // [`Runner::add_module_with`] form lets callers pin a strategy
    // explicitly, which is the in-code equivalent of the manifest's
    // `patch:` table.

    /// Load a `streamlib.yaml`-packaged module by typed
    /// [`streamlib_idents::ModuleIdent`]. Routes through the default
    /// resolver chain (workspace stage → installed-package cache).
    ///
    /// Imperative complement to the yaml-driven path: both this and
    /// the manifest's `dependencies:` table drive into the same
    /// internal module-loading machinery; the yaml form is for
    /// declarative deployment manifests, the imperative form is for
    /// REST endpoints, hot-reload tools, test setup, and
    /// composition-library wrapping.
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
