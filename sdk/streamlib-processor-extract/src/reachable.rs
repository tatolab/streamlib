// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Module-reachability resolution for the processor source-scan.
//!
//! [`crate::extract_rust_processors`] visits every `.rs` under `processors/`,
//! including platform arms a given host does not compile (`linux/` vs `apple/`)
//! and parked directories (`_apple_impl_pending_/`). That raw scan
//! over-collects: two platform arms declaring the same processor both surface,
//! and a parked module surfaces a `#[processor(...)]` that never compiles on any
//! target. Before extraction can replace the hand-authored `processors:` as the
//! authoritative truth-source — and before a drift check between the two can be
//! a hard `pkg publish` error without false positives on cfg-gated packages — the
//! scan must resolve to the set of modules the build **target** actually
//! compiles.
//!
//! [`extract_reachable_rust_processors`] does that. The crate root is generated
//! (see [`crate::crate_root`]), so the walk does not read it: it enumerates the
//! top-level arms under `processors/` exactly the way the generator declares
//! them, then follows each `mod` the way `rustc` does (honoring
//! `#[path = "..."]`), evaluates the `#[cfg(...)]` predicate on every `mod` and
//! every `#[processor(...)]`-bearing struct against a
//! [`ModuleReachabilityTarget`], and collects only the processors that survive.
//! A module file's own inner `#![cfg(...)]` gates it the same way, pruning the
//! whole subtree it declares.
//!
//! The parked-directory convention needs no special case: a parked module
//! declares `#![cfg(any())]` (an always-false predicate), so cfg evaluation
//! skips it exactly as `rustc` does — one rule, not a hard-coded directory name.
//!
//! [`extract_processors_across_every_build_target`] runs the same walk with cfg
//! pruning turned off, recording each predicate it passed through instead. That
//! is what lets the crate-root generator mirror the author's `#[cfg]` onto the
//! `export_plugin!` entry verbatim rather than re-deriving a platform rule.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use quote::ToTokens;
use syn::punctuated::Punctuated;
use syn::{Meta, Token};

use streamlib_idents::PACKAGE_PROCESSOR_SOURCE_DIR_NAME as PROCESSOR_SOURCE_DIR_NAME;

use crate::{ExtractError, ExtractedProcessor, parse_processor_attr, processor_attr};

/// The `#[cfg(...)]` evaluation environment a module-reachability walk resolves
/// against — the set of cfg atoms the build **target** defines.
///
/// A `#[cfg(target_os = "linux")]` module is reachable iff `("target_os",
/// "linux")` is in [`ModuleReachabilityTarget::key_values`]; a `#[cfg(unix)]`
/// module iff `"unix"` is in [`ModuleReachabilityTarget::flags`]. An atom the
/// target does not define evaluates to `false` — the same way `rustc` treats an
/// unset cfg — so a cross-target platform arm and a `#[cfg(any())]` parked
/// module are both excluded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleReachabilityTarget {
    /// Key/value cfg atoms the target defines, e.g. `("target_os", "linux")`,
    /// `("target_arch", "x86_64")`, `("feature", "cuda")`.
    pub key_values: BTreeSet<(String, String)>,
    /// Bare flag cfg atoms the target defines, e.g. `"unix"`, `"windows"`.
    pub flags: BTreeSet<String>,
}

impl ModuleReachabilityTarget {
    /// An empty target defining no cfg atoms — every `#[cfg(...)]`-gated module
    /// is excluded, only unconditional modules are reachable.
    pub fn new() -> Self {
        Self::default()
    }

    /// The cfg atoms for the **host** the extractor is running on, derived from
    /// [`std::env::consts`]: `target_os`, `target_arch`, `target_family`, and
    /// the `unix` / `windows` family flag. This is the target `streamlib
    /// pkg publish` extracts for — the package is built for the invoking host's
    /// triple, so the reachable processor set is the set that host compiles.
    ///
    /// Cargo features are NOT inferred here (the extractor cannot know which
    /// features a downstream build enables); add each enabled feature with
    /// [`ModuleReachabilityTarget::with_feature`].
    pub fn for_host() -> Self {
        let os = std::env::consts::OS; // "linux" / "macos" / "windows"
        let arch = std::env::consts::ARCH; // "x86_64" / "aarch64" / …
        let family = std::env::consts::FAMILY; // "unix" / "windows"
        Self::new()
            .with_key_value("target_os", os)
            .with_key_value("target_arch", arch)
            .with_key_value("target_family", family)
            .with_flag(family)
    }

    /// Add a key/value cfg atom (e.g. `("target_os", "linux")`).
    pub fn with_key_value(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.key_values.insert((key.into(), value.into()));
        self
    }

    /// Add a bare flag cfg atom (e.g. `unix`).
    pub fn with_flag(mut self, flag: impl Into<String>) -> Self {
        self.flags.insert(flag.into());
        self
    }

    /// Add an enabled cargo feature (`#[cfg(feature = "<name>")]`).
    pub fn with_feature(self, name: impl Into<String>) -> Self {
        self.with_key_value("feature", name)
    }

    /// Whether `("key", "value")` is defined by this target.
    fn has_key_value(&self, key: &str, value: &str) -> bool {
        self.key_values
            .contains(&(key.to_string(), value.to_string()))
    }

    /// Whether the bare flag `name` is defined by this target.
    fn has_flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
}

/// One top-level module arm under a package's `processors/` directory — the
/// unit the generated crate root declares with a `#[path]`-attributed
/// `pub mod`, and the unit the reachability walk starts each descent from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorSourceModuleArm {
    /// The module name the generated crate root declares — the file stem for a
    /// flat arm, the directory name for a `mod.rs`-backed one.
    pub module_name: String,
    /// Absolute path to the arm's own module file.
    pub module_file: PathBuf,
    /// The `#[path = "..."]` value the generated crate root carries, relative
    /// to the directory holding the generated crate root file. Always
    /// forward-slashed — `#[path]` is a source literal, not a host path.
    pub crate_root_relative_module_path: String,
    /// The file-level `#![cfg(...)]` predicates the arm's own file declares, in
    /// source order and as written. The generator mirrors these verbatim onto
    /// the `pub mod` and onto every `export_plugin!` entry the arm contributes.
    pub file_level_cfg_predicates: Vec<String>,
}

