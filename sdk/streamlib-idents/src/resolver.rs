// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib.yaml` dependency resolver.
//!
//! Given a root manifest, walks declared dependencies via path / git / .slpkg
//! sources, validates package identifiers + semver ranges, and returns a
//! [`ResolvedPackages`] set along with content hashes that drive
//! `streamlib-codegen.lock`.
//!
//! The resolver is the keystone of the milestone-10 architecture: it converts
//! the structured `Manifest` into the input shape codegen consumes (a flat
//! set of `(SchemaIdent, JtdSchema)` pairs grouped by package).

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{ResolverError, ResolverResult};
use crate::git::fetch_git;
use crate::ident::{PackageRef, TypeName};
use crate::lockfile::{Lockfile, LockfileEntry, LockfileSource, compute_content_hash};
use crate::manifest::{DependencySpec, Manifest, RegistryDependency, SchemaEntry};
use crate::registry::{RegistryClient, RegistryConfig, cache_slpkg_bytes, select_version};

/// Outcome of resolving a `streamlib.yaml` graph: the root project + every
/// transitive package, keyed by canonical `"@org/name"` lockfile key.
///
/// `root` is the entry point manifest the resolver was invoked on; it's
/// accessible separately from `packages` because:
///
/// 1. A project-flavor root has no `package_id()` and therefore no place in
///    the lockfile.
/// 2. Even when the root *is* a package, the lockfile records *its
///    dependencies*, not the package itself (mirrors `Cargo.lock`).
#[derive(Debug, Clone)]
pub struct ResolvedPackages {
    pub root: ResolvedPackage,
    pub packages: BTreeMap<String, ResolvedPackage>,
}

impl ResolvedPackages {
    /// Iterate root + every dependency.
    pub fn iter_all(&self) -> impl Iterator<Item = &ResolvedPackage> {
        std::iter::once(&self.root).chain(self.packages.values())
    }

    /// Build a `Lockfile` from the dependency set (root excluded — the lock
    /// records dependencies, not the consumer).
    pub fn to_lockfile(&self) -> Lockfile {
        let mut packages = BTreeMap::new();
        for (key, pkg) in &self.packages {
            let entry = LockfileEntry {
                version: pkg
                    .manifest
                    .package
                    .as_ref()
                    .map(|p| p.version)
                    .expect("dependencies must be package-flavor manifests"),
                source: pkg.source.to_lockfile_source(),
                content_hash: pkg.content_hash.clone(),
            };
            packages.insert(key.clone(), entry);
        }
        Lockfile {
            version: 1,
            packages,
        }
    }
}

/// A resolved package: the loaded manifest, its root directory on disk, the
/// schema files it owns, the source it was resolved from, and a content hash
/// that locks the pair.
#[derive(Debug, Clone)]
pub struct ResolvedPackage {
    pub manifest: Manifest,
    pub root_dir: PathBuf,
    pub schema_files: Vec<PathBuf>,
    pub source: ResolvedSource,
    pub content_hash: String,
}

/// Resolved provenance: where this package came from after walking the
/// dependency graph.
#[derive(Debug, Clone)]
pub enum ResolvedSource {
    /// The root manifest the resolver was invoked on. Not in the lockfile.
    Root,
    /// Resolved from a path dependency (manifest path stored relative to the
    /// consumer's manifest dir, mirroring how the user wrote the dep).
    Path { relative: PathBuf },
    /// Resolved from a git pinned commit.
    Git { url: String, rev: String },
    /// Resolved from a `.slpkg` archive (path to the archive is stored).
    Slpkg { archive: PathBuf },
    /// Resolved from a registry. Reserved — v1 does not implement a
    /// registry, so this variant is constructable only by future code paths.
    Registry { url: String },
}

impl ResolvedSource {
    fn to_lockfile_source(&self) -> LockfileSource {
        match self {
            Self::Root => unreachable!("root source never lands in the lockfile"),
            Self::Path { relative } => LockfileSource::Path {
                path: relative.clone(),
            },
            Self::Git { url, rev } => LockfileSource::Git {
                url: url.clone(),
                rev: rev.clone(),
            },
            Self::Slpkg { archive } => LockfileSource::Path {
                path: archive.clone(),
            },
            Self::Registry { url } => LockfileSource::Registry { url: url.clone() },
        }
    }
}

/// Configuration for [`resolve`]. Defaults to `~/.streamlib/resolver-cache/`
/// for git + `.slpkg` extraction storage.
#[derive(Debug, Clone, Default)]
pub struct ResolverOptions {
    /// Override the cache directory for git clones and `.slpkg` extractions.
    /// `None` falls back to `$HOME/.streamlib/resolver-cache/`.
    pub cache_dir: Option<PathBuf>,
    /// Static-registry configuration for resolving `Registry` schema
    /// dependencies. `None` means no registry is configured — a `Registry`
    /// dependency then surfaces [`ResolverError::RegistryNotConfigured`].
    /// [`resolve_with`] reads this field only; it never consults the process
    /// environment. Codegen entry points (build scripts, `streamlib
    /// generate`) populate it via [`ResolverOptions::from_env`].
    pub registry: Option<RegistryConfig>,
    /// Active `streamlib link` checkout, when a link is in effect. `Some`
    /// redirects any dependency present in `<checkout>/packages/<name>` to that
    /// checkout source — the schema-dep mirror of the module loader's link
    /// redirect, completing the zero-registry dev loop. `None` (the dominant
    /// case) leaves resolution byte-identical to a non-linked build: absent
    /// packages, and every dep when no link is active, resolve by version from
    /// the store / registry (or via a dev path patch). Populated at the
    /// compile-time codegen boundary by [`ResolverOptions::from_env_or_marker`]
    /// — **marker-first** (the `.streamlib/link.json` a `streamlib link` wrote,
    /// discovered by walking up from the crate dir) with
    /// [`crate::LINK_CHECKOUT_ENV`] as an explicit override. A locked run or an
    /// orchestrator-driven distribution build stays `None`: locked runs never
    /// build (so no codegen boundary runs), and the orchestrator sets the env
    /// to empty to suppress marker discovery for a relocated build, preserving
    /// reproducibility. [`resolve_with`] reads this field only; it never
    /// consults the marker or the environment.
    pub link_checkout: Option<PathBuf>,
}

impl ResolverOptions {
    /// Options with the registry config read from the environment
    /// (`STREAMLIB_REGISTRY_URL`, defaulting to the first-party registry) and
    /// the default cache dir. This is the codegen-boundary constructor — build
    /// scripts and `streamlib generate` use it so a registry-cached crate
    /// resolves its schema deps from the configured registry. Unit tests
    /// construct [`ResolverOptions`] directly to stay hermetic.
    pub fn from_env() -> Self {
        Self {
            cache_dir: None,
            registry: RegistryConfig::from_env(),
            link_checkout: link_checkout_from_env(),
        }
    }

    /// The compile-time codegen-boundary constructor: like [`from_env`], but
    /// resolves the active `streamlib link` checkout **marker-first** — walking
    /// up from `manifest_dir` for the `.streamlib/link.json` a `streamlib link`
    /// wrote — with [`crate::LINK_CHECKOUT_ENV`] as an explicit override. Build
    /// scripts, the `#[processor]` macro, and `streamlib generate` use this so a
    /// directly-`cargo build`-ed linked app resolves its schema deps from the
    /// checkout with no env var to export by hand.
    ///
    /// Env precedence over the marker is what preserves the load-bearing
    /// suppression boundary: the build orchestrator sets the env for every
    /// relocated build (the checkout path when a link is active, empty to
    /// suppress otherwise), so a distribution / locked-materialization build
    /// never re-derives link state from a stray marker up-tree of the
    /// relocated crate dir. Unit tests construct [`ResolverOptions`] directly to
    /// stay hermetic.
    ///
    /// [`from_env`]: ResolverOptions::from_env
    pub fn from_env_or_marker(manifest_dir: &Path) -> Self {
        Self {
            cache_dir: None,
            registry: RegistryConfig::from_env(),
            link_checkout: link_checkout_env_or_marker(manifest_dir),
        }
    }
}

