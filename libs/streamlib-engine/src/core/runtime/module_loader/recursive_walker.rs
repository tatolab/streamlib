// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashSet;

use super::errors::AddModuleError;
use super::processor_registration::register_manifest_processors;
use super::schema_registration::register_package_schemas;
use super::strategy::{resolve_strategy_to_manifest_dir, ModuleResolverStrategy};
use crate::core::{Error, Result};
use crate::iceoryx2::Iceoryx2Node;

/// Recursive worker: resolves the strategy, validates the manifest's
/// identity + version range, registers the package's schemas, walks
/// dependencies (each routed through this same helper), then registers
/// the package's processors.
///
/// `seen` tracks every [`PackageRef`] currently on the recursion stack
/// (O(1) membership check); `path` preserves insertion order so the
/// dependency-cycle error carries the actual edge that re-entered.
/// The current package's ref is inserted on entry and removed on exit,
/// so sibling sub-trees can revisit a shared transitive dep without
/// false positives.
///
/// Order matters: deps load before this package's own processors so
/// schemas referenced from `with_config_schema(...)` resolve.
///
/// [`PackageRef`]: streamlib_idents::PackageRef
pub(super) fn add_module_recursively(
    iceoryx2_node: &Iceoryx2Node,
    module: streamlib_idents::ModuleIdent,
    strategy: ModuleResolverStrategy,
    seen: &mut HashSet<streamlib_idents::PackageRef>,
    path: &mut Vec<streamlib_idents::PackageRef>,
) -> std::result::Result<(), AddModuleError> {
    let pkg_ref = module.package_ref();
    if !seen.insert(pkg_ref.clone()) {
        // Reaching this package while it is already mid-load on the
        // recursion stack is the dependency-cycle signal. Surface
        // the full recursion path plus the repeated vertex so the
        // caller can see exactly which edge re-entered.
        let mut cycle = path.clone();
        cycle.push(pkg_ref);
        return Err(AddModuleError::DependencyCycleDetected { cycle });
    }
    path.push(pkg_ref.clone());
    // Run the body, then remove `pkg_ref` from both `seen` and
    // `path` regardless of the body's exit path — sibling sub-trees
    // can revisit a shared transitive dep without false-positive
    // cycle reports.
    let result = add_module_recursive_body(iceoryx2_node, module, strategy, seen, path);
    seen.remove(&pkg_ref);
    path.pop();
    result
}

/// Body of [`add_module_recursively`] — split out so the caller can
/// pop `pkg_ref` from `seen` + `path` after every exit path.
fn add_module_recursive_body(
    iceoryx2_node: &Iceoryx2Node,
    module: streamlib_idents::ModuleIdent,
    strategy: ModuleResolverStrategy,
    seen: &mut HashSet<streamlib_idents::PackageRef>,
    path: &mut Vec<streamlib_idents::PackageRef>,
) -> std::result::Result<(), AddModuleError> {
    use crate::core::config::ProjectConfig;

    let pkg_ref = module.package_ref();
    let (manifest_dir, on_disk_version) =
        resolve_strategy_to_manifest_dir(&strategy, Some(&pkg_ref))?;

    // Read the manifest; this is the authoritative source of
    // identity for the package at the resolved location.
    let config = ProjectConfig::load(&manifest_dir)
        .map_err(|e| AddModuleError::ManifestLoadFailed {
            module: module.clone(),
            source_path: manifest_dir.clone(),
            detail: e.to_string(),
        })?;

    config
        .check_streamlib_version_compatibility()
        .map_err(|e| AddModuleError::ManifestLoadFailed {
            module: module.clone(),
            source_path: manifest_dir.clone(),
            detail: e.to_string(),
        })?;

    // Identity check: the manifest's `[package]` org/name must match
    // the requested ident. Catches `.slpkg`s shipped with the wrong
    // content as well as workspace-stage clobbers.
    let pkg_meta = config
        .package
        .as_ref()
        .ok_or_else(|| AddModuleError::ManifestLoadFailed {
            module: module.clone(),
            source_path: manifest_dir.clone(),
            detail: "manifest has no `package:` block".into(),
        })?;
    if pkg_meta.org != module.org || pkg_meta.name != module.name {
        return Err(AddModuleError::ManifestIdentityMismatch {
            module: module.clone(),
            source_path: manifest_dir.clone(),
            actual: format!(
                "@{}/{}",
                pkg_meta.org.as_str(),
                pkg_meta.name.as_str()
            ),
        });
    }
    if !module.version.matches(on_disk_version) {
        return Err(AddModuleError::VersionRangeUnsatisfied {
            module: module.clone(),
            found: on_disk_version,
            source_path: manifest_dir.clone(),
        });
    }

    tracing::info!(
        "add_module: '{}' → {} (on-disk version {})",
        module,
        manifest_dir.display(),
        on_disk_version,
    );

    // Register every schema declared in this package's `streamlib.yaml`'s
    // `schemas:` list BEFORE recursing — schemas are leaves in the dep
    // graph and don't depend on transitive package state. (Processor
    // registration runs AFTER deps so bare-name config schemas in dep
    // packages have already populated the registry.)
    register_package_schemas(&manifest_dir, &config).map_err(|e| {
        AddModuleError::LoadProjectFailed {
            module: module.clone(),
            source: Box::new(e),
        }
    })?;

    // Walk transitive deps. Each dep is itself routed through
    // `add_module_recursively`, so the per-dep strategy lookup is
    // the same shape as the consumer's own top-level call.
    for (dep_ref, spec) in &config.dependencies {
        let (dep_ident, dep_strategy) =
            derive_dep_strategy_and_ident(&manifest_dir, dep_ref, spec, &config.patch)
                .map_err(|e| AddModuleError::LoadProjectFailed {
                    module: module.clone(),
                    source: Box::new(e),
                })?;
        tracing::info!(
            "Loading dependency '{}' (strategy {:?})",
            dep_ident,
            dep_strategy
        );
        add_module_recursively(iceoryx2_node, dep_ident, dep_strategy, seen, path)?;
    }

    // Now register this package's own processors.
    register_manifest_processors(iceoryx2_node, &manifest_dir, &config).map_err(|e| {
        AddModuleError::LoadProjectFailed {
            module: module.clone(),
            source: Box::new(e),
        }
    })?;

    Ok(())
}