impl ProcessorSourceModuleArm {
    /// The directory this arm's own `mod <name>;` children resolve against.
    /// The generated declaration always carries `#[path]`, and `rustc` gives a
    /// `#[path]` module the directory holding its file — so a `mod.rs` arm owns
    /// its directory and a flat arm's children are its file's SIBLINGS.
    fn child_module_search_dir(&self) -> PathBuf {
        self.module_file
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf()
    }
}

/// Enumerate the top-level module arms under `<package_dir>/processors`.
///
/// A `processors/<name>.rs` is a flat arm; a `processors/<name>/mod.rs` is a
/// directory arm. A directory without a `mod.rs` and a file with any other
/// extension are skipped rather than refused: `processors/` is the shared
/// discovery root for every language, so a Rust package in a polyglot package
/// legitimately sits beside `.py` / `.ts` modules and their directories.
///
/// A missing `processors/` yields an empty list — a schema-only Rust package is
/// first-class. Arms are returned sorted by module name so generation and
/// extraction are deterministic.
#[tracing::instrument(skip_all, fields(package_dir = %package_dir.display()))]
pub fn enumerate_processor_source_module_arms(
    package_dir: &Path,
) -> Result<Vec<ProcessorSourceModuleArm>, ExtractError> {
    let processor_source_dir = package_dir.join(PROCESSOR_SOURCE_DIR_NAME);
    if !processor_source_dir.is_dir() {
        return Ok(Vec::new());
    }

    let entries = std::fs::read_dir(&processor_source_dir).map_err(|e| ExtractError::Io {
        path: processor_source_dir.clone(),
        source: e,
    })?;

    let mut arms = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| ExtractError::Io {
            path: processor_source_dir.clone(),
            source: e,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| ExtractError::Io {
            path: path.clone(),
            source: e,
        })?;

        let (module_name, module_file, relative_module_path) = if file_type.is_dir() {
            let mod_rs = path.join("mod.rs");
            if !mod_rs.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            (
                name.to_string(),
                mod_rs,
                format!("../{PROCESSOR_SOURCE_DIR_NAME}/{name}/mod.rs"),
            )
        } else {
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            (
                stem.to_string(),
                path.clone(),
                format!("../{PROCESSOR_SOURCE_DIR_NAME}/{stem}.rs"),
            )
        };

        let file_level_cfg_predicates = read_file_level_cfg_predicates(&module_file)?;
        arms.push(ProcessorSourceModuleArm {
            module_name,
            module_file,
            crate_root_relative_module_path: relative_module_path,
            file_level_cfg_predicates,
        });
    }

    arms.sort_by(|a, b| a.module_name.cmp(&b.module_name));
    tracing::debug!(arms = arms.len(), "enumerated processor source arms");
    Ok(arms)
}

/// The file-level inner `#![cfg(...)]` predicates a module file declares on
/// itself, as written.
fn read_file_level_cfg_predicates(file: &Path) -> Result<Vec<String>, ExtractError> {
    let body = std::fs::read_to_string(file).map_err(|e| ExtractError::Io {
        path: file.to_path_buf(),
        source: e,
    })?;
    let parsed = syn::parse_file(&body).map_err(|e| ExtractError::Syntax {
        path: file.to_path_buf(),
        source: e,
    })?;
    Ok(cfg_predicate_sources(&parsed.attrs))
}

/// The source text of every `#[cfg(<predicate>)]` on an item, in order. A
/// predicate the parser cannot read is rendered as its raw token text so the
/// generator still mirrors it rather than dropping it.
fn cfg_predicate_sources(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .map(|attr| match attr.parse_args::<Meta>() {
            Ok(meta) => render_cfg_predicate(&meta),
            Err(_) => match &attr.meta {
                Meta::List(list) => list.tokens.to_token_stream().to_string(),
                other => other.to_token_stream().to_string(),
            },
        })
        .collect()
}

/// Render a parsed cfg predicate back to source text. `TokenStream::to_string`
/// alone would emit `any (a , b)` — legal but unreadable in a generated crate
/// root a human is expected to open when a build breaks.
fn render_cfg_predicate(meta: &Meta) -> String {
    match meta {
        Meta::Path(path) => path.to_token_stream().to_string(),
        Meta::NameValue(name_value) => format!(
            "{} = {}",
            name_value.path.to_token_stream(),
            name_value.value.to_token_stream()
        ),
        Meta::List(list) => {
            let combinator = list.path.to_token_stream().to_string();
            match list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) {
                Ok(inner) => {
                    let rendered: Vec<String> = inner.iter().map(render_cfg_predicate).collect();
                    format!("{combinator}({})", rendered.join(", "))
                }
                Err(_) => format!("{combinator}({})", list.tokens.to_token_stream()),
            }
        }
    }
}

/// How a module walk treats the `#[cfg(...)]` predicates it meets.
enum ModuleWalkCfgResolution<'walk> {
    /// Prune every module / struct the build target does not compile. The
    /// collected processors carry no predicates — they are unconditionally live
    /// for this target.
    AgainstBuildTarget(&'walk ModuleReachabilityTarget),
    /// Keep every arm that is satisfiable on SOME target, recording the
    /// predicates in force so a generator can mirror them verbatim. A
    /// statically-unsatisfiable predicate (`any()` with no operands — the
    /// parked-module convention) is still pruned: it compiles nowhere, so no
    /// generated arm should name it.
    AcrossEveryBuildTarget,
}

