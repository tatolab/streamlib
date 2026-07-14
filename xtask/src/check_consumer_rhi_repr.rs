// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI gate enforcing the `#[repr(...)]` discipline on POD types in
//! `streamlib-consumer-rhi`.
//!
//! Why this exists: `streamlib-consumer-rhi` is the cross-FFI carve-out
//! every adapter cdylib (vulkan / opengl / skia / cuda / cpu-readback)
//! depends on. Adapter vtables pass POD discriminants across the
//! plugin DSO boundary as bare scalars (`format_raw: u32`,
//! `usage_bits: u32`, `initial_layout_raw: i32`, …) and reconstitute on
//! the receiving side via `as`/`from_bits_truncate`/`VulkanLayout(raw)`
//! round-trips. A `pub enum` without an explicit `#[repr(...)]` has
//! unstable discriminant width and the receiving side reads the wrong
//! bytes. A scalar-newtype `pub struct X(T)` without
//! `#[repr(transparent)]` (or `#[repr(C)]`) has unstable layout
//! relative to its inner field and the receiving side reads the wrong
//! offset.
//!
//! Scope:
//!
//! - **Every `pub enum`** declared in `runtime/streamlib-consumer-rhi/src/`
//!   MUST carry an explicit `#[repr(...)]` attribute. Bare-enum repr is
//!   NOT stable across rustc versions — explicit discriminant width is
//!   non-negotiable.
//! - **Every `pub struct X(T)` tuple newtype** where `T` is a scalar
//!   (`u8` / `u16` / `u32` / `u64` / `i8` / `i16` / `i32` / `i64` /
//!   `usize` / `isize`) MUST carry `#[repr(transparent)]` or
//!   `#[repr(C)]`. This locks the newtype's byte layout to the inner
//!   scalar so adapter vtables can pass the raw value and the
//!   receiving side reconstitutes via `X(raw)` cleanly.
//!
//! Multi-field structs that genuinely cannot be `#[repr(C)]` (because
//! they hold `Arc<...>` / `Vec<...>` / `Mutex<...>` / etc. — the
//! consumer-side wrapper types like `ConsumerVulkanDevice`,
//! `ConsumerVulkanTexture`, `ConsumerVulkanBuffer`,
//! `ConsumerVulkanTimelineSemaphore`) are intentionally NOT flagged:
//! they don't cross the FFI boundary by value — each cdylib statically
//! links its own copy and uses it through methods, not direct field
//! access. The Rust-internal struct layout doesn't matter as long as
//! `Send + Sync` and the method-dispatch surface are honored. See
//! `docs/architecture/subprocess-rhi-parity.md` for the carve-out
//! shape.
//!
//! Same rationale exempts enums with one or more data-bearing variants
//! (`Foo(String)` / `Bar { x: u32 }`). They're Rust-internal algebraic
//! data types (error enums in particular) and a `#[repr(u32)]`
//! discriminant doesn't apply to their shape. **Footgun warning**: a
//! contributor adding a single data variant to an otherwise unit-only
//! `pub enum` in this crate will silently exempt it from the gate. If
//! the enum genuinely crosses the FFI boundary, that's a regression
//! the gate can't catch — keep new FFI-crossing enums unit-only and
//! pass payloads alongside as separate fields. See
//! `flags_pub_enum_with_unit_variants_only_and_no_repr` and
//! `skips_pub_enum_with_data_bearing_variants` for the boundary.
//!
//! Per-file opt-out is via a single-line pragma at top of file:
//!     // check-consumer-rhi-repr:allow-file
//! Reserved for unusual cases reviewers approve explicitly.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const CRATE_SRC: &str = "runtime/streamlib-consumer-rhi/src";

const ALLOW_FILE_PRAGMA: &str = "check-consumer-rhi-repr:allow-file";

/// Scalar field types that trigger the `#[repr(transparent)]` /
/// `#[repr(C)]` requirement on a `pub struct X(T)` tuple newtype.
const SCALAR_TYPES: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
];