/// Read the active `streamlib link` checkout from [`crate::LINK_CHECKOUT_ENV`],
/// returning `None` when unset/empty — the codegen-boundary read that makes
/// `resolve_with` link-aware without the pure resolver ever touching the
/// environment. The orchestrator sets this env on a linked package's `cargo
/// build` (and sets it to empty for locked/distribution builds), so a
/// `build.rs` schema-dep codegen redirects to the checkout only when a link is
/// active. Used by [`ResolverOptions::from_env`]; the marker-first boundary
/// ([`ResolverOptions::from_env_or_marker`]) layers `.streamlib/link.json`
/// discovery under an absent env.
fn link_checkout_from_env() -> Option<PathBuf> {
    std::env::var_os(crate::LINK_CHECKOUT_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Resolve the active `streamlib link` checkout for a compile-time codegen
/// boundary — **marker-first with env override**, `var_os` distinguishing
/// unset from set-empty:
///
/// * [`crate::LINK_CHECKOUT_ENV`] **set to a non-empty path** ⇒ that path (an
///   explicit override — a dev exporting it, or the build orchestrator
///   threading the discovered checkout for a relocated build).
/// * [`crate::LINK_CHECKOUT_ENV`] **set to empty** ⇒ `None` (suppression) with
///   NO marker discovery. The orchestrator sets this for a locked / distribution
///   build so a relocated `cargo build` never re-derives link state from a stray
///   `.streamlib/link.json` up-tree of the relocated crate dir — the
///   streamlib-home cache the orchestrator relocates into shares the
///   `.streamlib` dir name with the link-state dir, so an unconditional walk-up
///   would otherwise collide.
/// * [`crate::LINK_CHECKOUT_ENV`] **unset** ⇒ walk up from `manifest_dir` for
///   `.streamlib/link.json` and take its checkout — the direct-`cargo build`
///   path with no env exported.
///
/// The env read still keeps the pure resolver env-free: this runs at the
/// codegen boundary only ([`ResolverOptions::from_env_or_marker`]).
fn link_checkout_env_or_marker(manifest_dir: &Path) -> Option<PathBuf> {
    match std::env::var_os(crate::LINK_CHECKOUT_ENV) {
        Some(raw) => {
            let path = PathBuf::from(raw);
            if path.as_os_str().is_empty() {
                None
            } else {
                Some(path)
            }
        }
        None => link_checkout_from_marker(manifest_dir),
    }
}

/// Walk up from `manifest_dir` for a `streamlib link` marker
/// (`.streamlib/link.json`) and return its recorded checkout. `None` when no
/// marker is found; a corrupt marker is logged and treated as no link (never a
/// silent redirect to a suspect checkout — the resolve then fails loud if the
/// dep is otherwise unresolvable).
fn link_checkout_from_marker(manifest_dir: &Path) -> Option<PathBuf> {
    match crate::link_marker::find_and_load_active_link(manifest_dir) {
        Ok(Some((marker, manifest))) => {
            tracing::debug!(
                marker = %marker.display(),
                checkout = %manifest.checkout.display(),
                "streamlib link marker discovered at codegen boundary — resolving \
                 schema deps from the linked checkout"
            );
            Some(manifest.checkout)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(
                start = %manifest_dir.display(),
                error = %e,
                "streamlib link marker present but unreadable — ignoring for schema \
                 resolution (run `streamlib unlink` then re-link to repair)"
            );
            None
        }
    }
}

/// Resolve a `streamlib.yaml` at `root_dir` and every transitive dependency
/// it declares.
pub fn resolve(root_dir: &Path) -> ResolverResult<ResolvedPackages> {
    resolve_with(root_dir, &ResolverOptions::default())
}

/// Resolve with explicit options.
pub fn resolve_with(
    root_dir: &Path,
    options: &ResolverOptions,
) -> ResolverResult<ResolvedPackages> {
    let root_manifest_path = root_dir.join(Manifest::FILE_NAME);
    let root_manifest = Manifest::load_file(&root_manifest_path)?;

    let cache_dir = match &options.cache_dir {
        Some(p) => p.clone(),
        None => default_cache_dir()?,
    };

    // `resolve_with` is pure: it reads the registry config from `options`
    // only, never from the process environment. The env read lives at the
    // codegen boundary (`ResolverOptions::from_env`, used by build scripts
    // and `streamlib generate`) so unit tests fully control resolution and
    // a stray `STREAMLIB_REGISTRY_URL` in the shell can't redirect a resolve
    // into a live fetch. `None` means "no registry" — a `Registry` dep then
    // fails loud with `RegistryNotConfigured`.
    let registry = options.registry.as_ref();

    // Active `streamlib link` checkout (a non-locked build only; the
    // orchestrator withholds it for locked/distribution builds). When `Some`, a
    // dep present in the checkout's `packages/` tree resolves from there — the
    // schema-dep mirror of the module loader's link redirect. `None` (the
    // dominant case) ⇒ resolution byte-identical to before.
    let link_checkout = options.link_checkout.as_deref();

    let root = build_resolved_package(root_manifest, root_dir.to_path_buf(), ResolvedSource::Root)?;

    // The dep map is now typed end-to-end: `BTreeMap<PackageRef, _>`. The
    // canonical-string lookup-key invariants the resolver previously
    // hand-validated in `parse_dep_key` are now enforced by `PackageRef`'s
    // Deserialize at YAML-read time — invalid keys never reach this code.
    //
    // Each queue entry carries (consumer_dir, deps, patch): when iterating
    // the consumer's deps we consult the SAME consumer's `patch:` table
    // for resolution overrides. No tree-level walk-up — what's in the
    // consumer's manifest is what the resolver sees.
    let mut packages: BTreeMap<String, ResolvedPackage> = BTreeMap::new();
    let mut queue: VecDeque<QueueEntry> = VecDeque::new();
    queue.push_back(QueueEntry {
        consumer_dir: root.root_dir.clone(),
        dependencies: root.manifest.dependencies.clone(),
        patch: root.manifest.patch.clone(),
    });

    let mut visiting: HashSet<String> = HashSet::new();

    while let Some(QueueEntry {
        consumer_dir,
        dependencies,
        patch,
    }) = queue.pop_front()
    {
        for (dep_ref, spec) in dependencies {
            // Lockfile + return-shape are still keyed on the canonical
            // joined-string form (yaml map keys are strings on disk). The
            // typed PackageRef is the in-memory primary; `Display` is the
            // wire form, used here only for the lockfile key.
            let dep_id = dep_ref.to_string();

            if let Some(existing) = packages.get(&dep_id) {
                check_existing_satisfies_spec(existing, &spec, &consumer_dir, &dep_id)?;
                continue;
            }

            if !visiting.insert(dep_id.clone()) {
                return Err(ResolverError::CircularDependency { chain: dep_id });
            }

            let resolved = resolve_one(
                &consumer_dir,
                &dep_ref,
                &dep_id,
                &spec,
                &patch,
                &cache_dir,
                registry,
                link_checkout,
            )?;

            check_resolved_id_matches(&resolved, &dep_id, &consumer_dir)?;
            check_resolved_satisfies_spec(&resolved, &spec, &consumer_dir, &dep_id)?;

            queue.push_back(QueueEntry {
                consumer_dir: resolved.root_dir.clone(),
                dependencies: resolved.manifest.dependencies.clone(),
                patch: resolved.manifest.patch.clone(),
            });
            packages.insert(dep_id.clone(), resolved);
            visiting.remove(&dep_id);
        }
    }

    Ok(ResolvedPackages { root, packages })
}

struct QueueEntry {
    consumer_dir: PathBuf,
    dependencies: BTreeMap<PackageRef, DependencySpec>,
    patch: BTreeMap<PackageRef, DependencySpec>,
}

#[allow(clippy::too_many_arguments)]
fn resolve_one(
    consumer_dir: &Path,
    dep_ref: &PackageRef,
    dep_id: &str,
    spec: &DependencySpec,
    patch: &BTreeMap<PackageRef, DependencySpec>,
    cache_dir: &Path,
    registry: Option<&RegistryConfig>,
    link_checkout: Option<&Path>,
) -> ResolverResult<ResolvedPackage> {
    // Link-aware short-circuit — the schema-dep mirror of the module loader's
    // `streamlib link` redirect (npm-link precedence): when a link is active
    // and this dep is present in the checkout's `packages/<name>` tree, resolve
    // it from the checkout, overriding whatever `dependencies:` / `patch:`
    // declared. Absent-from-checkout falls through to the by-version / patch
    // path below, and `link_checkout` is `None` for every non-linked build
    // (and every locked run), so this branch changes nothing off the dev loop.
    // Discovery of the link itself (the `.streamlib/link.json` marker) happens
    // once in the build orchestrator; here we only look a dep up within the
    // already-resolved checkout path.
    if let Some(checkout) = link_checkout
        && let Some(pkg_dir) = checkout_package_dir(checkout, dep_ref)?
    {
        tracing::info!(
            dependency = dep_id,
            checkout = %checkout.display(),
            source = %pkg_dir.display(),
            "streamlib link active — resolving schema dep from linked checkout \
             (overriding declared spec)"
        );
        let manifest = Manifest::load(&pkg_dir)?;
        return build_resolved_package(
            manifest,
            pkg_dir.clone(),
            ResolvedSource::Path { relative: pkg_dir },
        );
    }
    // Consumer's `patch:` table overrides the dep declaration when present —
    // Cargo `[patch.crates-io]` semantics: `dependencies:` declares *what* the
    // consumer needs, `patch:` declares *which copy* to use.
    //
    // A path patch is a **dev-loop-only** affordance (the two-loops model in
    // docs/architecture/package-development-model.md). It ships inside the
    // published artifact, but its monorepo-relative target only exists in a dev
    // checkout:
    //   * dev loop     — target exists → the patch wins (byte-identical to an
    //                    in-tree resolve).
    //   * distribution — target absent (a standalone consumer of the published
    //                    crate) → fall back to the declared `spec`, resolving
    //                    the dep by version from the configured registry /
    //                    `.slpkg` store.
    // A path declared directly in `dependencies:` (no patch) still fails loud
    // with `PathDependencyNotFound` when absent — there is no version range to
    // fall back to, and a missing direct path source is a genuine error.
    let effective_spec = match patch.get(dep_ref) {
        Some(DependencySpec::Path(path_dep))
            if !path_dependency_absolute_path(consumer_dir, path_dep).exists() =>
        {
            tracing::debug!(
                dependency = dep_id,
                patch_path = %path_dependency_absolute_path(consumer_dir, path_dep).display(),
                "dev path patch target absent; falling back to declared version spec \
                 (two-loops distribution resolution)"
            );
            spec
        }
        Some(patched) => patched,
        None => spec,
    };
    match effective_spec {
        DependencySpec::Path(path_dep) => {
            resolve_path_dependency(consumer_dir, dep_id, path_dep, cache_dir)
        }
        DependencySpec::Git(git_dep) => {
            let target = fetch_git(dep_id, &git_dep.git, &git_dep.rev, cache_dir)?;
            let manifest = Manifest::load(&target)?;
            build_resolved_package(
                manifest,
                target,
                ResolvedSource::Git {
                    url: git_dep.git.clone(),
                    rev: git_dep.rev.clone(),
                },
            )
        }
        DependencySpec::Registry(reg) => {
            resolve_registry_dependency(dep_ref, dep_id, reg, cache_dir, registry)
        }
    }
}

/// If `dep_ref` (`@org/name`) is present in the linked checkout's `packages/`
/// tree, return its source dir. The match is by the manifest's declared
/// `package.org` + `package.name` (not the directory name), so a package whose
/// directory differs from its name still resolves. `Ok(None)` when the dep is
/// absent — the caller then resolves it by version / patch, unchanged.
///
/// This mirrors the module loader's `ActiveLinkedCheckout::checkout_package_dir`
/// (`core/runtime/module_loader/source.rs`); the two live in different crates
/// (engine vs. idents) and can't share the type, but the lookup shape is the
/// same so a linked package and its linked schema deps resolve from one tree.
fn checkout_package_dir(checkout: &Path, dep_ref: &PackageRef) -> ResolverResult<Option<PathBuf>> {
    let packages_root = checkout.join("packages");
    if !packages_root.is_dir() {
        return Ok(None);
    }
    // Fast path: the standard monorepo layout is `packages/<name>`.
    let by_name = packages_root.join(dep_ref.name.as_str());
    if manifest_declares_package(&by_name, dep_ref) {
        return Ok(Some(by_name));
    }
    // Fallback: scan for a package dir whose manifest declares this ident
    // (covers a package whose directory name differs from its package name).
    let entries = std::fs::read_dir(&packages_root).map_err(|e| ResolverError::Io {
        path: packages_root.clone(),
        source: e,
    })?;
    for entry in entries {
        let dir = entry
            .map_err(|e| ResolverError::Io {
                path: packages_root.clone(),
                source: e,
            })?
            .path();
        if dir == by_name || !dir.is_dir() {
            continue;
        }
        if manifest_declares_package(&dir, dep_ref) {
            return Ok(Some(dir));
        }
    }
    Ok(None)
}

/// Whether the `streamlib.yaml` at `dir` declares a package whose org + name
/// equal `dep_ref`. A missing / unreadable / malformed manifest is treated as
/// "no match" — this dir just isn't the package we're looking for; a genuine
/// manifest error surfaces on the real resolve path with a precise message.
fn manifest_declares_package(dir: &Path, dep_ref: &PackageRef) -> bool {
    if !dir.join(Manifest::FILE_NAME).exists() {
        return false;
    }
    match Manifest::load(dir) {
        Ok(m) => m
            .package
            .as_ref()
            .is_some_and(|p| p.org == dep_ref.org && p.name == dep_ref.name),
        Err(_) => false,
    }
}

/// Resolve a `Registry` schema dependency from the static registry's generic
/// store: list the package's available versions, select the highest satisfying
/// the declared range, fetch + extract that version's `.slpkg`, and load it.
///
/// The flat generic registry has no semver-range query, so range → concrete
/// version happens client-side (cargo/npm/pip shape). The resolved concrete
/// version is recorded in the lockfile via [`ResolvedSource::Registry`].
fn resolve_registry_dependency(
    dep_ref: &PackageRef,
    dep_id: &str,
    reg: &RegistryDependency,
    cache_dir: &Path,
    registry: Option<&RegistryConfig>,
) -> ResolverResult<ResolvedPackage> {
    let config = registry.ok_or_else(|| ResolverError::RegistryNotConfigured {
        name: dep_id.to_string(),
        env: crate::registry::REGISTRY_URL_ENV.to_string(),
    })?;
    let client = RegistryClient::new(config);
    let available = client.list_versions(dep_ref)?;
    let selected = select_version(dep_ref, &reg.version, &available)?;
    let (bytes, url) = client.download_slpkg(dep_ref, selected)?;
    let archive = cache_slpkg_bytes(dep_ref, &bytes, cache_dir)?;
    let extracted = extract_slpkg(&archive, cache_dir)?;
    let manifest = Manifest::load(&extracted)?;
    build_resolved_package(manifest, extracted, ResolvedSource::Registry { url })
}

/// Absolute path a [`PathDependency`] resolves to, relative to `consumer_dir`
/// (an absolute `path:` value passes through unchanged). Shared by
/// [`resolve_path_dependency`] and the dev-patch-fallback existence check in
/// [`resolve_one`] so both agree on exactly which path a patch names.
fn path_dependency_absolute_path(
    consumer_dir: &Path,
    path_dep: &crate::manifest::PathDependency,
) -> PathBuf {
    if path_dep.path.is_absolute() {
        path_dep.path.clone()
    } else {
        consumer_dir.join(&path_dep.path)
    }
}

fn resolve_path_dependency(
    consumer_dir: &Path,
    dep_id: &str,
    path_dep: &crate::manifest::PathDependency,
    cache_dir: &Path,
) -> ResolverResult<ResolvedPackage> {
    let abs = path_dependency_absolute_path(consumer_dir, path_dep);
    if !abs.exists() {
        return Err(ResolverError::PathDependencyNotFound {
            name: dep_id.to_string(),
            path: abs,
        });
    }
    // `.slpkg` archive (path-flavored): extract first.
    if abs.extension().and_then(|s| s.to_str()) == Some("slpkg") {
        let extracted = extract_slpkg(&abs, cache_dir)?;
        let manifest = Manifest::load(&extracted)?;
        return build_resolved_package(manifest, extracted, ResolvedSource::Slpkg { archive: abs });
    }
    if !abs.is_dir() {
        return Err(ResolverError::PathDependencyNotDirectory {
            name: dep_id.to_string(),
            path: abs,
        });
    }
    let manifest = Manifest::load(&abs)?;
    let relative = path_dep.path.clone();
    build_resolved_package(manifest, abs, ResolvedSource::Path { relative })
}

fn build_resolved_package(
    manifest: Manifest,
    root_dir: PathBuf,
    source: ResolvedSource,
) -> ResolverResult<ResolvedPackage> {
    let schema_files = discover_schema_files(&manifest, &root_dir)?;
    let content_hash = hash_package_contents(&manifest, &root_dir, &schema_files)?;

    Ok(ResolvedPackage {
        manifest,
        root_dir,
        schema_files,
        source,
        content_hash,
    })
}

/// Hash a package's manifest body + schema files — the single content-hash
/// routine behind both resolver-time hashing and package-dir
/// re-verification ([`content_hash_for_package_dir`]).
fn hash_package_contents(
    manifest: &Manifest,
    root_dir: &Path,
    schema_files: &[PathBuf],
) -> ResolverResult<String> {
    let manifest_path = root_dir.join(Manifest::FILE_NAME);
    let manifest_body = if manifest_path.exists() {
        std::fs::read_to_string(&manifest_path).map_err(|e| ResolverError::ManifestRead {
            path: manifest_path.clone(),
            source: e,
        })?
    } else {
        // Manifests synthesized in tests may have no on-disk file; serialize.
        serde_yaml::to_string(manifest).map_err(|e| ResolverError::ManifestParse {
            path: manifest_path.clone(),
            source: e,
        })?
    };

    let mut schema_pairs = Vec::with_capacity(schema_files.len());
    for path in schema_files {
        let rel = path
            .strip_prefix(root_dir)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned());
        let body = std::fs::read_to_string(path).map_err(|e| ResolverError::Io {
            path: path.clone(),
            source: e,
        })?;
        schema_pairs.push((rel, body));
    }

    Ok(compute_content_hash(&manifest_body, &schema_pairs))
}

/// Content hash of the package rooted at `root_dir`: SHA-256 over its
/// `streamlib.yaml` body + every schema file it owns, via the exact same
/// discovery + hashing routine the resolver uses when pinning
/// `content_hash` into a lockfile entry. Callers use it to re-verify a
/// materialized package directory (e.g. an installed-cache slot at locked
/// run time) against a lockfile pin.
pub fn content_hash_for_package_dir(root_dir: &Path) -> ResolverResult<String> {
    let manifest = Manifest::load(root_dir)?;
    let schema_files = discover_schema_files(&manifest, root_dir)?;
    hash_package_contents(&manifest, root_dir, &schema_files)
}

/// Discover the schema files this manifest owns. Two modes:
///
/// 1. Explicit: `manifest.schemas: { Name: { file | package } }`. `Local`
///    entries contribute their `file:` path; `External` entries do not (they
///    declare imports, not files this package owns).
/// 2. Implicit: every `*.yaml` under `<root_dir>/schemas/` (sorted) — used
///    when `schemas:` is omitted, mostly for tests and as a back-compat
///    convenience.
fn discover_schema_files(manifest: &Manifest, root_dir: &Path) -> ResolverResult<Vec<PathBuf>> {
    if let Some(declared) = &manifest.schemas {
        let mut files = Vec::new();
        for (_name, entry) in declared {
            let SchemaEntry::Local { file } = entry else {
                continue;
            };
            let abs = if file.is_absolute() {
                file.clone()
            } else {
                root_dir.join(file)
            };
            if !abs.exists() {
                return Err(ResolverError::SchemaNotFound {
                    path: abs,
                    from: root_dir.join(Manifest::FILE_NAME),
                });
            }
            files.push(abs);
        }
        files.sort();
        return Ok(files);
    }

    let schemas_dir = root_dir.join("schemas");
    if !schemas_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let entries = std::fs::read_dir(&schemas_dir).map_err(|e| ResolverError::Io {
        path: schemas_dir.clone(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| ResolverError::Io {
            path: schemas_dir.clone(),
            source: e,
        })?;
        let path = entry.path();
        let ext = path.extension().and_then(|s: &OsStr| s.to_str());
        if matches!(ext, Some("yaml") | Some("yml")) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Resolve a bare-name schema reference for a given root package.
///
/// Walks the manifest's `schemas:` map: `Local` entries point at this
/// package's own schema files; `External { package }` entries delegate to
/// the named dependency's `schemas:` map, recursively. Returns the
/// owning [`ResolvedPackage`] plus the absolute path of the schema YAML.
///
/// Use this from build-time / startup-time consumers (codegen, validator,
/// runtime registration). Do not call on the hot path.
pub fn resolve_bare_schema_name<'a>(
    packages: &'a ResolvedPackages,
    root: &'a ResolvedPackage,
    name: &TypeName,
) -> ResolverResult<(&'a ResolvedPackage, PathBuf)> {
    resolve_bare_schema_name_internal(packages, root, name, &mut Vec::new())
}

fn resolve_bare_schema_name_internal<'a>(
    packages: &'a ResolvedPackages,
    root: &'a ResolvedPackage,
    name: &TypeName,
    chain: &mut Vec<String>,
) -> ResolverResult<(&'a ResolvedPackage, PathBuf)> {
    let pkg_id = root
        .manifest
        .package_id()
        .unwrap_or_else(|| "<root>".into());
    chain.push(pkg_id.clone());

    let declared =
        root.manifest
            .schemas
            .as_ref()
            .ok_or_else(|| ResolverError::BareSchemaNameUnresolved {
                name: name.as_str().to_string(),
                package: pkg_id.clone(),
                chain: chain.clone(),
            })?;
    let entry = declared
        .get(name)
        .ok_or_else(|| ResolverError::BareSchemaNameUnresolved {
            name: name.as_str().to_string(),
            package: pkg_id.clone(),
            chain: chain.clone(),
        })?;

    match entry {
        SchemaEntry::Local { file } => {
            let abs = if file.is_absolute() {
                file.clone()
            } else {
                root.root_dir.join(file)
            };
            Ok((root, abs))
        }
        SchemaEntry::External { package } => {
            let dep_id = package.to_string();
            let dep = packages.packages.get(&dep_id).ok_or_else(|| {
                ResolverError::BareSchemaNameDepMissing {
                    name: name.as_str().to_string(),
                    package: pkg_id.clone(),
                    dep: dep_id.clone(),
                }
            })?;
            // Guard against a mutually- or self-referential external chain
            // (A -> B -> A): without it the recursion never terminates and
            // aborts on stack overflow instead of surfacing a typed error.
            if chain.contains(&dep_id) {
                chain.push(dep_id);
                return Err(ResolverError::BareSchemaNameCycle {
                    name: name.as_str().to_string(),
                    package: pkg_id.clone(),
                    chain: chain.clone(),
                });
            }
            resolve_bare_schema_name_internal(packages, dep, name, chain)
        }
    }
}

fn check_resolved_id_matches(
    resolved: &ResolvedPackage,
    requested: &str,
    consumer_dir: &Path,
) -> ResolverResult<()> {
    match resolved.manifest.package_id() {
        Some(declared) if declared == requested => Ok(()),
        Some(declared) => Err(ResolverError::PackageIdMismatch {
            path: consumer_dir.join(Manifest::FILE_NAME),
            declared,
            requested: requested.to_string(),
        }),
        None => Err(ResolverError::PackageIdMismatch {
            path: consumer_dir.join(Manifest::FILE_NAME),
            declared: "<no package block>".into(),
            requested: requested.to_string(),
        }),
    }
}

fn check_resolved_satisfies_spec(
    resolved: &ResolvedPackage,
    spec: &DependencySpec,
    consumer_dir: &Path,
    dep_id: &str,
) -> ResolverResult<()> {
    let DependencySpec::Registry(reg) = spec else {
        return Ok(());
    };
    let v = resolved
        .manifest
        .package
        .as_ref()
        .map(|p| p.version)
        .ok_or_else(|| ResolverError::PackageIdMismatch {
            path: consumer_dir.join(Manifest::FILE_NAME),
            declared: "<no package block>".into(),
            requested: dep_id.to_string(),
        })?;
    if !reg.version.matches(v) {
        return Err(ResolverError::VersionRangeUnsatisfied {
            name: dep_id.to_string(),
            from: consumer_dir.join(Manifest::FILE_NAME),
            found: v.to_string(),
            range: reg.version.to_string(),
        });
    }
    Ok(())
}

fn check_existing_satisfies_spec(
    existing: &ResolvedPackage,
    spec: &DependencySpec,
    consumer_dir: &Path,
    dep_id: &str,
) -> ResolverResult<()> {
    if let DependencySpec::Registry(reg) = spec {
        let v = existing
            .manifest
            .package
            .as_ref()
            .map(|p| p.version)
            .expect("existing dep is package-flavor");
        if !reg.version.matches(v) {
            return Err(ResolverError::VersionRangeConflict {
                name: dep_id.to_string(),
                range_a: format!("(already-resolved {})", v),
                from_a: existing.root_dir.join(Manifest::FILE_NAME),
                range_b: reg.version.to_string(),
                from_b: consumer_dir.join(Manifest::FILE_NAME),
            });
        }
    }
    Ok(())
}

fn default_cache_dir() -> ResolverResult<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| ResolverError::Io {
            path: PathBuf::from("$HOME"),
            source: std::io::Error::other("HOME environment variable not set"),
        })?;
    Ok(home.join(".streamlib").join("resolver-cache"))
}