/// Derive the `processors:` manifest section from the modules a Rust package
/// compiles **for `target`** — the reachability-resolved counterpart to
/// [`crate::extract_rust_processors`].
///
/// Starts at each top-level arm under `processors/`, follows every reachable
/// `mod` the way `rustc` resolves module files, and evaluates `#[cfg(...)]` on
/// each `mod` and each `#[processor(...)]`-bearing struct against `target`, plus
/// the inner `#![cfg(...)]` a module file declares on itself. A `#[processor]`
/// under a cfg-excluded module — a cross-platform arm, a disabled feature, or a
/// `#[cfg(any())]` parked directory — is never collected. The result is
/// deterministic (source order within a file, arm order across files) and
/// de-duplicated by resolved source file.
#[tracing::instrument(skip_all, fields(package_dir = %package_dir.display()))]
pub fn extract_reachable_rust_processors(
    package_dir: &Path,
    target: &ModuleReachabilityTarget,
) -> Result<Vec<ExtractedProcessor>, ExtractError> {
    walk_processor_source_arms(
        package_dir,
        ModuleWalkCfgResolution::AgainstBuildTarget(target),
    )
}

/// Every `#[processor(...)]` a Rust package declares on ANY target, each
/// carrying the `#[cfg(...)]` predicates that gate it.
///
/// This is the crate-root generator's input: it needs the union across targets
/// (a Linux host still generates the macOS arm's `export_plugin!` entry, gated)
/// together with the author's predicates, mirrored verbatim so the generated
/// root never re-derives a platform rule. Parked arms — anything gated by a
/// statically-unsatisfiable predicate such as `#[cfg(any())]` — are excluded:
/// they compile on no target, so naming them in a generated arm would only
/// produce a declaration entry that is always stripped.
#[tracing::instrument(skip_all, fields(package_dir = %package_dir.display()))]
pub fn extract_processors_across_every_build_target(
    package_dir: &Path,
) -> Result<Vec<ExtractedProcessor>, ExtractError> {
    walk_processor_source_arms(package_dir, ModuleWalkCfgResolution::AcrossEveryBuildTarget)
}

fn walk_processor_source_arms(
    package_dir: &Path,
    cfg_resolution: ModuleWalkCfgResolution<'_>,
) -> Result<Vec<ExtractedProcessor>, ExtractError> {
    let arms = enumerate_processor_source_module_arms(package_dir)?;

    let mut walker = ReachableModuleWalker {
        package_dir,
        cfg_resolution,
        visited: BTreeSet::new(),
        active_cfg_predicates: Vec::new(),
        module_path_segments: Vec::new(),
        out: Vec::new(),
    };

    for arm in &arms {
        walker.module_path_segments.push(arm.module_name.clone());
        let child_module_search_dir = arm.child_module_search_dir();
        walker.walk_file(&arm.module_file, &child_module_search_dir)?;
        walker.module_path_segments.pop();
    }

    tracing::debug!(processors = walker.out.len(), "extracted (reachable)");
    Ok(walker.out)
}

/// Carries the walk state so the recursive descent doesn't thread six
/// parameters through every call.
struct ReachableModuleWalker<'walk> {
    package_dir: &'walk Path,
    cfg_resolution: ModuleWalkCfgResolution<'walk>,
    visited: BTreeSet<PathBuf>,
    /// `#[cfg]` predicates passed through to reach the current item, outermost
    /// first. Only recorded under [`ModuleWalkCfgResolution::AcrossEveryBuildTarget`].
    active_cfg_predicates: Vec<String>,
    /// `mod` segments from the crate root to the current module.
    module_path_segments: Vec<String>,
    out: Vec<ExtractedProcessor>,
}

