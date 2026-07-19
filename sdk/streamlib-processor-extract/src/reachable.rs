// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Module-reachability resolution for the processor source-scan.
//!
//! [`crate::extract_rust_processors`] visits every `.rs` under `src/`, including
//! platform arms a given host does not compile (`linux/` vs `apple/`) and parked
//! directories (`_apple_impl_pending_/`). That raw scan over-collects: two
//! platform arms declaring the same processor both surface, and a parked module
//! surfaces a `#[processor(...)]` that never compiles on any target. Before
//! extraction can replace the hand-authored `processors:` as the authoritative
//! truth-source — and before a drift check between the two can be a hard
//! `pkg build` error without false positives on cfg-gated packages — the scan
//! must resolve to the set of modules the build **target** actually compiles.
//!
//! [`extract_reachable_rust_processors`] does that: it walks the module tree
//! from the crate root (`src/lib.rs` / `src/main.rs`), follows each `mod` the
//! way `rustc` does (honoring `#[path = "..."]`), evaluates the `#[cfg(...)]`
//! predicate on every `mod` and every `#[processor(...)]`-bearing struct against
//! a [`ModuleReachabilityTarget`], and collects only the processors that survive.
//!
//! The parked-directory convention needs no special case: a parked module is
//! declared `#[cfg(any())]` (an always-false predicate), so cfg evaluation skips
//! it exactly as `rustc` does — one rule, not a hard-coded directory name.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use syn::punctuated::Punctuated;
use syn::{Meta, Token};

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
    /// pkg build` extracts for — the package is built for the invoking host's
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

/// Derive the `processors:` manifest section from the modules a Rust crate
/// compiles **for `target`** — the reachability-resolved counterpart to
/// [`crate::extract_rust_processors`].
///
/// Starts at the crate root (`src/lib.rs`, else `src/main.rs`, else both when a
/// crate carries a lib and a bin), follows every reachable `mod` the way
/// `rustc` resolves module files, and evaluates `#[cfg(...)]` on each `mod` and
/// each `#[processor(...)]`-bearing struct against `target`. A `#[processor]`
/// under a cfg-excluded module — a cross-platform arm, a disabled feature, or a
/// `#[cfg(any())]` parked directory — is never collected. The result is
/// deterministic (source order within a file, module-declaration order across
/// files) and de-duplicated by resolved source file.
#[tracing::instrument(skip_all, fields(crate_root = %crate_root.display()))]
pub fn extract_reachable_rust_processors(
    crate_root: &Path,
    target: &ModuleReachabilityTarget,
) -> Result<Vec<ExtractedProcessor>, ExtractError> {
    let src_dir = crate_root.join("src");
    if !src_dir.is_dir() {
        return Err(ExtractError::NoSrcDir {
            root: crate_root.to_path_buf(),
        });
    }

    let mut walker = ReachableModuleWalker {
        crate_root,
        target,
        visited: BTreeSet::new(),
        out: Vec::new(),
    };

    // A crate root is `lib.rs` and/or `main.rs`; both are module roots whose
    // child modules resolve relative to `src/`. Scan whichever exist so a bare
    // `@app/local` bin (`main.rs`) and a plugin cdylib (`lib.rs`) both work.
    let mut scanned_any = false;
    for root_name in ["lib.rs", "main.rs"] {
        let root_file = src_dir.join(root_name);
        if root_file.is_file() {
            scanned_any = true;
            walker.walk_file(&root_file, &src_dir)?;
        }
    }
    if !scanned_any {
        return Err(ExtractError::NoSrcDir {
            root: crate_root.to_path_buf(),
        });
    }

    tracing::debug!(processors = walker.out.len(), "extracted (reachable)");
    Ok(walker.out)
}

/// Carries the walk state so the recursive descent doesn't thread five
/// parameters through every call.
struct ReachableModuleWalker<'walk> {
    crate_root: &'walk Path,
    target: &'walk ModuleReachabilityTarget,
    visited: BTreeSet<PathBuf>,
    out: Vec<ExtractedProcessor>,
}