fn extract_slpkg(archive: &Path, cache_dir: &Path) -> ResolverResult<PathBuf> {
    let archive_bytes = std::fs::read(archive).map_err(|e| ResolverError::SlpkgExtractFailed {
        path: archive.to_path_buf(),
        message: format!("read failed: {e}"),
    })?;
    let archive_hash = crate::lockfile::hash_content(&archive_bytes);
    let safe_hash = archive_hash.replace(':', "_");
    let target = cache_dir.join("slpkg").join(safe_hash);

    let manifest_path = target.join(Manifest::FILE_NAME);
    if manifest_path.exists() {
        return Ok(target);
    }

    std::fs::create_dir_all(&target).map_err(|e| ResolverError::Io {
        path: target.clone(),
        source: e,
    })?;

    let cursor = std::io::Cursor::new(&archive_bytes);
    let mut zip = zip::ZipArchive::new(cursor).map_err(|e| ResolverError::SlpkgExtractFailed {
        path: archive.to_path_buf(),
        message: format!("not a valid zip: {e}"),
    })?;

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| ResolverError::SlpkgExtractFailed {
                path: archive.to_path_buf(),
                message: format!("entry {i} read failed: {e}"),
            })?;
        let entry_name = entry.name().to_string();
        // Reject path traversal.
        if entry_name.contains("..") || entry_name.starts_with('/') {
            return Err(ResolverError::SlpkgExtractFailed {
                path: archive.to_path_buf(),
                message: format!("rejected unsafe entry path: {entry_name}"),
            });
        }
        let out_path = target.join(&entry_name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path).map_err(|e| ResolverError::Io {
                path: out_path,
                source: e,
            })?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ResolverError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let mut out = std::fs::File::create(&out_path).map_err(|e| ResolverError::Io {
            path: out_path.clone(),
            source: e,
        })?;
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| ResolverError::SlpkgExtractFailed {
                path: archive.to_path_buf(),
                message: format!("read entry {entry_name} failed: {e}"),
            })?;
        std::io::Write::write_all(&mut out, &buf).map_err(|e| ResolverError::Io {
            path: out_path,
            source: e,
        })?;
    }

    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_yaml(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn write_streamlib_yaml(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        write_yaml(dir, Manifest::FILE_NAME, body);
    }

    #[test]
    fn resolve_root_only_no_dependencies() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        let res = resolve(&root).unwrap();
        assert!(res.packages.is_empty());
        assert_eq!(
            res.root.manifest.package_id().as_deref(),
            Some("@tatolab/core")
        );
        assert!(res.root.content_hash.starts_with("sha256:"));
        assert!(matches!(res.root.source, ResolvedSource::Root));
    }

    #[test]
    fn resolve_path_dependency() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let core = tmp.path().join("core");

        write_streamlib_yaml(
            &core,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        std::fs::create_dir_all(core.join("schemas")).unwrap();
        write_yaml(
            &core.join("schemas"),
            "VideoFrame.yaml",
            "metadata:\n  name: VideoFrame\nproperties:\n  width:\n    type: uint32\n",
        );

        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );

        let res = resolve(&root).unwrap();
        assert_eq!(res.packages.len(), 1);
        let core_pkg = res.packages.get("@tatolab/core").unwrap();
        assert_eq!(core_pkg.schema_files.len(), 1);
        assert!(matches!(core_pkg.source, ResolvedSource::Path { .. }));
        assert!(core_pkg.content_hash.starts_with("sha256:"));
    }

    #[test]
    fn resolve_missing_path_dependency() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        let err = resolve(&root).unwrap_err();
        assert!(matches!(err, ResolverError::PathDependencyNotFound { .. }));
    }

    #[test]
    fn resolve_path_dependency_id_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let core = tmp.path().join("core");
        write_streamlib_yaml(
            &core,
            "package:\n  org: tatolab\n  name: notcore\n  version: 1.0.0\n",
        );
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        let err = resolve(&root).unwrap_err();
        assert!(matches!(err, ResolverError::PackageIdMismatch { .. }));
    }

    #[test]
    fn resolve_invalid_dep_key_shape() {
        // Post-#717 the canonical-key shape is enforced at YAML parse time
        // by `PackageRef::Deserialize`, not in the resolver. A bare
        // `"tatolab/core"` (missing `@` prefix) fails to deserialize as a
        // PackageRef and surfaces as a `ManifestParse` error. The structural
        // intent — that invalid shapes can't reach the resolver's lookup
        // logic — is preserved; the rejection just moves earlier.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "tatolab/core":
    path: ../core
"#,
        );
        let err = resolve(&root).unwrap_err();
        assert!(
            matches!(err, ResolverError::ManifestParse { .. }),
            "expected ManifestParse, got {:?}",
            err,
        );
    }

    #[test]
    fn resolve_transitive_path_dependencies() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let h264 = tmp.path().join("h264");
        let core = tmp.path().join("core");

        write_streamlib_yaml(
            &core,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        write_streamlib_yaml(
            &h264,
            r#"
package:
  org: tatolab
  name: h264
  version: 0.4.0
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/h264":
    path: ../h264
"#,
        );

        let res = resolve(&root).unwrap();
        assert_eq!(res.packages.len(), 2);
        assert!(res.packages.contains_key("@tatolab/core"));
        assert!(res.packages.contains_key("@tatolab/h264"));
    }

    #[test]
    fn resolve_diamond_dependency_dedupes() {
        // root → a, b
        // a → core
        // b → core
        // Expect `core` resolved once, not twice.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let core = tmp.path().join("core");

        write_streamlib_yaml(
            &core,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        write_streamlib_yaml(
            &a,
            r#"
package:
  org: tatolab
  name: a
  version: 1.0.0
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        write_streamlib_yaml(
            &b,
            r#"
package:
  org: tatolab
  name: b
  version: 1.0.0
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/a":
    path: ../a
  "@tatolab/b":
    path: ../b
"#,
        );

        let res = resolve(&root).unwrap();
        assert_eq!(res.packages.len(), 3);
        assert!(res.packages.contains_key("@tatolab/core"));
        assert!(res.packages.contains_key("@tatolab/a"));
        assert!(res.packages.contains_key("@tatolab/b"));
    }

    #[test]
    fn registry_dependency_without_config_errors() {
        // A bare registry range with `registry: None` fails loud with
        // RegistryNotConfigured — the actionable successor to the old
        // RegistryNotImplemented. `resolve_with` is pure (it never reads the
        // process env), so this is deterministic regardless of any ambient
        // STREAMLIB_REGISTRY_URL in the shell.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/core": "^1.0.0"
"#,
        );
        let opts = ResolverOptions {
            cache_dir: Some(tmp.path().join("cache")),
            registry: None,
            link_checkout: None,
        };
        let err = resolve_with(&root, &opts).unwrap_err();
        assert!(
            matches!(err, ResolverError::RegistryNotConfigured { .. }),
            "expected RegistryNotConfigured, got {err:?}"
        );
    }

    /// End-to-end registry resolution over the hermetic `file://` mirror:
    /// build a real schema `.slpkg`, lay it out in a versioned mirror dir,
    /// declare a bare semver-range registry dep (NO path patch), and assert
    /// the resolver lists → selects-highest-in-range → fetches → extracts →
    /// loads it. This is exactly the path `build.rs` codegen drives for a
    /// registry-cached crate. Mirrors the broken case the issue fixes: with
    /// the patch stripped, the bare range MUST resolve from the registry.
    #[test]
    fn registry_dependency_resolves_from_file_mirror() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("mirror");
        let cache = tmp.path().join("cache");

        // Two versions present; the ^1.0.0 range must select 1.2.0 over
        // 1.0.0 (and ignore the out-of-range 2.0.0).
        let make_slpkg = |dir: &Path, version: &str| {
            std::fs::create_dir_all(dir).unwrap();
            let archive = dir.join("escalate.slpkg");
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file(Manifest::FILE_NAME, opts).unwrap();
            zip.write_all(
                format!(
                    "package:\n  org: tatolab\n  name: escalate\n  version: {version}\nschemas:\n  EscalateRequest:\n    file: schemas/EscalateRequest.yaml\n"
                )
                .as_bytes(),
            )
            .unwrap();
            zip.start_file("schemas/EscalateRequest.yaml", opts)
                .unwrap();
            zip.write_all(b"metadata:\n  name: EscalateRequest\nproperties: {}\n")
                .unwrap();
            zip.finish().unwrap();
        };
        let slpkg = mirror.join("slpkg");
        make_slpkg(&slpkg.join("escalate").join("1.0.0"), "1.0.0");
        make_slpkg(&slpkg.join("escalate").join("1.2.0"), "1.2.0");
        make_slpkg(&slpkg.join("escalate").join("2.0.0"), "2.0.0");

        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/escalate": "^1.0.0"
"#,
        );

        let opts = ResolverOptions {
            cache_dir: Some(cache),
            registry: Some(crate::RegistryConfig {
                base_url: format!("file://{}", mirror.display()),
            }),
            link_checkout: None,
        };
        let res = resolve_with(&root, &opts).unwrap();
        let escalate = res.packages.get("@tatolab/escalate").unwrap();
        assert!(matches!(escalate.source, ResolvedSource::Registry { .. }));
        // Highest-in-range selected.
        assert_eq!(
            escalate
                .manifest
                .package
                .as_ref()
                .unwrap()
                .version
                .to_string(),
            "1.2.0"
        );
        assert_eq!(escalate.schema_files.len(), 1);

        // Lockfile records the registry source + concrete version.
        let lock = res.to_lockfile();
        let entry = lock.packages.get("@tatolab/escalate").unwrap();
        assert_eq!(entry.version.to_string(), "1.2.0");
        assert!(matches!(entry.source, LockfileSource::Registry { .. }));
    }

    /// Write a minimal escalate schema `.slpkg` into a `file://` mirror's
    /// generic store at `<mirror>/slpkg/escalate/<version>/escalate.slpkg`.
    fn write_escalate_slpkg(mirror: &Path, version: &str) {
        use std::io::Write;
        let dir = mirror.join("slpkg").join("escalate").join(version);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("escalate.slpkg");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file(Manifest::FILE_NAME, opts).unwrap();
        zip.write_all(
            format!(
                "package:\n  org: tatolab\n  name: escalate\n  version: {version}\nschemas:\n  EscalateRequest:\n    file: schemas/EscalateRequest.yaml\n"
            )
            .as_bytes(),
        )
        .unwrap();
        zip.start_file("schemas/EscalateRequest.yaml", opts)
            .unwrap();
        zip.write_all(b"metadata:\n  name: EscalateRequest\nproperties: {}\n")
            .unwrap();
        zip.finish().unwrap();
    }

    /// Distribution loop: a dev path patch whose target is absent (the shape
    /// `streamlib-engine`'s manifest ships — a bare registry range in
    /// `dependencies:` overridden by a monorepo-relative `patch: { path }`
    /// that doesn't exist for a standalone consumer) falls back to resolving
    /// the declared version from the registry rather than failing with
    /// `PathDependencyNotFound`. This is the regression lock for the fix:
    /// reverting the fallback makes the patched Path spec authoritative and
    /// the resolve panics on the missing path.
    #[test]
    fn dev_path_patch_absent_falls_back_to_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("mirror");
        write_escalate_slpkg(&mirror, "1.0.0");
        write_escalate_slpkg(&mirror, "1.2.0");

        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/escalate": "^1.0.0"