impl ReachableModuleWalker<'_> {
    /// Parse `file` and process its items. `mod_dir` is the directory that this
    /// file's `mod <name>;` children resolve against: the arm's own directory
    /// for a `#[path]`-declared arm, `<parent>/foo/` for a plain `mod foo;`
    /// that resolved to `foo.rs`.
    fn walk_file(&mut self, file: &Path, mod_dir: &Path) -> Result<(), ExtractError> {
        // A module file is reachable via exactly one `mod` path in valid Rust,
        // but guard against re-processing (and any pathological `#[path]` alias).
        let canonical = file.to_path_buf();
        if !self.visited.insert(canonical) {
            return Ok(());
        }

        let body = std::fs::read_to_string(file).map_err(|e| ExtractError::Io {
            path: file.to_path_buf(),
            source: e,
        })?;
        let parsed = syn::parse_file(&body).map_err(|e| ExtractError::Syntax {
            path: file.to_path_buf(),
            source: e,
        })?;

        // A file-level inner `#![cfg(...)]` gates the module the file *is*, so a
        // false predicate strips the file and everything it declares — including
        // its `mod` children. `syn` keeps these on `File::attrs`; an inline
        // `mod foo { #![cfg] }` instead folds them into `ItemMod::attrs`, which
        // `walk_item` gates.
        let Some(pushed_predicate_count) = self.enter_cfg_scope(&parsed.attrs) else {
            tracing::trace!(file = %file.display(), "module file excluded by file-level cfg");
            return Ok(());
        };

        let rel = file
            .strip_prefix(self.package_dir)
            .unwrap_or(file)
            .to_path_buf();
        // `#[path]` on a non-inline `mod` resolves relative to the directory
        // holding the DECLARING file, which differs from `mod_dir` for a
        // non-`mod.rs` file (`src/a/b.rs` declares against `src/a/`, while its
        // plain `mod` children resolve against `src/a/b/`).
        let path_attribute_base_dir = file.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        for item in &parsed.items {
            self.walk_item(item, file, mod_dir, &path_attribute_base_dir, &rel)?;
        }
        self.leave_cfg_scope(pushed_predicate_count);
        Ok(())
    }

    /// Process one item: collect a reachable `#[processor(...)]` struct, or
    /// descend into a cfg-reachable module (inline or external).
    fn walk_item(
        &mut self,
        item: &syn::Item,
        declaring_file: &Path,
        mod_dir: &Path,
        path_attribute_base_dir: &Path,
        rel_path: &Path,
    ) -> Result<(), ExtractError> {
        match item {
            syn::Item::Struct(item_struct) => {
                // A struct behind a false `#[cfg(...)]` is not compiled, so its
                // `#[processor]` (if any) is not a real processor for this target.
                let Some(pushed_predicate_count) = self.enter_cfg_scope(&item_struct.attrs) else {
                    return Ok(());
                };
                if let Some(attr) = processor_attr(&item_struct.attrs) {
                    let mut extracted = parse_processor_attr(attr, &item_struct.ident, rel_path)?;
                    extracted
                        .module_path_segments
                        .clone_from(&self.module_path_segments);
                    extracted
                        .cfg_predicates
                        .clone_from(&self.active_cfg_predicates);
                    self.out.push(extracted);
                }
                self.leave_cfg_scope(pushed_predicate_count);
            }
            syn::Item::Mod(item_mod) => {
                let Some(pushed_predicate_count) = self.enter_cfg_scope(&item_mod.attrs) else {
                    return Ok(());
                };
                self.module_path_segments.push(item_mod.ident.to_string());
                match &item_mod.content {
                    // Inline `mod foo { ... }` introduces a `foo` directory
                    // component for its children's file resolution — including
                    // for a `#[path]` on a child, which is why the base dir
                    // tracks the inline components from here down.
                    Some((_, items)) => {
                        let inner_dir = mod_dir.join(item_mod.ident.to_string());
                        for inner in items {
                            self.walk_item(
                                inner,
                                declaring_file,
                                &inner_dir,
                                &inner_dir,
                                rel_path,
                            )?;
                        }
                    }
                    // External `mod foo;` resolves to a sibling file.
                    None => {
                        let resolved = self.resolve_module_file(
                            item_mod,
                            declaring_file,
                            mod_dir,
                            path_attribute_base_dir,
                        )?;
                        let child_mod_dir =
                            child_module_search_dir(&resolved, &item_mod.ident.to_string());
                        self.walk_file(&resolved.module_file, &child_mod_dir)?;
                    }
                }
                self.module_path_segments.pop();
                self.leave_cfg_scope(pushed_predicate_count);
            }
            _ => {}
        }
        Ok(())
    }

    /// Resolve an external `mod <name>;` to its source file, honoring a
    /// `#[path = "..."]` override and otherwise the standard
    /// `<mod_dir>/<name>.rs` then `<mod_dir>/<name>/mod.rs` search.
    ///
    /// `rustc` resolves a `#[path]` on a non-inline module against the directory
    /// holding the declaring file (`path_attribute_base_dir`), which is NOT the
    /// directory that module's plain `mod` children resolve against.
    fn resolve_module_file(
        &self,
        item_mod: &syn::ItemMod,
        declaring_file: &Path,
        mod_dir: &Path,
        path_attribute_base_dir: &Path,
    ) -> Result<ResolvedChildModule, ExtractError> {
        let name = item_mod.ident.to_string();

        if let Some(path_attr) = path_override(&item_mod.attrs) {
            let candidate = path_attribute_base_dir.join(&path_attr);
            if candidate.is_file() {
                return Ok(ResolvedChildModule {
                    module_file: candidate,
                    resolved_via_path_attribute: true,
                });
            }
            return Err(ExtractError::UnresolvedModule {
                module: name,
                declared_in: declaring_file.to_path_buf(),
                candidates: candidate.display().to_string(),
            });
        }

        let flat = mod_dir.join(format!("{name}.rs"));
        if flat.is_file() {
            return Ok(ResolvedChildModule {
                module_file: flat,
                resolved_via_path_attribute: false,
            });
        }
        let nested = mod_dir.join(&name).join("mod.rs");
        if nested.is_file() {
            return Ok(ResolvedChildModule {
                module_file: nested,
                resolved_via_path_attribute: false,
            });
        }
        Err(ExtractError::UnresolvedModule {
            module: name,
            declared_in: declaring_file.to_path_buf(),
            candidates: format!("{} or {}", flat.display(), nested.display()),
        })
    }

    /// Enter the cfg scope an item's attributes open. Returns `None` when the
    /// item is not compiled (so the caller skips it), otherwise the number of
    /// predicates pushed, to be handed back to [`Self::leave_cfg_scope`].
    fn enter_cfg_scope(&mut self, attrs: &[syn::Attribute]) -> Option<usize> {
        match self.cfg_resolution {
            ModuleWalkCfgResolution::AgainstBuildTarget(target) => attrs
                .iter()
                .filter(|attr| attr.path().is_ident("cfg"))
                .all(|attr| eval_cfg_attr(attr, target))
                .then_some(0),
            ModuleWalkCfgResolution::AcrossEveryBuildTarget => {
                let cfg_attrs: Vec<&syn::Attribute> = attrs
                    .iter()
                    .filter(|attr| attr.path().is_ident("cfg"))
                    .collect();
                if cfg_attrs.iter().any(|attr| {
                    attr.parse_args::<Meta>()
                        .is_ok_and(|meta| cfg_predicate_is_statically_unsatisfiable(&meta))
                }) {
                    return None;
                }
                let predicates = cfg_predicate_sources(attrs);
                let pushed = predicates.len();
                self.active_cfg_predicates.extend(predicates);
                Some(pushed)
            }
        }
    }

    fn leave_cfg_scope(&mut self, pushed_predicate_count: usize) {
        self.active_cfg_predicates
            .truncate(self.active_cfg_predicates.len() - pushed_predicate_count);
    }
}

