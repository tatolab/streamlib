// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashSet;
use std::sync::Arc;

use super::build_orchestrator::{BuildEventSink, BuildOrchestrator, BuildPolicy};
use super::errors::AddModuleError;
use super::processor_registration::register_manifest_processors;
use super::schema_registration::register_package_schemas;
use super::source::{read_version_from_manifest_dir, resolve_strategy_to_source, ResolvedSource, Strategy};
use crate::core::{Error, Result};
use crate::iceoryx2::Iceoryx2Node;

/// Recursive worker: resolves the [`Strategy`] to a source, materializes
/// via the injected [`BuildOrchestrator`] when a build is required,
/// validates the manifest's identity + version range, registers the
/// package's schemas, walks dependencies (each routed through this same
/// helper), then registers the package's processors.
///
/// `seen` tracks every [`PackageRef`] currently on the recursion stack
/// (O(1) membership); `path` preserves insertion order so the
/// dependency-cycle error carries the actual edge that re-entered.
///
/// [`PackageRef`]: streamlib_idents::PackageRef
#[allow(clippy::too_many_arguments)]
pub(super) fn add_module_recursively(
    iceoryx2_node: &Iceoryx2Node,
    orchestrator: Option<&Arc<dyn BuildOrchestrator>>,
    sink: &dyn BuildEventSink,
    module: streamlib_idents::ModuleIdent,
    strategy: Strategy,
    seen: &mut HashSet<streamlib_idents::PackageRef>,
    path: &mut Vec<streamlib_idents::PackageRef>,
) -> std::result::Result<(), AddModuleError> {
    let pkg_ref = module.package_ref();
    if !seen.insert(pkg_ref.clone()) {
        let mut cycle = path.clone();
        cycle.push(pkg_ref);
        return Err(AddModuleError::DependencyCycleDetected { cycle });
    }
    path.push(pkg_ref.clone());
    let result =
        add_module_recursive_body(iceoryx2_node, orchestrator, sink, module, strategy, seen, path);
    seen.remove(&pkg_ref);
    path.pop();
    result
}