patch:
  "@tatolab/escalate":
    path: ../packages/escalate
"#,
        );

        let opts = ResolverOptions {
            cache_dir: Some(tmp.path().join("cache")),
            registry: Some(crate::RegistryConfig {
                base_url: format!("file://{}", mirror.display()),
            }),
            link_checkout: None,
        };
        let res = resolve_with(&root, &opts).unwrap();
        let escalate = res.packages.get("@tatolab/escalate").unwrap();
        // Resolved from the registry (patch path was absent), highest-in-range.
        assert!(matches!(escalate.source, ResolvedSource::Registry { .. }));
        assert_eq!(
            escalate
                .manifest
                .package
                .as_ref()
                .unwrap()
                .version
                .to_string(),
            "1.2.0"
        );
    }

    /// Dev loop: when the path patch's target DOES exist (the monorepo
    /// checkout), the patch still wins over the registry — byte-identical to
    /// pre-fix behavior. A different local version proves the local path, not
    /// the registry copy, was resolved.
    #[test]
    fn dev_path_patch_present_wins_over_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("mirror");
        // Registry holds 1.2.0; the local dev checkout is a different version
        // so the source of truth is unambiguous.
        write_escalate_slpkg(&mirror, "1.2.0");

        let local = tmp.path().join("packages").join("escalate");
        write_streamlib_yaml(
            &local,
            "package:\n  org: tatolab\n  name: escalate\n  version: 1.5.0\n",
        );

        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/escalate": "^1.0.0"