/// Map a single declared dep (with optional consumer `patch:`
/// override) to the [`ModuleIdent`] + [`ModuleResolverStrategy`]
/// pair that recursively re-enters [`add_module_recursively`].
///
/// Patch precedence mirrors Cargo's `[patch.crates-io]`: consumer's
/// patch table wins when present; otherwise the dep declaration's
/// source variant decides.
///
/// [`ModuleIdent`]: streamlib_idents::ModuleIdent
fn derive_dep_strategy_and_ident(
    consumer_dir: &std::path::Path,
    dep_ref: &streamlib_idents::PackageRef,
    spec: &streamlib_idents::DependencySpec,
    patch: &std::collections::BTreeMap<streamlib_idents::PackageRef, streamlib_idents::DependencySpec>,
) -> Result<(streamlib_idents::ModuleIdent, ModuleResolverStrategy)> {
    use streamlib_idents::{DependencySpec, ModuleIdent, SemVerRange};

    // The declared range (registry deps) constrains version resolution
    // even when the source is patched. Path / git deps don't carry a
    // range at declaration time — the patched location's manifest
    // version becomes authoritative (range = any).
    let declared_range = match spec {
        DependencySpec::Registry(r) => r.version.clone(),
        DependencySpec::Path(_) | DependencySpec::Git(_) => SemVerRange::Any,
    };

    let (strategy_spec, source_label) = match patch.get(dep_ref) {
        Some(patched) => (patched, "patch"),
        None => (spec, "dep"),
    };

    let strategy = match strategy_spec {
        DependencySpec::Path(p) => {
            let abs = if p.path.is_absolute() {
                p.path.clone()
            } else {
                consumer_dir.join(&p.path)
            };
            // npm/wrangler-style strict validation: a missing path is
            // a hard error so the dev knows immediately to fix the
            // manifest.
            if source_label == "patch" && !abs.exists() {
                return Err(Error::Configuration(format!(
                    "patch entry for '{dep_ref}' in {}/{} points at `{}` \
                     which does not exist. Path patches are dev-time \
                     overrides — they must resolve to a real directory \
                     at parse time.",
                    consumer_dir.display(),
                    crate::core::config::ProjectConfig::FILE_NAME,
                    abs.display(),
                )));
            }
            ModuleResolverStrategy::ManifestDirectory { path: abs }
        }
        DependencySpec::Git(g) => {
            let cache_dir =
                crate::core::streamlib_home::get_streamlib_home().join("resolver-cache");
            let checkout = streamlib_idents::fetch_git(
                &dep_ref.to_string(),
                &g.git,
                &g.rev,
                &cache_dir,
            )
            .map_err(|e| Error::Configuration(e.to_string()))?;
            ModuleResolverStrategy::ManifestDirectory { path: checkout }
        }
        DependencySpec::Registry(_) => {
            if source_label == "patch" {
                return Err(Error::Configuration(format!(
                    "patch entry for '{dep_ref}' in {}/{} is registry-flavored. \
                     The v1 resolver doesn't ship a registry — declare a \
                     `path:` or `git:` patch entry, or remove the patch and \
                     rely on the installed-package cache.",
                    consumer_dir.display(),
                    crate::core::config::ProjectConfig::FILE_NAME,
                )));
            }
            ModuleResolverStrategy::DefaultChain
        }
    };

    let ident = ModuleIdent::new(dep_ref.org.clone(), dep_ref.name.clone(), declared_range);
    Ok((ident, strategy))
}