#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub file: PathBuf,
    pub kind: ViolationKind,
    pub name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ViolationKind {
    /// `pub enum` without explicit `#[repr(...)]`.
    EnumMissingRepr,
    /// `pub struct X(T)` tuple newtype over a scalar without
    /// `#[repr(transparent)]` or `#[repr(C)]`.
    ScalarNewtypeMissingRepr,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let crate_src = workspace_root.join(CRATE_SRC);
    if !crate_src.exists() {
        anyhow::bail!(
            "check-consumer-rhi-repr: {} not found — has the crate moved?",
            crate_src.display()
        );
    }

    let violations = scan_dir(&crate_src)?;
    if violations.is_empty() {
        println!(
            "✓ check-consumer-rhi-repr: every `pub enum` carries an explicit \
             `#[repr(...)]` and every scalar-newtype `pub struct X(T)` carries \
             `#[repr(transparent)]` / `#[repr(C)]` in `{CRATE_SRC}`."
        );
        return Ok(());
    }
    eprintln!(
        "✗ check-consumer-rhi-repr: {} violation(s) — consumer-rhi POD type \
         without explicit byte-layout pin:",
        violations.len()
    );
    for v in &violations {
        let what = match v.kind {
            ViolationKind::EnumMissingRepr => "`pub enum` missing #[repr(...)]",
            ViolationKind::ScalarNewtypeMissingRepr => {
                "`pub struct X(T)` scalar newtype missing #[repr(transparent)] or #[repr(C)]"
            }
        };
        eprintln!("  {}: {} → {}", v.file.display(), v.name, what);
    }
    eprintln!(
        "\nFix:\n  \
         consumer-rhi POD types cross the plugin FFI boundary as bare scalars; \
         their byte layout is part of the wire contract. Add `#[repr(u32)]` or \
         `#[repr(i32)]` to enums (matching the Vulkan enumerant they mirror), and \
         `#[repr(transparent)]` to scalar-newtype `pub struct X(T)` types. See \
         `docs/architecture/subprocess-rhi-parity.md` and the existing types in \
         `runtime/streamlib-consumer-rhi/src/{{formats,vulkan_layout,pixel_format}}.rs` \
         for the canonical pattern.\n  \
         Note: this gate only inspects unit-only enums and scalar tuple newtypes. \
         An FFI-crossing `pub enum` that mixes in a data-bearing variant (e.g. \
         `Foo(String)`) silently slips past — keep FFI-crossing enums unit-only \
         and pass payloads alongside as separate fields."
    );
    anyhow::bail!("check-consumer-rhi-repr failed");
}

fn scan_dir(dir: &Path) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            violations.extend(scan_dir(&path)?);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        scan_file(&path, &mut violations)?;
    }
    Ok(violations)
}

fn scan_file(path: &Path, violations: &mut Vec<Violation>) -> Result<()> {
    let body = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if body.contains(ALLOW_FILE_PRAGMA) {
        return Ok(());
    }
    let file = match syn::parse_file(&body) {
        Ok(f) => f,
        Err(_) => return Ok(()),
    };
    for item in file.items {
        match item {
            syn::Item::Enum(e) => {
                if !matches!(e.vis, syn::Visibility::Public(_)) {
                    continue;
                }
                // Only POD-discriminant enums are FFI-crossing. An enum
                // with any data-bearing variant (`Foo(String)` /
                // `Bar { x: u32 }`) is a Rust-internal algebraic data
                // type — error types in particular — and a
                // `#[repr(u32)]` discriminant doesn't apply to its
                // shape. Skip them; they don't cross FFI by value.
                if !all_variants_are_unit(&e.variants) {
                    continue;
                }
                if !has_explicit_repr(&e.attrs) {
                    violations.push(Violation {
                        file: path.to_path_buf(),
                        kind: ViolationKind::EnumMissingRepr,
                        name: e.ident.to_string(),
                    });
                }
            }
            syn::Item::Struct(s) => {
                if !matches!(s.vis, syn::Visibility::Public(_)) {
                    continue;
                }
                if !is_scalar_tuple_newtype(&s.fields) {
                    continue;
                }
                if !has_transparent_or_c_repr(&s.attrs) {
                    violations.push(Violation {
                        file: path.to_path_buf(),
                        kind: ViolationKind::ScalarNewtypeMissingRepr,
                        name: s.ident.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// True if `fields` is a single-element tuple struct whose field type
/// is one of the scalar primitives in [`SCALAR_TYPES`].
fn is_scalar_tuple_newtype(fields: &syn::Fields) -> bool {
    let syn::Fields::Unnamed(unnamed) = fields else {
        return false;
    };
    if unnamed.unnamed.len() != 1 {
        return false;
    }
    let field = &unnamed.unnamed[0];
    let syn::Type::Path(p) = &field.ty else {
        return false;
    };
    let Some(last) = p.path.segments.last() else {
        return false;
    };
    SCALAR_TYPES.contains(&last.ident.to_string().as_str())
}

fn has_explicit_repr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| attr.path().is_ident("repr"))
}

/// True if every variant of `variants` is a bare unit variant
/// (`A` or `A = 5` — no `A(T)` and no `A { x: T }`).
fn all_variants_are_unit(
    variants: &syn::punctuated::Punctuated<syn::Variant, syn::token::Comma>,
) -> bool {
    variants
        .iter()
        .all(|v| matches!(v.fields, syn::Fields::Unit))
}

fn has_transparent_or_c_repr(attrs: &[syn::Attribute]) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("repr") {
            continue;
        }
        let Ok(list) = attr.meta.require_list() else {
            continue;
        };
        for tt in list.tokens.clone() {
            if let proc_macro2::TokenTree::Ident(id) = tt {
                if id == "transparent" || id == "C" {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn passes_on_well_formed_pod_types() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "lib.rs",
            r#"
#[repr(u32)]
pub enum Format { A = 0, B = 1 }

#[repr(transparent)]
pub struct Usages(u32);

#[repr(C)]
pub struct AlsoFine(i32);

// Non-POD wrapper structs are exempt.
pub struct Wrapper {
    inner: std::sync::Arc<u8>,
}
"#,
        );
        let v = scan_dir(tmp.path()).unwrap();
        assert!(v.is_empty(), "got {v:?}");
    }

    #[test]
    fn flags_pub_enum_without_repr() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "lib.rs",
            r#"
pub enum Drift { A, B }
"#,
        );
        let v = scan_dir(tmp.path()).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, ViolationKind::EnumMissingRepr);
        assert_eq!(v[0].name, "Drift");
    }

    #[test]
    fn flags_scalar_tuple_newtype_without_repr() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "lib.rs",
            r#"
pub struct LayoutDrift(i32);
"#,
        );
        let v = scan_dir(tmp.path()).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, ViolationKind::ScalarNewtypeMissingRepr);
        assert_eq!(v[0].name, "LayoutDrift");
    }

    #[test]
    fn skips_private_enum_without_repr() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "lib.rs",
            r#"