#[allow(clippy::too_many_arguments)]
fn add_module_recursive_body(
    iceoryx2_node: &Iceoryx2Node,
    orchestrator: Option<&Arc<dyn BuildOrchestrator>>,
    sink: &dyn BuildEventSink,
    module: streamlib_idents::ModuleIdent,
    strategy: Strategy,
    seen: &mut HashSet<streamlib_idents::PackageRef>,
    path: &mut Vec<streamlib_idents::PackageRef>,
) -> std::result::Result<(), AddModuleError> {
    use crate::core::config::ProjectConfig;

    let pkg_ref = module.package_ref();

    // Resolve where the source lives (pure filesystem / cache / git),
    // then materialize via the orchestrator if a build is required.
    let manifest_dir = match resolve_strategy_to_source(&strategy, &pkg_ref)? {
        ResolvedSource::Ready(dir) => dir,
        ResolvedSource::NeedsBuild(request) => match orchestrator {
            // An orchestrator is wired — materialize (fetch/build/stage).
            Some(orch) => {
                let staged = orch.materialize(&request, sink).map_err(|e| {
                    AddModuleError::MaterializeFailed {
                        package: pkg_ref.clone(),
                        detail: e.to_string(),
                    }
                })?;
                staged.staged_dir
            }
            // No orchestrator wired. Any build-requiring policy
            // (`IfStale` or `AlwaysBuild`) fails loud — never silently
            // load a possibly-stale or unbuilt artifact, and never branch
            // behavior on package shape. A no-build deployment uses
            // `NeverBuild` / `InstalledCache` / `.slpkg`; a building one
            // wires an orchestrator (the SDK `auto-build` feature does so
            // by default).
            None => {
                return Err(AddModuleError::BuildRequiredButNoOrchestrator {
                    package: pkg_ref.clone(),
                    policy: request.policy,
                })
            }
        },
    };

    let on_disk_version = read_version_from_manifest_dir(&manifest_dir)?;

    // Read the manifest; authoritative source of identity for the
    // package at the resolved location.
    let config = ProjectConfig::load(&manifest_dir).map_err(|e| {
        AddModuleError::ManifestLoadFailed {
            module: module.clone(),
            source_path: manifest_dir.clone(),
            detail: e.to_string(),
        }
    })?;

    config
        .check_streamlib_version_compatibility()
        .map_err(|e| AddModuleError::ManifestLoadFailed {
            module: module.clone(),
            source_path: manifest_dir.clone(),
            detail: e.to_string(),
        })?;

    // Identity check: the manifest's `[package]` org/name must match
    // the requested ident.
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
            actual: format!("@{}/{}", pkg_meta.org.as_str(), pkg_meta.name.as_str()),
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

    // Schemas are leaves — register before recursing into deps.
    register_package_schemas(&manifest_dir, &config).map_err(|e| {
        AddModuleError::LoadProjectFailed {
            module: module.clone(),
            source: Box::new(e),
        }
    })?;

    // Walk transitive deps, each routed through this same helper.
    for (dep_ref, spec) in &config.dependencies {
        let (dep_ident, dep_strategy) =
            derive_dep_strategy_and_ident(&manifest_dir, dep_ref, spec, &config.patch).map_err(
                |e| AddModuleError::LoadProjectFailed {
                    module: module.clone(),
                    source: Box::new(e),
                },
            )?;
        tracing::info!(
            "Loading dependency '{}' (strategy {:?})",
            dep_ident,
            dep_strategy
        );
        add_module_recursively(
            iceoryx2_node,
            orchestrator,
            sink,
            dep_ident,
            dep_strategy,
            seen,
            path,
        )?;
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

/// Map a single declared dep (with optional consumer `patch:` override)
/// to the [`ModuleIdent`] + [`Strategy`] pair that recursively re-enters
/// [`add_module_recursively`].
///
/// Patch precedence mirrors Cargo's `[patch.crates-io]`: consumer's
/// patch wins when present; otherwise the dep declaration's source
/// variant decides. Path / git source deps are dev-shaped and default to
/// [`BuildPolicy::IfStale`] (rebuild-on-change via the build tool);
/// registry deps resolve from the installed cache.
///
/// [`ModuleIdent`]: streamlib_idents::ModuleIdent
fn derive_dep_strategy_and_ident(
    consumer_dir: &std::path::Path,
    dep_ref: &streamlib_idents::PackageRef,
    spec: &streamlib_idents::DependencySpec,
    patch: &std::collections::BTreeMap<streamlib_idents::PackageRef, streamlib_idents::DependencySpec>,
) -> Result<(streamlib_idents::ModuleIdent, Strategy)> {
    use streamlib_idents::{DependencySpec, ModuleIdent, SemVerRange};

    // Registry deps carry a range that constrains resolution even when
    // patched. Path / git deps don't — the source's manifest version
    // becomes authoritative (range = any).
    let declared_range = match spec {
        DependencySpec::Registry(r) => r.version.clone(),
        DependencySpec::Path(_) | DependencySpec::Git(_) => SemVerRange::Any,
    };

    let (strategy_spec, is_patch) = match patch.get(dep_ref) {
        Some(patched) => (patched, true),
        None => (spec, false),
    };

    let strategy = match strategy_spec {
        DependencySpec::Path(p) => {
            // Path deps are dev-time sources resolved relative to the
            // CWD (the consumer's run dir); a missing patch path is a
            // hard error so the dev fixes the manifest immediately.
            let abs = if p.path.is_absolute() {
                p.path.clone()
            } else {
                consumer_dir.join(&p.path)
            };
            if is_patch && !abs.exists() {
                return Err(Error::Configuration(format!(
                    "patch entry for '{dep_ref}' points at `{}` which does not \
                     exist. Path patches are dev-time overrides — they must \
                     resolve to a real directory.",
                    abs.display(),
                )));
            }
            Strategy::Path {
                path: abs,
                build: BuildPolicy::IfStale,
            }
        }
        DependencySpec::Git(g) => Strategy::Git {
            url: g.git.clone(),
            rev: g.rev.clone(),
            build: BuildPolicy::IfStale,
        },
        DependencySpec::Registry(_) => {
            if is_patch {
                return Err(Error::Configuration(format!(
                    "patch entry for '{dep_ref}' is registry-flavored. The v1 \
                     resolver doesn't ship a registry — declare a `path:` or \
                     `git:` patch, or rely on the installed-package cache.",
                )));
            }
            Strategy::InstalledCache
        }
    };

    let ident = ModuleIdent::new(dep_ref.org.clone(), dep_ref.name.clone(), declared_range);
    Ok((ident, strategy))
}