/// A child `mod` resolved to its file, plus how it was resolved — `rustc` gives
/// a `#[path]` module the directory holding its file, with no extra component,
/// which is a different rule from the plain `foo.rs` / `foo/mod.rs` search.
struct ResolvedChildModule {
    module_file: PathBuf,
    resolved_via_path_attribute: bool,
}

/// Evaluate a single `#[cfg(<predicate>)]`. A malformed predicate the parser
/// can't read is treated as unreachable (conservative: never over-collect a
/// processor from a cfg we could not prove true).
fn eval_cfg_attr(attr: &syn::Attribute, target: &ModuleReachabilityTarget) -> bool {
    match attr.parse_args::<Meta>() {
        Ok(meta) => eval_cfg_meta(&meta, target),
        Err(_) => false,
    }
}

/// Evaluate a cfg predicate meta: `all(..)` / `any(..)` / `not(..)`
/// combinators, `key = "value"` atoms, and bare flag atoms.
fn eval_cfg_meta(meta: &Meta, target: &ModuleReachabilityTarget) -> bool {
    match meta {
        Meta::Path(path) => path
            .get_ident()
            .is_some_and(|ident| target.has_flag(&ident.to_string())),
        Meta::NameValue(name_value) => {
            let Some(key) = name_value.path.get_ident().map(|i| i.to_string()) else {
                return false;
            };
            match literal_str(&name_value.value) {
                Some(value) => target.has_key_value(&key, &value),
                None => false,
            }
        }
        Meta::List(list) => {
            let combinator = match list.path.get_ident() {
                Some(ident) => ident.to_string(),
                None => return false,
            };
            let Ok(inner) = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
            else {
                return false;
            };
            match combinator.as_str() {
                // `all()` is vacuously true; `any()` is vacuously false —
                // which is exactly why `#[cfg(any())]` parks a module.
                "all" => inner.iter().all(|m| eval_cfg_meta(m, target)),
                "any" => inner.iter().any(|m| eval_cfg_meta(m, target)),
                "not" => inner.len() == 1 && !eval_cfg_meta(&inner[0], target),
                _ => false,
            }
        }
    }
}

/// Whether a cfg predicate is false on EVERY target, structurally — the parked
/// convention `#[cfg(any())]` and anything that folds down to it. An atom is
/// satisfiable by definition (some target defines it), so only the vacuous
/// `any()` and combinations built from it fold to unsatisfiable.
fn cfg_predicate_is_statically_unsatisfiable(meta: &Meta) -> bool {
    let Meta::List(list) = meta else {
        return false;
    };
    let Some(combinator) = list.path.get_ident().map(|i| i.to_string()) else {
        return false;
    };
    let Ok(inner) = list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated) else {
        return false;
    };
    match combinator.as_str() {
        "any" => inner.iter().all(cfg_predicate_is_statically_unsatisfiable),
        "all" => inner.iter().any(cfg_predicate_is_statically_unsatisfiable),
        _ => false,
    }
}

/// The `#[path = "..."]` override on a `mod`, if present.
fn path_override(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("path") {
            return None;
        }
        match &attr.meta {
            Meta::NameValue(name_value) => literal_str(&name_value.value),
            _ => None,
        }
    })
}

/// The string value of an expression literal (`"linux"`), if it is one.
fn literal_str(expr: &syn::Expr) -> Option<String> {
    if let syn::Expr::Lit(expr_lit) = expr
        && let syn::Lit::Str(lit_str) = &expr_lit.lit
    {
        return Some(lit_str.value());
    }
    None
}