enum PrivateOk { A, B }

pub(crate) enum CrateLocalOk { A, B }
"#,
        );
        let v = scan_dir(tmp.path()).unwrap();
        assert!(
            v.is_empty(),
            "private/crate-local enums are not FFI surface: {v:?}"
        );
    }

    #[test]
    fn skips_multi_field_struct() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "lib.rs",
            r#"
pub struct TwoFields(u32, u32);

pub struct Named {
    pub a: u32,
}
"#,
        );
        let v = scan_dir(tmp.path()).unwrap();
        assert!(v.is_empty(), "multi-field structs are out of scope: {v:?}");
    }

    #[test]
    fn skips_non_scalar_newtype() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "lib.rs",
            r#"
pub struct StringWrapper(String);

pub struct ArcWrapper(std::sync::Arc<u8>);
"#,
        );
        let v = scan_dir(tmp.path()).unwrap();
        assert!(v.is_empty(), "non-scalar newtypes are out of scope: {v:?}");
    }

    #[test]
    fn skips_file_with_allow_pragma() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "lib.rs",
            r#"
// check-consumer-rhi-repr:allow-file
pub enum WouldOtherwiseFlag { A, B }
"#,
        );
        let v = scan_dir(tmp.path()).unwrap();
        assert!(
            v.is_empty(),
            "allow-file pragma should skip the file: {v:?}"
        );
    }

    #[test]
    fn skips_pub_enum_with_data_bearing_variants() {
        // Error-style enums with `String` / structured payloads are
        // Rust-internal — they can't be `#[repr(u32)]` and don't cross
        // FFI by value. The gate must skip them.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "lib.rs",
            r#"
pub enum InternalError {
    Gpu(String),
    Configuration(String),
}

pub enum Mixed {
    Unit,
    WithData(u32),
}
"#,
        );
        let v = scan_dir(tmp.path()).unwrap();
        assert!(v.is_empty(), "data-bearing enums are out of scope: {v:?}");
    }

    #[test]
    fn flags_pub_enum_with_unit_variants_only_and_no_repr() {
        // Unit-only enums are the FFI-crossing POD discriminant shape;
        // they MUST carry an explicit `#[repr(...)]`.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "lib.rs",
            r#"
pub enum DiscriminantOnly {
    A,
    B = 5,
    C,
}
"#,
        );
        let v = scan_dir(tmp.path()).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, ViolationKind::EnumMissingRepr);
        assert_eq!(v[0].name, "DiscriminantOnly");
    }

    #[test]
    fn recurses_into_subdirs() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "sub/inner.rs",
            r#"
pub enum NestedDrift { A, B }
"#,
        );
        let v = scan_dir(tmp.path()).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "NestedDrift");
    }
}