patch:
  "@tatolab/escalate":
    path: ../packages/escalate
"#,
        );

        let opts = ResolverOptions {
            cache_dir: Some(tmp.path().join("cache")),
            registry: Some(crate::RegistryConfig {
                base_url: format!("file://{}", mirror.display()),
            }),
            link_checkout: None,
        };
        let res = resolve_with(&root, &opts).unwrap();
        let escalate = res.packages.get("@tatolab/escalate").unwrap();
        // Local path patch won; the registry's 1.2.0 was NOT consulted.
        assert!(matches!(escalate.source, ResolvedSource::Path { .. }));
        assert_eq!(
            escalate
                .manifest
                .package
                .as_ref()
                .unwrap()
                .version
                .to_string(),
            "1.5.0"
        );
    }

    #[test]
    fn registry_dependency_no_matching_version_errors() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("mirror");
        let dir = mirror.join("slpkg").join("escalate").join("2.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join("escalate.slpkg");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let zopts = zip::write::SimpleFileOptions::default();
        zip.start_file(Manifest::FILE_NAME, zopts).unwrap();
        zip.write_all(b"package:\n  org: tatolab\n  name: escalate\n  version: 2.0.0\n")
            .unwrap();
        zip.finish().unwrap();

        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/escalate": "^1.0.0"
"#,
        );
        let opts = ResolverOptions {
            cache_dir: Some(tmp.path().join("cache")),
            registry: Some(crate::RegistryConfig {
                base_url: format!("file://{}", mirror.display()),
            }),
            link_checkout: None,
        };
        let err = resolve_with(&root, &opts).unwrap_err();
        assert!(
            matches!(err, ResolverError::RegistryNoMatchingVersion { .. }),
            "expected RegistryNoMatchingVersion, got {err:?}"
        );
    }

    #[test]
    fn slpkg_path_dependency_extracts_and_loads() {
        // Build a minimal .slpkg in a temp dir, then resolve a path dep
        // that points at the archive file.
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("cache");
        let archive_path = tmp.path().join("core.slpkg");

        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file(Manifest::FILE_NAME, opts).unwrap();
        zip.write_all(b"package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n")
            .unwrap();
        zip.start_file("schemas/VideoFrame.yaml", opts).unwrap();
        zip.write_all(b"metadata:\n  name: VideoFrame\nproperties: {}\n")
            .unwrap();
        zip.finish().unwrap();

        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            &format!(
                "dependencies:\n  \"@tatolab/core\":\n    path: {}\n",
                archive_path.to_string_lossy()
            ),
        );

        let opts = ResolverOptions {
            cache_dir: Some(cache),
            registry: None,
            link_checkout: None,
        };
        let res = resolve_with(&root, &opts).unwrap();
        let core = res.packages.get("@tatolab/core").unwrap();
        assert!(matches!(core.source, ResolvedSource::Slpkg { .. }));
        assert_eq!(core.schema_files.len(), 1);
    }

    #[test]
    fn lockfile_built_from_resolved_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let core = tmp.path().join("core");

        write_streamlib_yaml(
            &core,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        write_streamlib_yaml(
            &root,
            r#"
dependencies:
  "@tatolab/core":
    path: ../core
"#,
        );
        let res = resolve(&root).unwrap();
        let lock = res.to_lockfile();
        assert_eq!(lock.version, 1);
        assert_eq!(lock.packages.len(), 1);
        let entry = lock.packages.get("@tatolab/core").unwrap();
        assert_eq!(entry.version.to_string(), "1.0.0");
        assert!(matches!(entry.source, LockfileSource::Path { .. }));
        assert!(entry.content_hash.starts_with("sha256:"));
    }

    #[test]
    fn schema_auto_discovery_picks_yaml_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\n",
        );
        let schemas = root.join("schemas");
        std::fs::create_dir_all(&schemas).unwrap();
        write_yaml(
            &schemas,
            "VideoFrame.yaml",
            "metadata:\n  name: VideoFrame\n",
        );
        write_yaml(
            &schemas,
            "AudioFrame.yaml",
            "metadata:\n  name: AudioFrame\n",
        );
        // Non-yaml files must not be picked up.
        write_yaml(&schemas, "README.md", "readme\n");

        let res = resolve(&root).unwrap();
        assert_eq!(res.root.schema_files.len(), 2);
    }

    #[test]
    fn explicit_schema_map_overrides_auto_discovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");

        std::fs::create_dir_all(root.join("schemas")).unwrap();
        write_yaml(
            &root.join("schemas"),
            "Implicit.yaml",
            "metadata:\n  type: Implicit\n",
        );
        let custom = root.join("custom");
        std::fs::create_dir_all(&custom).unwrap();
        write_yaml(&custom, "Explicit.yaml", "metadata:\n  type: Explicit\n");

        write_streamlib_yaml(
            &root,
            r#"
package:
  org: tatolab
  name: core
  version: 1.0.0
schemas:
  Explicit:
    file: custom/Explicit.yaml
"#,
        );

        let res = resolve(&root).unwrap();
        assert_eq!(res.root.schema_files.len(), 1);
        assert!(res.root.schema_files[0].ends_with("Explicit.yaml"));
    }

    #[test]
    fn external_schema_entry_does_not_contribute_local_files() {
        // External entries declare imported types; the file lives in the
        // dep package, not this one. `schema_files` reflects only the
        // Local entries this manifest owns.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let core = tmp.path().join("core");

        write_streamlib_yaml(
            &core,
            r#"
package:
  org: tatolab
  name: core
  version: 1.0.0
schemas:
  VideoFrame:
    file: schemas/VideoFrame.yaml
"#,
        );
        std::fs::create_dir_all(core.join("schemas")).unwrap();
        write_yaml(
            &core.join("schemas"),
            "VideoFrame.yaml",
            "metadata:\n  type: VideoFrame\n",
        );

        write_streamlib_yaml(
            &root,
            r#"
package:
  org: tatolab
  name: consumer
  version: 1.0.0
dependencies:
  "@tatolab/core":
    path: ../core
schemas:
  VideoFrame:
    package: "@tatolab/core"
"#,
        );

        let res = resolve(&root).unwrap();
        // Root package owns no Local schemas.
        assert!(res.root.schema_files.is_empty());
        // Core package owns one Local schema (VideoFrame.yaml).
        let core_pkg = res.packages.get("@tatolab/core").unwrap();
        assert_eq!(core_pkg.schema_files.len(), 1);

        // Bare-name resolution walks the External edge to core.
        let name = TypeName::new("VideoFrame").unwrap();
        let (owner, file) = resolve_bare_schema_name(&res, &res.root, &name).unwrap();
        assert_eq!(
            owner.manifest.package_id().as_deref(),
            Some("@tatolab/core")
        );
        assert!(file.ends_with("VideoFrame.yaml"));
    }

    /// A -> B -> A mutual `External` re-export of the same type must surface
    /// as a typed error, not recurse until stack overflow. Mentally revert
    /// the `chain.contains` guard in `resolve_bare_schema_name_internal` and
    /// this test aborts the process instead of passing.
    #[test]
    fn bare_schema_name_external_cycle_is_typed_error_not_stack_overflow() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        write_streamlib_yaml(
            &a,
            r#"
package:
  org: tatolab
  name: a
  version: 1.0.0
dependencies:
  "@tatolab/b":
    path: ../b
schemas:
  Loop:
    package: "@tatolab/b"
"#,
        );
        write_streamlib_yaml(
            &b,
            r#"
package:
  org: tatolab
  name: b
  version: 1.0.0
dependencies:
  "@tatolab/a":
    path: ../a
schemas:
  Loop:
    package: "@tatolab/a"
"#,
        );

        let res = resolve(&a).unwrap();
        let name = TypeName::new("Loop").unwrap();
        let err = resolve_bare_schema_name(&res, &res.root, &name).unwrap_err();
        match err {
            ResolverError::BareSchemaNameCycle { name, chain, .. } => {
                assert_eq!(name, "Loop");
                assert!(chain.iter().any(|p| p == "@tatolab/a"), "chain: {chain:?}");
                assert!(chain.iter().any(|p| p == "@tatolab/b"), "chain: {chain:?}");
            }
            other => panic!("expected BareSchemaNameCycle, got {other:?}"),
        }
    }

    #[test]
    fn bare_schema_name_unresolved_when_not_in_map() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        write_streamlib_yaml(
            &root,
            r#"
package:
  org: tatolab
  name: foo
  version: 1.0.0
schemas: {}
"#,
        );
        let res = resolve(&root).unwrap();
        let name = TypeName::new("Missing").unwrap();
        let err = resolve_bare_schema_name(&res, &res.root, &name).unwrap_err();
        assert!(matches!(
            err,
            ResolverError::BareSchemaNameUnresolved { .. }
        ));
    }

    // =====================================================================
    // Link-aware schema-dep resolution (`ResolverOptions::link_checkout`)
    //
    // The schema-dep mirror of the module loader's `streamlib link` redirect:
    // when a link is active, a dep present in the checkout's `packages/<name>`
    // tree resolves from the checkout (dev loop, zero-registry); absent-from-
    // checkout and no-link resolve by version from the store (distribution).
    // =====================================================================

    /// Write a minimal `@tatolab/<name>` schema `.slpkg` into a `file://`
    /// mirror's generic store at `<mirror>/slpkg/<name>/<version>/<name>.slpkg`.
    fn write_pkg_slpkg(mirror: &Path, name: &str, version: &str) {
        use std::io::Write;
        let dir = mirror.join("slpkg").join(name).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join(format!("{name}.slpkg"));
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file(Manifest::FILE_NAME, opts).unwrap();
        zip.write_all(
            format!("package:\n  org: tatolab\n  name: {name}\n  version: {version}\n").as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap();
    }

    /// Write `<checkout>/packages/<dir_name>/streamlib.yaml` declaring the
    /// package `@tatolab/<pkg_name>` at `version` — the linked-checkout layout
    /// the resolver looks a dep up in.
    fn write_checkout_package(checkout: &Path, dir_name: &str, pkg_name: &str, version: &str) {
        let pkg = checkout.join("packages").join(dir_name);
        write_streamlib_yaml(
            &pkg,
            &format!("package:\n  org: tatolab\n  name: {pkg_name}\n  version: {version}\n"),
        );
    }

    fn registry_opts(
        cache: &Path,
        mirror: &Path,
        link_checkout: Option<PathBuf>,
    ) -> ResolverOptions {
        ResolverOptions {
            cache_dir: Some(cache.to_path_buf()),
            registry: Some(crate::RegistryConfig {
                base_url: format!("file://{}", mirror.display()),
            }),
            link_checkout,
        }
    }

    /// Dev loop, link active: a schema dep present in the linked checkout's
    /// `packages/<name>` resolves FROM the checkout, overriding the declared
    /// version range — even though the registry holds a *different* version.
    /// This is the zero-registry dev loop for schema deps. Mentally revert the
    /// link-aware short-circuit in `resolve_one` and the resolve falls back to
    /// the registry (1.2.0, `Registry` source), failing both assertions.
    #[test]
    fn link_active_resolves_schema_dep_from_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("mirror");
        // Registry holds a DIFFERENT version so the source-of-truth is unambiguous.
        write_pkg_slpkg(&mirror, "core", "1.2.0");
        // The linked checkout carries @tatolab/core at 1.0.0.
        let checkout = tmp.path().join("checkout");
        write_checkout_package(&checkout, "core", "core", "1.0.0");

        let root = tmp.path().join("project");
        write_streamlib_yaml(&root, "dependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n");

        let opts = registry_opts(&tmp.path().join("cache"), &mirror, Some(checkout.clone()));
        let res = resolve_with(&root, &opts).unwrap();
        let core = res.packages.get("@tatolab/core").unwrap();
        assert!(
            matches!(core.source, ResolvedSource::Path { .. }),
            "linked dep must resolve from the checkout (Path), got {:?}",
            core.source
        );
        assert_eq!(
            core.manifest.package.as_ref().unwrap().version.to_string(),
            "1.0.0",
            "must be the checkout's version, not the registry's 1.2.0"
        );
        assert!(
            core.root_dir.starts_with(&checkout),
            "resolved root_dir must be inside the checkout: {}",
            core.root_dir.display()
        );
    }

    /// No link (`link_checkout: None`): even with a checkout present on disk,
    /// resolution is byte-identical to before — the dep resolves by version
    /// from the store, never the checkout. Locks the link-GATING guardrail
    /// (the checkout is consulted ONLY when a link is active).
    #[test]
    fn no_link_ignores_checkout_and_resolves_from_store() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("mirror");
        write_pkg_slpkg(&mirror, "core", "1.2.0");
        // A checkout exists on disk and carries core, but no link is active.
        let checkout = tmp.path().join("checkout");
        write_checkout_package(&checkout, "core", "core", "1.0.0");

        let root = tmp.path().join("project");
        write_streamlib_yaml(&root, "dependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n");

        let opts = registry_opts(&tmp.path().join("cache"), &mirror, None);
        let res = resolve_with(&root, &opts).unwrap();
        let core = res.packages.get("@tatolab/core").unwrap();
        assert!(
            matches!(core.source, ResolvedSource::Registry { .. }),
            "without a link the dep must resolve from the store, got {:?}",
            core.source
        );
        assert_eq!(
            core.manifest.package.as_ref().unwrap().version.to_string(),
            "1.2.0",
            "must be the registry's version — the on-disk checkout is ignored with no link"
        );
    }

    /// Link active but the dep is ABSENT from the checkout's `packages/` tree:
    /// resolution falls through to the by-version store (distribution loop),
    /// unchanged. A checkout that doesn't carry a given dep must not break it.
    #[test]
    fn link_active_dep_absent_from_checkout_falls_back_to_store() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("mirror");
        write_pkg_slpkg(&mirror, "core", "1.2.0");
        // Checkout exists and has a packages/ tree, but carries a DIFFERENT
        // package (not core).
        let checkout = tmp.path().join("checkout");
        write_checkout_package(&checkout, "camera", "camera", "1.0.0");

        let root = tmp.path().join("project");
        write_streamlib_yaml(&root, "dependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n");

        let opts = registry_opts(&tmp.path().join("cache"), &mirror, Some(checkout));
        let res = resolve_with(&root, &opts).unwrap();
        let core = res.packages.get("@tatolab/core").unwrap();
        assert!(
            matches!(core.source, ResolvedSource::Registry { .. }),
            "a dep absent from the checkout must fall back to the store, got {:?}",
            core.source
        );
        assert_eq!(
            core.manifest.package.as_ref().unwrap().version.to_string(),
            "1.2.0"
        );
    }

    /// The checkout lookup matches by the manifest's declared package ident,
    /// not the directory name: a dep whose checkout directory differs from its
    /// package name still resolves from the checkout (the scan-fallback branch
    /// of `checkout_package_dir`).
    #[test]
    fn link_checkout_matches_by_manifest_ident_not_dir_name() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("mirror");
        write_pkg_slpkg(&mirror, "core", "1.2.0");
        // Directory is `wire-vocab`, but the manifest declares @tatolab/core.
        let checkout = tmp.path().join("checkout");
        write_checkout_package(&checkout, "wire-vocab", "core", "1.0.0");

        let root = tmp.path().join("project");
        write_streamlib_yaml(&root, "dependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n");

        let opts = registry_opts(&tmp.path().join("cache"), &mirror, Some(checkout.clone()));
        let res = resolve_with(&root, &opts).unwrap();
        let core = res.packages.get("@tatolab/core").unwrap();
        assert!(
            matches!(core.source, ResolvedSource::Path { .. }),
            "got {:?}",
            core.source
        );
        assert_eq!(
            core.manifest.package.as_ref().unwrap().version.to_string(),
            "1.0.0"
        );
        assert!(
            core.root_dir.ends_with("wire-vocab"),
            "must resolve the scan-matched dir, got {}",
            core.root_dir.display()
        );
    }

    /// The link redirect threads through the whole dependency walk: a linked
    /// package AND its transitive schema deps all resolve from the checkout.
    #[test]
    fn link_checkout_threads_through_transitive_deps() {
        let tmp = tempfile::tempdir().unwrap();
        let mirror = tmp.path().join("mirror");
        // Registry holds different versions of both, so the source is unambiguous.
        write_pkg_slpkg(&mirror, "core", "1.2.0");
        write_pkg_slpkg(&mirror, "h264", "0.9.0");

        let checkout = tmp.path().join("checkout");
        write_checkout_package(&checkout, "core", "core", "1.0.0");
        // h264 (in the checkout) depends on core.
        write_streamlib_yaml(
            &checkout.join("packages").join("h264"),
            r#"
package:
  org: tatolab
  name: h264
  version: 0.4.0
dependencies:
  "@tatolab/core": "^1.0.0"
"#,
        );

        let root = tmp.path().join("project");
        write_streamlib_yaml(&root, "dependencies:\n  \"@tatolab/h264\": \"^0.4.0\"\n");

        let opts = registry_opts(&tmp.path().join("cache"), &mirror, Some(checkout.clone()));
        let res = resolve_with(&root, &opts).unwrap();

        let h264 = res.packages.get("@tatolab/h264").unwrap();
        let core = res.packages.get("@tatolab/core").unwrap();
        assert!(
            matches!(h264.source, ResolvedSource::Path { .. }),
            "h264: {:?}",
            h264.source
        );
        assert!(
            matches!(core.source, ResolvedSource::Path { .. }),
            "core: {:?}",
            core.source
        );
        assert_eq!(
            h264.manifest.package.as_ref().unwrap().version.to_string(),
            "0.4.0"
        );
        assert_eq!(
            core.manifest.package.as_ref().unwrap().version.to_string(),
            "1.0.0"
        );
        assert!(core.root_dir.starts_with(&checkout));
    }

    /// `ResolverOptions::from_env` reads the checkout from
    /// [`crate::LINK_CHECKOUT_ENV`]; unset/empty ⇒ `None`. Locks the
    /// codegen-boundary env read the build orchestrator threads through.
    #[test]
    #[serial_test::serial]
    fn from_env_reads_link_checkout() {
        // SAFETY: `#[serial]` makes the env-mutating tests mutually exclusive.
        let prev = std::env::var_os(crate::LINK_CHECKOUT_ENV);
        unsafe {
            std::env::set_var(crate::LINK_CHECKOUT_ENV, "/opt/streamlib-checkout");
        }
        assert_eq!(
            ResolverOptions::from_env().link_checkout,
            Some(PathBuf::from("/opt/streamlib-checkout"))
        );
        unsafe {
            std::env::set_var(crate::LINK_CHECKOUT_ENV, "");
        }
        assert_eq!(
            ResolverOptions::from_env().link_checkout,
            None,
            "empty env must read as None"
        );
        unsafe {
            match prev {
                Some(v) => std::env::set_var(crate::LINK_CHECKOUT_ENV, v),
                None => std::env::remove_var(crate::LINK_CHECKOUT_ENV),
            }
        }
    }

    /// Write a `.streamlib/link.json` marker at `root` recording `checkout` —
    /// the shape `streamlib link --engine` persists, enough for
    /// `find_and_load_active_link` walk-up discovery.
    fn write_link_marker(root: &Path, checkout: &Path) {
        use crate::link_marker::{
            LINK_MANIFEST_FILE, LINK_STATE_DIR, LinkManifest, LinkTransactionState,
        };
        let dir = root.join(LINK_STATE_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = LinkManifest {
            checkout: checkout.to_path_buf(),
            python_sdk_path: checkout.join("sdk/streamlib-python"),
            deno_sdk_entrypoint_path: checkout.join("sdk/streamlib-deno/mod.ts"),
            linked_at: "2026-01-01T00:00:00Z".into(),
            linked_crate_count: 1,
            state: LinkTransactionState::Active,
            files: Vec::new(),
        };
        std::fs::write(
            dir.join(LINK_MANIFEST_FILE),
            serde_json::to_string(&manifest).unwrap(),
        )
        .unwrap();
    }

    /// Run `f` with `STREAMLIB_LINK_CHECKOUT` forced to `value` (`None` ⇒
    /// unset), restoring the prior value afterward. Restores BEFORE returning
    /// the computed value so a later failed assert can't leak the env.
    fn with_link_checkout_env<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        // SAFETY: every caller is `#[serial]`, so the env-mutating tests are
        // mutually exclusive and no other thread reads the env concurrently.
        let prev = std::env::var_os(crate::LINK_CHECKOUT_ENV);
        unsafe {
            match value {
                Some(v) => std::env::set_var(crate::LINK_CHECKOUT_ENV, v),
                None => std::env::remove_var(crate::LINK_CHECKOUT_ENV),
            }
        }
        let out = f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var(crate::LINK_CHECKOUT_ENV, v),
                None => std::env::remove_var(crate::LINK_CHECKOUT_ENV),
            }
        }
        out
    }

    /// Marker-first discovery: with `STREAMLIB_LINK_CHECKOUT` unset, a
    /// `.streamlib/link.json` up-tree of the manifest dir supplies the checkout
    /// — the direct-`cargo build` fix (no env to export by hand).
    #[test]
    #[serial_test::serial]
    fn from_env_or_marker_discovers_marker_when_env_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("streamlib-checkout");
        write_link_marker(tmp.path(), &checkout);
        let nested = tmp.path().join("app").join("crate-dir");
        std::fs::create_dir_all(&nested).unwrap();

        let got = with_link_checkout_env(None, || {
            ResolverOptions::from_env_or_marker(&nested).link_checkout
        });
        assert_eq!(
            got,
            Some(checkout),
            "an up-tree marker must supply the checkout when the env is unset"
        );
    }

    /// Explicit env overrides the marker: a set (non-empty) env wins, so a dev
    /// who exports `STREAMLIB_LINK_CHECKOUT` (or the orchestrator threading the
    /// discovered checkout for a relocated build) is authoritative.
    #[test]
    #[serial_test::serial]
    fn from_env_or_marker_env_overrides_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let marker_checkout = tmp.path().join("marker-checkout");
        write_link_marker(tmp.path(), &marker_checkout);
        let nested = tmp.path().join("app");
        std::fs::create_dir_all(&nested).unwrap();

        let got = with_link_checkout_env(Some("/explicit/override"), || {
            ResolverOptions::from_env_or_marker(&nested).link_checkout
        });
        assert_eq!(
            got,
            Some(PathBuf::from("/explicit/override")),
            "a non-empty env must win over the marker"
        );
        assert_ne!(
            got,
            Some(marker_checkout),
            "the override must not defer to the marker's checkout"
        );
    }

    /// THE suppression boundary. The orchestrator sets the env to EMPTY for a
    /// relocated / locked / distribution build. Even with a stray
    /// `.streamlib/link.json` up-tree (the streamlib-home cache the orchestrator
    /// relocates into shares the `.streamlib` dir name with the link-state dir),
    /// an empty env must NOT redirect to the checkout. Mentally-revert the
    /// empty-suppression branch — fall through to marker discovery — and this
    /// asserts `Some(checkout)` and fails, proving the fix did not reintroduce
    /// the mixed-build hazard.
    #[test]
    #[serial_test::serial]
    fn from_env_or_marker_empty_env_suppresses_stray_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("stray-checkout");
        write_link_marker(tmp.path(), &checkout);
        // Mimic the relocated-build crate dir under a streamlib-home cache whose
        // own `.streamlib` dir sits beneath the marker's `.streamlib`.
        let relocated = tmp
            .path()
            .join("home")
            .join(".streamlib")
            .join("cache")
            .join("packages")
            .join("pkg-1.0.0");
        std::fs::create_dir_all(&relocated).unwrap();
        // Precondition: the marker IS discoverable up-tree, so suppression (not
        // absence) is what yields `None`.
        assert!(
            crate::link_marker::find_active_link_marker(&relocated).is_some(),
            "test setup: a marker must be present up-tree"
        );

        let got = with_link_checkout_env(Some(""), || {
            ResolverOptions::from_env_or_marker(&relocated).link_checkout
        });
        assert_eq!(
            got, None,
            "empty env is the suppression sentinel — a relocated/locked/distribution \
             build must not redirect to a checkout even with a marker up-tree"
        );
    }

    /// Neither channel active: no marker up-tree and no env ⇒ no redirect. The
    /// dominant non-linked build stays byte-identical to before.
    #[test]
    #[serial_test::serial]
    fn from_env_or_marker_no_marker_no_env_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("app");
        std::fs::create_dir_all(&nested).unwrap();

        let got = with_link_checkout_env(None, || {
            ResolverOptions::from_env_or_marker(&nested).link_checkout
        });
        assert_eq!(
            got, None,
            "no marker and no env must leave link_checkout unset"
        );
    }

    /// End-to-end: marker discovery → `from_env_or_marker` → `resolve_with`
    /// resolves a by-version dep from the linked checkout's `packages/` tree
    /// with NO env exported — the full direct-`cargo build` path the fix
    /// delivers (mirrors the codegen boundary `build.rs` drives). Mentally-revert
    /// marker discovery (`link_checkout_from_marker` returns `None`) and the dep
    /// falls through to the by-version path with no registry configured, so the
    /// resolve errors and `.expect` panics — the redirect is locked.
    #[test]
    #[serial_test::serial]
    fn marker_first_resolves_dep_from_linked_checkout_without_env() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("checkout");
        write_checkout_package(&checkout, "core", "core", "1.0.0");

        // The app root carries the marker; a nested crate dir builds under it.
        let app_root = tmp.path().join("app");
        write_link_marker(&app_root, &checkout);
        let crate_dir = app_root.join("crates").join("app-lib");
        write_streamlib_yaml(&crate_dir, "dependencies:\n  \"@tatolab/core\": \"^1.0.0\"\n");

        // Clear the registry env too so the mentally-revert is deterministic:
        // with marker discovery reverted, the bare `^1.0.0` dep would fall
        // through to by-version resolution and — with no registry — fail
        // `RegistryNotConfigured`, so `.expect` panics. Without this, a stray
        // `STREAMLIB_REGISTRY_URL` in the shell could send the reverted path to a
        // live fetch instead. The positive assertion (Path from the checkout)
        // holds regardless, since the link short-circuit precedes the registry.
        // SAFETY: `#[serial]` + `with_link_checkout_env` serialize env mutation.
        let prev_registry = std::env::var_os(crate::REGISTRY_URL_ENV);
        unsafe { std::env::remove_var(crate::REGISTRY_URL_ENV) };
        let res = with_link_checkout_env(None, || {
            let opts = ResolverOptions::from_env_or_marker(&crate_dir);
            resolve_with(&crate_dir, &opts)
        });
        unsafe {
            if let Some(v) = prev_registry {
                std::env::set_var(crate::REGISTRY_URL_ENV, v);
            }
        }
        let res = res.expect("marker-discovered checkout must resolve the by-version dep");

        let core = res.packages.get("@tatolab/core").unwrap();
        assert!(
            matches!(core.source, ResolvedSource::Path { .. }),
            "core must resolve from the linked checkout, got {:?}",
            core.source
        );
        assert!(
            core.root_dir.starts_with(&checkout),
            "core.root_dir must be under the checkout, got {}",
            core.root_dir.display()
        );
        assert_eq!(
            core.manifest.package.as_ref().unwrap().version.to_string(),
            "1.0.0"
        );
    }
}