/// The directory a child module file's own `mod <name>;` children resolve
/// against, matching `rustc`:
///
/// - resolved through `#[path]` — the directory holding the file, with no
///   component introduced, so a `#[path = ".../blur.rs"]` module's children are
///   that file's SIBLINGS while a `#[path = ".../linux/mod.rs"]` module keeps
///   its own directory;
/// - resolved by the standard search — `.../foo/mod.rs` keeps `.../foo`, and
///   `.../foo.rs` introduces the `.../foo` directory component.
fn child_module_search_dir(resolved: &ResolvedChildModule, mod_name: &str) -> PathBuf {
    let parent = resolved
        .module_file
        .parent()
        .unwrap_or_else(|| Path::new(""));
    if resolved.resolved_via_path_attribute {
        return parent.to_path_buf();
    }
    match resolved.module_file.file_name().and_then(|n| n.to_str()) {
        Some("mod.rs") => parent.to_path_buf(),
        _ => parent.join(mod_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn linux() -> ModuleReachabilityTarget {
        ModuleReachabilityTarget::new()
            .with_key_value("target_os", "linux")
            .with_key_value("target_family", "unix")
            .with_flag("unix")
    }

    fn macos() -> ModuleReachabilityTarget {
        ModuleReachabilityTarget::new()
            .with_key_value("target_os", "macos")
            .with_key_value("target_family", "unix")
            .with_flag("unix")
    }

    fn names(mut procs: Vec<ExtractedProcessor>) -> Vec<String> {
        procs.sort_by(|a, b| a.schema.name.cmp(&b.schema.name));
        procs.into_iter().map(|p| p.schema.name).collect()
    }

    /// The parked-directory convention (`#![cfg(any())]` in
    /// `processors/_apple_impl_pending_/mod.rs`) falls out of cfg evaluation for
    /// free: `any()` is vacuously false, so the parked subtree is never walked
    /// and its `#[processor]` never collected. Mentally revert `eval_cfg_meta`'s
    /// `any` arm to `true` and this fails.
    #[test]
    fn parked_cfg_any_arm_is_excluded() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux_impl.rs",
            r#"#![cfg(target_os = "linux")]

            #[processor("@tatolab/demo/Camera", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct Camera;
            "#,
        );
        write(
            root,
            "processors/_apple_impl_pending_/mod.rs",
            r#"#![cfg(any())]

            #[processor("@tatolab/demo/AppleCamera", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct AppleCamera;
            "#,
        );

        let procs = extract_reachable_rust_processors(root, &linux()).unwrap();
        assert_eq!(names(procs), vec!["Camera"]);
    }

    /// A parked arm is invisible to the across-every-target scan too: it
    /// compiles nowhere, so the generated crate root must not name it in an
    /// `export_plugin!` entry that would only ever be stripped.
    #[test]
    fn parked_cfg_any_arm_is_excluded_across_every_build_target() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/_apple_impl_pending_/mod.rs",
            r#"#![cfg(any())]

            #[processor("@tatolab/demo/AppleCamera", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct AppleCamera;
            "#,
        );
        write(
            root,
            "processors/live.rs",
            r#"#[processor("@tatolab/demo/Live", execution = reactive)]
            pub struct Live;"#,
        );

        let procs = extract_processors_across_every_build_target(root).unwrap();
        assert_eq!(names(procs), vec!["Live"]);
    }

    /// Two platform arms declaring the same processor: only the arm the target
    /// compiles surfaces (the raw scan would surface both).
    #[test]
    fn cross_platform_arms_resolve_to_the_target_arm() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/LinuxCam", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct LinuxCam;"#,
        );
        write(
            root,
            "processors/apple.rs",
            r#"#![cfg(target_os = "macos")]
            #[processor("@tatolab/demo/AppleCam", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct AppleCam;"#,
        );

        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["LinuxCam"]
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &macos()).unwrap()),
            vec!["AppleCam"]
        );
    }

    /// The across-every-target scan keeps both platform arms and records the
    /// author's predicate on each, verbatim.
    #[test]
    fn across_every_build_target_keeps_both_arms_with_their_predicates() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/linux.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/LinuxCam", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct LinuxCam;"#,
        );
        write(
            root,
            "processors/apple.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/demo/AppleCam", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct AppleCam;"#,
        );

        let procs = extract_processors_across_every_build_target(root).unwrap();
        let mut by_name: Vec<(String, Vec<String>, Vec<String>)> = procs
            .into_iter()
            .map(|p| (p.schema.name, p.cfg_predicates, p.module_path_segments))
            .collect();
        by_name.sort();
        assert_eq!(
            by_name,
            vec![
                (
                    "AppleCam".to_string(),
                    vec![r#"any(target_os = "macos", target_os = "ios")"#.to_string()],
                    vec!["apple".to_string()],
                ),
                (
                    "LinuxCam".to_string(),
                    vec![r#"target_os = "linux""#.to_string()],
                    vec!["linux".to_string()],
                ),
            ]
        );
    }

    /// Predicates nest: a struct-level `#[cfg]` under a file-level one records
    /// both, outermost first, so the generator can `all(...)` them.
    #[test]
    fn nested_cfg_predicates_accumulate_outermost_first() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/gated.rs",
            r#"#![cfg(unix)]

            #[cfg(feature = "cuda")]
            #[processor("@tatolab/demo/CudaOnly", execution = reactive)]
            pub struct CudaOnly;"#,
        );
        let procs = extract_processors_across_every_build_target(root).unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(
            procs[0].cfg_predicates,
            vec!["unix".to_string(), r#"feature = "cuda""#.to_string()]
        );
    }

    /// A `#[processor]` directly on a cfg-gated struct (no module boundary) is
    /// gated too.
    #[test]
    fn cfg_on_the_struct_itself_is_honored() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arms.rs",
            r#"
            #[cfg(target_os = "linux")]
            #[processor("@tatolab/demo/OnlyLinux", execution = reactive)]
            pub struct OnlyLinux;

            #[cfg(target_os = "windows")]
            #[processor("@tatolab/demo/OnlyWindows", execution = reactive)]
            pub struct OnlyWindows;
            "#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["OnlyLinux"]
        );
    }

    /// A file-level inner `#![cfg(...)]` on a nested module file gates the whole
    /// file, pruning the subtree it declares.
    #[test]
    fn file_level_inner_cfg_prunes_the_child_module_subtree() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/outer/mod.rs", "pub mod gated;\n");
        write(
            root,
            "processors/outer/gated.rs",
            r#"#![cfg(target_os = "linux")]

            pub mod child;"#,
        );
        write(
            root,
            "processors/outer/gated/child.rs",
            r#"#[processor("@tatolab/demo/DeepChild", execution = reactive)]
            pub struct DeepChild;"#,
        );

        assert!(
            extract_reachable_rust_processors(root, &macos())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["DeepChild"]
        );
    }

    /// File-level gates run through the same combinator evaluator as item-level
    /// ones: `all(...)`, `not(...)`, and `any(...)` resolve against the target.
    #[test]
    fn file_level_inner_cfg_evaluates_combinators() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/unix_not_linux.rs",
            r#"#![cfg(all(unix, not(target_os = "linux")))]

            #[processor("@tatolab/demo/UnixNotLinux", execution = reactive)]
            pub struct UnixNotLinux;"#,
        );
        write(
            root,
            "processors/unix_and_linux.rs",
            r#"#![cfg(all(unix, target_os = "linux"))]

            #[processor("@tatolab/demo/UnixAndLinux", execution = reactive)]
            pub struct UnixAndLinux;"#,
        );
        write(
            root,
            "processors/any_exotic.rs",
            r#"#![cfg(any(target_os = "windows", target_os = "redox"))]

            #[processor("@tatolab/demo/AnyExotic", execution = reactive)]
            pub struct AnyExotic;"#,
        );

        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["UnixAndLinux"]
        );
    }

    /// An inner `#![cfg]` inside an *inline* `mod foo { ... }` is folded into
    /// `ItemMod::attrs` by `syn`, so the `mod` arm of the walk already gates it.
    #[test]
    fn inline_mod_inner_cfg_is_honored() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arm.rs",
            r#"
            pub mod inline_linux {
                #![cfg(target_os = "linux")]

                #[processor("@tatolab/demo/InlineLinux", execution = reactive)]
                pub struct InlineLinux;
            }
            "#,
        );

        assert!(
            extract_reachable_rust_processors(root, &macos())
                .unwrap()
                .is_empty()
        );
        let procs = extract_reachable_rust_processors(root, &linux()).unwrap();
        assert_eq!(names(procs.clone()), vec!["InlineLinux"]);
        assert_eq!(
            procs[0].module_path_segments,
            vec!["arm".to_string(), "inline_linux".to_string()]
        );
    }

    /// The evaluator fails closed: a predicate whose tokens are not a `Meta` at
    /// all means "not reachable", never "reachable".
    #[test]
    fn file_level_inner_cfg_fails_closed_on_an_unparseable_predicate() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/unparseable.rs",
            r#"#![cfg(target_os =)]

            #[processor("@tatolab/demo/Unparseable", execution = reactive)]
            pub struct Unparseable;"#,
        );

        assert!(
            extract_reachable_rust_processors(root, &linux())
                .unwrap()
                .is_empty()
        );
    }

    /// The other fail-closed path: `target_os = 42` *is* a well-formed `Meta`,
    /// so `parse_args` succeeds and the atom lookup — not the parser — is what
    /// must refuse it, because a cfg value is always a string literal.
    #[test]
    fn file_level_inner_cfg_fails_closed_on_a_non_string_predicate_value() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/non_string_value.rs",
            r#"#![cfg(target_os = 42)]

            #[processor("@tatolab/demo/NonStringValue", execution = reactive)]
            pub struct NonStringValue;"#,
        );

        assert!(
            extract_reachable_rust_processors(root, &linux())
                .unwrap()
                .is_empty()
        );
    }

    /// A `.rs` file that is not `mod`-declared from an arm is unreachable — a
    /// scratch file nested under an arm's directory is not compiled and must not
    /// contribute a processor. The raw whole-tree scan would collect it.
    #[test]
    fn undeclared_file_under_an_arm_is_unreachable() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/wired/mod.rs", "pub mod declared;\n");
        write(
            root,
            "processors/wired/declared.rs",
            r#"#[processor("@tatolab/demo/Wired", execution = reactive)]
            pub struct Wired;"#,
        );
        write(
            root,
            "processors/wired/scratch.rs",
            r#"#[processor("@tatolab/demo/Scratch", execution = reactive)]
            pub struct Scratch;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["Wired"]
        );
    }

    /// `not(...)`, `all(...)`, and `any(...)` combinators evaluate against the
    /// target.
    #[test]
    fn cfg_combinators_evaluate() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arms.rs",
            r#"
            #[cfg(all(unix, target_arch = "x86_64"))]
            #[processor("@tatolab/demo/UnixX86", execution = reactive)]
            pub struct UnixX86;

            #[cfg(not(target_os = "windows"))]
            #[processor("@tatolab/demo/NotWindows", execution = reactive)]
            pub struct NotWindows;

            #[cfg(any(target_os = "windows", target_os = "redox"))]
            #[processor("@tatolab/demo/Exotic", execution = reactive)]
            pub struct Exotic;
            "#,
        );
        let target = linux().with_key_value("target_arch", "x86_64");
        assert_eq!(
            names(extract_reachable_rust_processors(root, &target).unwrap()),
            vec!["NotWindows", "UnixX86"]
        );
    }

    /// A `#[cfg(feature = "...")]` module is reachable only when the feature is
    /// declared on the target.
    #[test]
    fn feature_gated_module_needs_the_feature() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/cuda.rs",
            r#"#![cfg(feature = "cuda")]
            #[processor("@tatolab/demo/Cuda", execution = reactive)]
            pub struct Cuda;"#,
        );

        assert!(
            extract_reachable_rust_processors(root, &linux())
                .unwrap()
                .is_empty()
        );
        let with_cuda = linux().with_feature("cuda");
        assert_eq!(
            names(extract_reachable_rust_processors(root, &with_cuda).unwrap()),
            vec!["Cuda"]
        );
    }

    /// A `mod.rs`-backed arm keeps its own directory for its children, and a
    /// nested `foo.rs` introduces the `foo/` directory component for its own.
    #[test]
    fn nested_module_resolution_follows_rustc() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/devices/mod.rs", "pub mod webcam;\n");
        write(root, "processors/devices/webcam.rs", "pub mod sensor;\n");
        write(
            root,
            "processors/devices/webcam/sensor.rs",
            r#"#[processor("@tatolab/demo/Sensor", execution = reactive)]
            pub struct Sensor;"#,
        );
        let procs = extract_reachable_rust_processors(root, &linux()).unwrap();
        assert_eq!(names(procs.clone()), vec!["Sensor"]);
        assert_eq!(
            procs[0].module_path_segments,
            vec![
                "devices".to_string(),
                "webcam".to_string(),
                "sensor".to_string()
            ]
        );
    }

    /// `#[path]` on a `mod.rs`-shaped target keeps that file's own directory for
    /// the child's children — `rustc` gives a `#[path]` module the directory
    /// holding its file, and for a `mod.rs` that is the module's own directory.
    #[test]
    fn path_attribute_to_a_mod_rs_keeps_its_directory() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arm/mod.rs",
            "#[path = \"platform/linux/mod.rs\"]\npub mod linux;\n",
        );
        write(
            root,
            "processors/arm/platform/linux/mod.rs",
            "pub mod helper;\n",
        );
        write(
            root,
            "processors/arm/platform/linux/helper.rs",
            r#"#[processor("@tatolab/demo/PathModRs", execution = reactive)]
            pub struct PathModRs;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["PathModRs"]
        );
    }

    /// `#[path]` to a NON-`mod.rs` file makes that module's children SIBLINGS of
    /// the file — `rustc` introduces no directory component for a `#[path]`
    /// module. Mentally revert `child_module_search_dir` to always appending the
    /// module name and this looks for `blur/helper.rs`, which does not exist.
    #[test]
    fn path_attribute_to_a_flat_file_makes_children_siblings() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/arm/mod.rs",
            "#[path = \"effects/blur.rs\"]\npub mod blur;\n",
        );
        write(root, "processors/arm/effects/blur.rs", "pub mod helper;\n");
        write(
            root,
            "processors/arm/effects/helper.rs",
            r#"#[processor("@tatolab/demo/PathSibling", execution = reactive)]
            pub struct PathSibling;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["PathSibling"]
        );
    }

    /// A `#[path]` on a non-inline `mod` resolves against the directory holding
    /// the DECLARING file, not the directory that file's plain `mod` children
    /// resolve against. Declaring file `processors/arm/inner.rs` resolves
    /// `#[path = "aliased.rs"]` to `processors/arm/aliased.rs`, never
    /// `processors/arm/inner/aliased.rs`.
    #[test]
    fn path_attribute_resolves_against_the_declaring_files_directory() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/arm/mod.rs", "pub mod inner;\n");
        write(
            root,
            "processors/arm/inner.rs",
            "#[path = \"aliased.rs\"]\npub mod aliased;\n",
        );
        write(
            root,
            "processors/arm/aliased.rs",
            r#"#[processor("@tatolab/demo/PathBase", execution = reactive)]
            pub struct PathBase;"#,
        );
        // The pre-fix resolution would look here and find nothing.
        write(
            root,
            "processors/arm/inner/decoy.rs",
            "// not a module of anything\n",
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["PathBase"]
        );
    }

    /// A `#[path]` inside an INLINE `mod` block still resolves against the
    /// inline module's directory chain, which for a non-`mod.rs` declaring file
    /// starts with a directory named after that file.
    #[test]
    fn path_attribute_inside_an_inline_module_keeps_the_inline_directory_chain() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/arm/mod.rs", "pub mod inner;\n");
        write(
            root,
            "processors/arm/inner.rs",
            "pub mod nested { #[path = \"aliased.rs\"] pub mod aliased; }\n",
        );
        write(
            root,
            "processors/arm/inner/nested/aliased.rs",
            r#"#[processor("@tatolab/demo/InlinePathBase", execution = reactive)]
            pub struct InlinePathBase;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["InlinePathBase"]
        );
    }

    /// A reachable `mod x;` with no backing file is a typed error (a compilable
    /// crate never hits it, but the walk surfaces it rather than dropping a
    /// subtree the target would compile).
    #[test]
    fn missing_reachable_module_file_is_typed_error() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/arm/mod.rs", "pub mod ghost;\n");
        let err = extract_reachable_rust_processors(root, &linux()).unwrap_err();
        match err {
            ExtractError::UnresolvedModule { module, .. } => assert_eq!(module, "ghost"),
            other => panic!("expected UnresolvedModule, got {other:?}"),
        }
    }

    /// An unconditional arm is reachable regardless of target.
    #[test]
    fn unconditional_arm_always_reachable() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/always.rs",
            r#"#[processor("@tatolab/demo/Always", execution = reactive)]
            pub struct Always;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &macos()).unwrap()),
            vec!["Always"]
        );
    }

    /// `for_host()` defines the running host's os/arch/family so the crate's own
    /// host arm is reachable.
    #[test]
    fn for_host_defines_host_atoms() {
        let host = ModuleReachabilityTarget::for_host();
        assert!(host.has_key_value("target_os", std::env::consts::OS));
        assert!(host.has_key_value("target_arch", std::env::consts::ARCH));
        assert!(host.has_flag(std::env::consts::FAMILY));
    }

    /// A package with no `processors/` directory derives the empty set — a
    /// schema-only Rust package is first-class, not an error.
    #[test]
    fn missing_processors_dir_yields_no_processors() {
        let tmp = tempdir();
        assert!(
            extract_reachable_rust_processors(tmp.path(), &linux())
                .unwrap()
                .is_empty()
        );
        assert!(
            enumerate_processor_source_module_arms(tmp.path())
                .unwrap()
                .is_empty()
        );
    }

    /// Arm enumeration: flat files and `mod.rs` directories become arms; a
    /// directory without a `mod.rs` and a non-`.rs` file are skipped, because
    /// `processors/` is the shared discovery root for every language.
    #[test]
    fn arm_enumeration_covers_flat_and_directory_arms_and_skips_foreign_entries() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/blur.rs", "");
        write(
            root,
            "processors/linux/mod.rs",
            "#![cfg(target_os = \"linux\")]\n",
        );
        write(root, "processors/vision/detector.py", "");
        write(root, "processors/effect.ts", "");

        let arms = enumerate_processor_source_module_arms(root).unwrap();
        let described: Vec<(String, String, Vec<String>)> = arms
            .into_iter()
            .map(|a| {
                (
                    a.module_name,
                    a.crate_root_relative_module_path,
                    a.file_level_cfg_predicates,
                )
            })
            .collect();
        assert_eq!(
            described,
            vec![
                (
                    "blur".to_string(),
                    "../processors/blur.rs".to_string(),
                    Vec::new()
                ),
                (
                    "linux".to_string(),
                    "../processors/linux/mod.rs".to_string(),
                    vec![r#"target_os = "linux""#.to_string()]
                ),
            ]
        );
    }

    /// Minimal tempdir (no `tempfile` dep in this lean crate).
    struct TmpDir(PathBuf);
    impl TmpDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tempdir() -> TmpDir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("slreach-{pid}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        TmpDir(dir)
    }
}