impl ReachableModuleWalker<'_> {
    /// Parse `file` and process its items. `mod_dir` is the directory that this
    /// file's `mod <name>;` children resolve against: `src/` for the crate root
    /// and for a `src/foo/mod.rs`, or `src/foo/` for a `src/foo.rs` (which
    /// introduces the `foo` directory component).
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

        let rel = file
            .strip_prefix(self.crate_root)
            .unwrap_or(file)
            .to_path_buf();
        for item in &parsed.items {
            self.walk_item(item, file, mod_dir, &rel)?;
        }
        Ok(())
    }

    /// Process one item: collect a reachable `#[processor(...)]` struct, or
    /// descend into a cfg-reachable module (inline or external).
    fn walk_item(
        &mut self,
        item: &syn::Item,
        declaring_file: &Path,
        mod_dir: &Path,
        rel_path: &Path,
    ) -> Result<(), ExtractError> {
        match item {
            syn::Item::Struct(item_struct) => {
                // A struct behind a false `#[cfg(...)]` is not compiled, so its
                // `#[processor]` (if any) is not a real processor for this target.
                if !self.cfg_reachable(&item_struct.attrs) {
                    return Ok(());
                }
                if let Some(attr) = processor_attr(&item_struct.attrs) {
                    let extracted = parse_processor_attr(attr, &item_struct.ident, rel_path)?;
                    self.out.push(extracted);
                }
            }
            syn::Item::Mod(item_mod) => {
                if !self.cfg_reachable(&item_mod.attrs) {
                    return Ok(());
                }
                match &item_mod.content {
                    // Inline `mod foo { ... }` introduces a `foo` directory
                    // component for its children's file resolution.
                    Some((_, items)) => {
                        let inner_dir = mod_dir.join(item_mod.ident.to_string());
                        for inner in items {
                            self.walk_item(inner, declaring_file, &inner_dir, rel_path)?;
                        }
                    }
                    // External `mod foo;` resolves to a sibling file.
                    None => {
                        let child = self.resolve_module_file(item_mod, declaring_file, mod_dir)?;
                        // `#[path]` on the child controls its own directory the
                        // same rustc way: a `foo.rs` introduces `foo/`, a
                        // `foo/mod.rs` keeps its own dir as the child module dir.
                        let child_mod_dir = child_module_dir(&child, &item_mod.ident.to_string());
                        self.walk_file(&child, &child_mod_dir)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Resolve an external `mod <name>;` to its source file, honoring a
    /// `#[path = "..."]` override (relative to `mod_dir`) and otherwise the
    /// standard `<mod_dir>/<name>.rs` then `<mod_dir>/<name>/mod.rs` search.
    fn resolve_module_file(
        &self,
        item_mod: &syn::ItemMod,
        declaring_file: &Path,
        mod_dir: &Path,
    ) -> Result<PathBuf, ExtractError> {
        let name = item_mod.ident.to_string();

        if let Some(path_attr) = path_override(&item_mod.attrs) {
            let candidate = mod_dir.join(&path_attr);
            if candidate.is_file() {
                return Ok(candidate);
            }
            return Err(ExtractError::UnresolvedModule {
                module: name,
                declared_in: declaring_file.to_path_buf(),
                candidates: candidate.display().to_string(),
            });
        }

        let flat = mod_dir.join(format!("{name}.rs"));
        if flat.is_file() {
            return Ok(flat);
        }
        let nested = mod_dir.join(&name).join("mod.rs");
        if nested.is_file() {
            return Ok(nested);
        }
        Err(ExtractError::UnresolvedModule {
            module: name,
            declared_in: declaring_file.to_path_buf(),
            candidates: format!("{} or {}", flat.display(), nested.display()),
        })
    }

    /// Whether every `#[cfg(...)]` attribute on an item passes for the target.
    /// An item with no `#[cfg]` is always reachable; multiple `#[cfg]`s are
    /// ANDed (rustc applies each independently).
    fn cfg_reachable(&self, attrs: &[syn::Attribute]) -> bool {
        attrs
            .iter()
            .filter(|attr| attr.path().is_ident("cfg"))
            .all(|attr| self.eval_cfg_attr(attr))
    }

    /// Evaluate a single `#[cfg(<predicate>)]`. A malformed predicate the parser
    /// can't read is treated as unreachable (conservative: never over-collect a
    /// processor from a cfg we could not prove true).
    fn eval_cfg_attr(&self, attr: &syn::Attribute) -> bool {
        match attr.parse_args::<Meta>() {
            Ok(meta) => self.eval_cfg_meta(&meta),
            Err(_) => false,
        }
    }

    /// Evaluate a cfg predicate meta: `all(..)` / `any(..)` / `not(..)`
    /// combinators, `key = "value"` atoms, and bare flag atoms.
    fn eval_cfg_meta(&self, meta: &Meta) -> bool {
        match meta {
            Meta::Path(path) => path
                .get_ident()
                .is_some_and(|ident| self.target.has_flag(&ident.to_string())),
            Meta::NameValue(name_value) => {
                let Some(key) = name_value.path.get_ident().map(|i| i.to_string()) else {
                    return false;
                };
                match literal_str(&name_value.value) {
                    Some(value) => self.target.has_key_value(&key, &value),
                    None => false,
                }
            }
            Meta::List(list) => {
                let combinator = match list.path.get_ident() {
                    Some(ident) => ident.to_string(),
                    None => return false,
                };
                let Ok(inner) =
                    list.parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                else {
                    return false;
                };
                match combinator.as_str() {
                    // `all()` is vacuously true; `any()` is vacuously false —
                    // which is exactly why `#[cfg(any())]` parks a module.
                    "all" => inner.iter().all(|m| self.eval_cfg_meta(m)),
                    "any" => inner.iter().any(|m| self.eval_cfg_meta(m)),
                    "not" => inner.len() == 1 && !self.eval_cfg_meta(&inner[0]),
                    _ => false,
                }
            }
        }
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
/// against. A `.../foo/mod.rs` keeps `.../foo`; a `.../foo.rs` introduces the
/// `.../foo` directory component (matching rustc).
fn child_module_dir(child_file: &Path, mod_name: &str) -> PathBuf {
    let parent = child_file.parent().unwrap_or_else(|| Path::new(""));
    match child_file.file_name().and_then(|n| n.to_str()) {
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

    /// The parked-directory convention (`#[cfg(any())] mod _apple_impl_pending_;`)
    /// falls out of cfg evaluation for free: `any()` is vacuously false, so the
    /// parked subtree is never walked and its `#[processor]` never collected.
    /// Mentally revert `eval_cfg_meta`'s `any` arm to `true` and this fails.
    #[test]
    fn parked_cfg_any_module_is_excluded() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "src/lib.rs",
            r#"
            #[cfg(target_os = "linux")]
            pub mod linux_impl;

            #[cfg(any())]
            mod _apple_impl_pending_;
            "#,
        );
        write(
            root,
            "src/linux_impl.rs",
            r#"
            #[processor("@tatolab/demo/Camera", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct Camera;
            "#,
        );
        write(
            root,
            "src/_apple_impl_pending_/mod.rs",
            r#"
            #[processor("@tatolab/demo/AppleCamera", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct AppleCamera;
            "#,
        );

        let procs = extract_reachable_rust_processors(root, &linux()).unwrap();
        assert_eq!(names(procs), vec!["Camera"]);
    }

    /// Two platform arms declaring the same processor: only the arm the target
    /// compiles surfaces (the raw scan would surface both).
    #[test]
    fn cross_platform_arms_resolve_to_the_target_arm() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "src/lib.rs",
            r#"
            #[cfg(target_os = "linux")]
            pub mod linux;
            #[cfg(target_os = "macos")]
            pub mod apple;
            "#,
        );
        write(
            root,
            "src/linux.rs",
            r#"#[processor("@tatolab/demo/LinuxCam", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
            pub struct LinuxCam;"#,
        );
        write(
            root,
            "src/apple.rs",
            r#"#[processor("@tatolab/demo/AppleCam", execution = manual, output("v", "@tatolab/core/VideoFrame"))]
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

    /// A `#[processor]` directly on a cfg-gated struct (no module boundary) is
    /// gated too.
    #[test]
    fn cfg_on_the_struct_itself_is_honored() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "src/lib.rs",
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

    /// A module never `mod`-declared from the crate root is unreachable — a
    /// stray `.rs` under `src/` (scratch file, unwired arm) is not compiled and
    /// must not contribute a processor. The raw whole-tree scan would collect it.
    #[test]
    fn undeclared_file_under_src_is_unreachable() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "src/lib.rs", "pub mod wired;\n");
        write(
            root,
            "src/wired.rs",
            r#"#[processor("@tatolab/demo/Wired", execution = reactive)]
            pub struct Wired;"#,
        );
        write(
            root,
            "src/scratch.rs",
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
            "src/lib.rs",
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
            "src/lib.rs",
            r#"
            #[cfg(feature = "cuda")]
            pub mod cuda;
            "#,
        );
        write(
            root,
            "src/cuda.rs",
            r#"#[processor("@tatolab/demo/Cuda", execution = reactive)]
            pub struct Cuda;"#,
        );

        assert!(extract_reachable_rust_processors(root, &linux()).unwrap().is_empty());
        let with_cuda = linux().with_feature("cuda");
        assert_eq!(
            names(extract_reachable_rust_processors(root, &with_cuda).unwrap()),
            vec!["Cuda"]
        );
    }

    /// Nested module-file resolution: `mod.rs` keeps its dir, a `foo.rs`
    /// introduces the `foo/` directory for its own children, and `#[path]`
    /// overrides the search.
    #[test]
    fn nested_and_path_override_module_resolution() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "src/lib.rs",
            "pub mod devices;\n#[path = \"custom_location.rs\"]\npub mod aliased;\n",
        );
        // devices/mod.rs → devices/webcam.rs (mod.rs keeps its own dir).
        write(root, "src/devices/mod.rs", "pub mod webcam;\n");
        write(
            root,
            "src/devices/webcam.rs",
            r#"#[processor("@tatolab/demo/Webcam", execution = reactive)]
            pub struct Webcam;"#,
        );
        write(
            root,
            "src/custom_location.rs",
            r#"#[processor("@tatolab/demo/Aliased", execution = reactive)]
            pub struct Aliased;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["Aliased", "Webcam"]
        );
    }

    /// A `foo.rs` (not `mod.rs`) introduces a `foo/` directory component for its
    /// own `mod bar;` children.
    #[test]
    fn non_mod_rs_file_introduces_directory_component() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "src/lib.rs", "pub mod outer;\n");
        write(root, "src/outer.rs", "pub mod inner;\n");
        write(
            root,
            "src/outer/inner.rs",
            r#"#[processor("@tatolab/demo/Deep", execution = reactive)]
            pub struct Deep;"#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["Deep"]
        );
    }

    /// A reachable `mod x;` with no backing file is a typed error (a compilable
    /// crate never hits it, but the walk surfaces it rather than dropping a
    /// subtree the target would compile).
    #[test]
    fn missing_reachable_module_file_is_typed_error() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "src/lib.rs", "pub mod ghost;\n");
        let err = extract_reachable_rust_processors(root, &linux()).unwrap_err();
        match err {
            ExtractError::UnresolvedModule { module, .. } => assert_eq!(module, "ghost"),
            other => panic!("expected UnresolvedModule, got {other:?}"),
        }
    }

    /// A bare `@app/local` bin (`main.rs`, no `lib.rs`) is a valid crate root.
    #[test]
    fn main_rs_is_a_crate_root() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "src/main.rs",
            r#"
            #[processor(execution = reactive)]
            struct MyLocalThing;
            fn main() {}
            "#,
        );
        assert_eq!(
            names(extract_reachable_rust_processors(root, &linux()).unwrap()),
            vec!["MyLocalThing"]
        );
    }

    /// An unconditional module is reachable regardless of target.
    #[test]
    fn unconditional_module_always_reachable() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "src/lib.rs", "pub mod always;\n");
        write(
            root,
            "src/always.rs",
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

    /// Missing `src/` is the same typed error the raw scan gives.
    #[test]
    fn missing_src_dir_is_typed_error() {
        let tmp = tempdir();
        let err = extract_reachable_rust_processors(tmp.path(), &linux()).unwrap_err();
        assert!(matches!(err, ExtractError::NoSrcDir { .. }));
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
