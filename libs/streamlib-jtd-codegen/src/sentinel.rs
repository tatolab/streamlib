// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Sentinel-substitution codegen pipeline (Decision 7 of the milestone-10
//! architecture in `docs/architecture/schema-identity-and-packaging.md`).
//!
//! ## Why
//!
//! `jtd-codegen`'s per-backend output mangles types subtly differently
//! across runs and across backends, especially when types live in another
//! package. The fix is to replace cross-package references with deterministic
//! placeholder *sentinels* before `jtd-codegen` runs, then substitute the
//! sentinels back into native cross-package imports after `jtd-codegen`
//! completes. The backend never sees a real cross-package reference and
//! can't disagree about how to mangle one.
//!
//! ## Schema-side syntax
//!
//! A schema declares its cross-package imports in a top-level `imports:`
//! block. Each entry maps a local alias to a [`SchemaIdent`] structured
//! record:
//!
//! ```yaml
//! imports:
//!   EncodedVideoFrame:
//!     org: tatolab
//!     package: core
//!     type: EncodedVideoFrame
//!     version: "1.0.0"
//!
//! metadata:
//!   name: JtdCodegenFixtureA
//!
//! properties:
//!   source_frame:
//!     ref: EncodedVideoFrame
//! ```
//!
//! The pre-pass detects every `ref:` whose value matches a name in
//! `imports:` and replaces it with `ref: __STREAMLIB_REF_<hash>__`, adding
//! a placeholder `definitions[__STREAMLIB_REF_<hash>__] = { properties: {} }`
//! so `jtd-codegen` resolves the ref as an empty struct. After
//! `jtd-codegen` produces output, the post-pass strips the placeholder
//! struct and rewrites every reference to the sentinel into a native
//! cross-package import statement plus a use of the imported type.
//!
//! When a schema declares no `imports:` block (the in-tree state today —
//! every schema lives in `libs/streamlib`'s single bundle), the pre-pass
//! is a pass-through and the post-pass has nothing to substitute.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use streamlib_idents::{Org, Package, SchemaIdent, SemVer, TypeName};

/// Records the sentinel placeholder names a single schema produced and the
/// [`SchemaIdent`] each maps back to.
///
/// The pre-pass populates this table; the post-pass consumes it.
#[derive(Debug, Default, Clone)]
pub struct SentinelTable {
    pub map: BTreeMap<String, SchemaIdent>,
}

impl SentinelTable {
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn extend(&mut self, other: SentinelTable) {
        for (k, v) in other.map {
            self.map.insert(k, v);
        }
    }
}

/// Pre-pass: scan `schema_value` (a JTD schema as `serde_json::Value`) for
/// an `imports:` block and replace every `ref:` to an imported alias with a
/// deterministic sentinel. Adds placeholder definitions so `jtd-codegen` can
/// resolve refs as opaque empty structs.
///
/// On return, `imports:` is removed from `schema_value` (it's not part of
/// JTD; jtd-codegen would reject unknown top-level fields).
pub fn substitute(schema_value: &mut Value, table: &mut SentinelTable) -> anyhow::Result<()> {
    let root = match schema_value.as_object_mut() {
        Some(obj) => obj,
        None => return Ok(()),
    };

    let imports = match root.remove("imports") {
        Some(v) => v,
        None => return Ok(()),
    };

    let imports_obj = imports.as_object().ok_or_else(|| {
        anyhow::anyhow!("`imports` must be a map of alias → SchemaIdent record")
    })?;

    let mut alias_to_sentinel: BTreeMap<String, String> = BTreeMap::new();
    for (alias, ident_value) in imports_obj {
        let ident: SchemaIdent = parse_schema_ident_from_value(ident_value, alias)?;
        let sentinel = sentinel_name(&ident);
        alias_to_sentinel.insert(alias.clone(), sentinel.clone());
        table.map.insert(sentinel, ident);
    }

    rewrite_refs(schema_value, &alias_to_sentinel);

    if !alias_to_sentinel.is_empty() {
        let root = schema_value
            .as_object_mut()
            .expect("checked at top of function");
        let definitions = root
            .entry("definitions")
            .or_insert_with(|| Value::Object(Map::new()));
        let defs_obj = definitions.as_object_mut().ok_or_else(|| {
            anyhow::anyhow!("`definitions` must be a map (schema-level invariant)")
        })?;
        for sentinel in alias_to_sentinel.values() {
            defs_obj.insert(
                sentinel.clone(),
                serde_json::json!({ "properties": {} }),
            );
        }
    }

    Ok(())
}

fn parse_schema_ident_from_value(value: &Value, alias: &str) -> anyhow::Result<SchemaIdent> {
    #[derive(serde::Deserialize)]
    struct Raw {
        org: String,
        package: String,
        r#type: String,
        version: String,
    }
    let raw: Raw = serde_json::from_value(value.clone()).map_err(|e| {
        anyhow::anyhow!(
            "imports[{alias}] must be a {{org, package, type, version}} record: {e}"
        )
    })?;

    Ok(SchemaIdent::new(
        Org::new(raw.org)?,
        Package::new(raw.package)?,
        TypeName::new(raw.r#type)?,
        SemVer::deserialize_from_str(&raw.version)?,
    ))
}

fn rewrite_refs(value: &mut Value, alias_to_sentinel: &BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(s)) = map.get("ref") {
                if let Some(sentinel) = alias_to_sentinel.get(s) {
                    map.insert("ref".into(), Value::String(sentinel.clone()));
                }
            }
            for (_, v) in map.iter_mut() {
                rewrite_refs(v, alias_to_sentinel);
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                rewrite_refs(v, alias_to_sentinel);
            }
        }
        _ => {}
    }
}

/// Sentinel name for a [`SchemaIdent`]. Format: `__STREAMLIB_REF_h<hex16>__`.
///
/// `<hex16>` is the first 16 hex chars of `sha256(@org/package/Type@version)`.
/// The `h` prefix ensures the digest segment never starts with a digit —
/// `jtd-codegen` strips leading digits when mangling identifiers into Rust /
/// Python class names (Rust identifiers can't start with a digit), and a
/// leading-digit hash slips past the post-pass strip / replace because the
/// name in the generated output no longer matches the sentinel string.
/// `h` is opaque, lowercase, and survives all three backends' PascalCase
/// pass.
pub fn sentinel_name(ident: &SchemaIdent) -> String {
    let canonical = format!(
        "@{}/{}/{}@{}",
        ident.org.as_str(),
        ident.package.as_str(),
        ident.r#type.as_str(),
        ident.version
    );
    let digest = sha256_hex(&canonical);
    format!("__STREAMLIB_REF_h{}__", &digest[..16])
}

/// PascalCase → snake_case (e.g. `ColorInfo` → `color_info`,
/// `MasteringDisplay` → `mastering_display`). Insert an underscore before
/// every uppercase letter that's not the first character, then lowercase
/// the whole thing. Mirrors the same transform the codegen emits for
/// per-type Python module file names.
fn pascal_to_snake_simple(pascal: &str) -> String {
    let mut out = String::with_capacity(pascal.len() + pascal.len() / 4);
    for (i, c) in pascal.chars().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// PascalCase form a sentinel takes after `jtd-codegen` mangles it into a
/// type identifier (e.g. `__STREAMLIB_REF_b6261594f80ea4d8__` →
/// `StreamlibRefB6261594f80ea4d8`). Used by the language-specific
/// post-passes — `jtd-codegen` rewrites the sentinel into PascalCase
/// when it lands in a struct / class / interface name OR a field type
/// reference, so the strip + replace passes have to look for the
/// PascalCase form, not the original underscore-decorated sentinel.
fn mangle_sentinel_pascal(sentinel: &str) -> String {
    sentinel
        .trim_matches('_')
        .split('_')
        .filter(|seg| !seg.is_empty())
        .map(|seg| {
            let mut chars = seg.chars();
            match chars.next() {
                Some(first) => first
                    .to_uppercase()
                    .chain(chars.flat_map(|c| c.to_lowercase()))
                    .collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

fn sha256_hex(input: &str) -> String {
    // Reuse streamlib-idents' content-hash helper.
    let raw = streamlib_idents::hash_content(input.as_bytes());
    raw.strip_prefix("sha256:").unwrap_or(&raw).to_string()
}

/// Post-pass for Rust output: strip placeholder structs and rewrite sentinel
/// names into cross-package imports.
pub fn restore_rust(code: &str, table: &SentinelTable) -> String {
    if table.is_empty() {
        return code.to_string();
    }

    // jtd-codegen writes the sentinel into Rust output PascalCase'd
    // (`__STREAMLIB_REF_xxx__` → `StreamlibRefXxx`). The strip and
    // replace passes have to search for the mangled form.
    let mangled_sentinels: BTreeSet<String> = table
        .map
        .keys()
        .map(|s| mangle_sentinel_pascal(s))
        .collect();
    let stripped = strip_placeholder_decls_rust(code, &mangled_sentinels);

    let mut imports_by_module: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut sentinel_replacements: Vec<(String, String)> = Vec::new();
    for (sentinel, ident) in &table.map {
        let module_path = format!(
            "crate::_generated_::{}__{}",
            ident.org.as_str(),
            ident.package.as_str().replace('-', "_")
        );
        let type_name = ident.r#type.as_str().to_string();
        imports_by_module
            .entry(module_path)
            .or_default()
            .insert(type_name.clone());
        sentinel_replacements.push((mangle_sentinel_pascal(sentinel), type_name));
    }

    let mut substituted = stripped;
    for (sentinel, type_name) in &sentinel_replacements {
        substituted = substituted.replace(sentinel.as_str(), type_name.as_str());
    }

    let mut import_block = String::new();
    for (module_path, types) in &imports_by_module {
        let imports: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
        import_block.push_str(&format!(
            "use {}::{{{}}};\n",
            module_path,
            imports.join(", ")
        ));
    }
    if !import_block.is_empty() {
        import_block.push('\n');
    }

    insert_after_header(&substituted, &import_block)
}

/// Post-pass for Python output: strip placeholder dataclasses and rewrite
/// sentinel names into cross-package imports.
pub fn restore_python(code: &str, table: &SentinelTable) -> String {
    if table.is_empty() {
        return code.to_string();
    }

    // Same shape as Rust — `jtd-codegen` PascalCase's the sentinel
    // for Python class names, so search for the mangled form.
    let mangled_sentinels: BTreeSet<String> = table
        .map
        .keys()
        .map(|s| mangle_sentinel_pascal(s))
        .collect();
    let stripped = strip_placeholder_decls_python(code, &mangled_sentinels);

    let mut imports_by_module: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut replacements: Vec<(String, String)> = Vec::new();
    for (sentinel, ident) in &table.map {
        // Python: relative import directly from the per-type module
        // (`from ..tatolab__core.color_info import ColorInfo`) so the
        // codegen output is host-package-name agnostic AND avoids
        // re-entering the per-package `__init__.py` — same-package
        // imports otherwise loop through the package's own init while
        // its own names are still being bound, causing a partial-module
        // circular-import failure.
        let snake_type = pascal_to_snake_simple(ident.r#type.as_str());
        let module_path = format!(
            "..{}__{}.{}",
            ident.org.as_str(),
            ident.package.as_str().replace('-', "_"),
            snake_type
        );
        let type_name = ident.r#type.as_str().to_string();
        imports_by_module
            .entry(module_path)
            .or_default()
            .insert(type_name.clone());
        replacements.push((mangle_sentinel_pascal(sentinel), type_name));
    }

    let mut substituted = stripped;
    for (sentinel, type_name) in &replacements {
        substituted = substituted.replace(sentinel.as_str(), type_name.as_str());
    }

    let mut import_block = String::new();
    for (module_path, types) in &imports_by_module {
        let imports: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
        import_block.push_str(&format!("from {} import {}\n", module_path, imports.join(", ")));
    }
    if !import_block.is_empty() {
        import_block.push('\n');
    }

    insert_after_header(&substituted, &import_block)
}

/// Post-pass for TypeScript output: strip placeholder interfaces and
/// rewrite sentinel names into cross-package imports.
pub fn restore_typescript(code: &str, table: &SentinelTable) -> String {
    if table.is_empty() {
        return code.to_string();
    }

    // Same shape as Rust / Python — `jtd-codegen` PascalCase's the
    // sentinel for TypeScript interface / type names.
    let mangled_sentinels: BTreeSet<String> = table
        .map
        .keys()
        .map(|s| mangle_sentinel_pascal(s))
        .collect();
    let stripped = strip_placeholder_decls_typescript(code, &mangled_sentinels);

    let mut imports_by_module: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut replacements: Vec<(String, String)> = Vec::new();
    for (sentinel, ident) in &table.map {
        let module_path = format!(
            "../{}__{}/index.ts",
            ident.org.as_str(),
            ident.package.as_str().replace('-', "_")
        );
        let type_name = ident.r#type.as_str().to_string();
        imports_by_module
            .entry(module_path)
            .or_default()
            .insert(type_name.clone());
        replacements.push((mangle_sentinel_pascal(sentinel), type_name));
    }

    let mut substituted = stripped;
    for (sentinel, type_name) in &replacements {
        substituted = substituted.replace(sentinel.as_str(), type_name.as_str());
    }

    let mut import_block = String::new();
    for (module_path, types) in &imports_by_module {
        let imports: Vec<&str> = types.iter().map(|s| s.as_str()).collect();
        import_block.push_str(&format!(
            "import {{ {} }} from \"{}\";\n",
            imports.join(", "),
            module_path
        ));
    }
    if !import_block.is_empty() {
        import_block.push('\n');
    }

    insert_after_header(&substituted, &import_block)
}

/// Strip `pub struct __STREAMLIB_REF_xxx__ { ... }` and `pub enum
/// __STREAMLIB_REF_xxx__ { ... }` placeholder declarations along with their
/// preceding attribute / derive lines.
fn strip_placeholder_decls_rust(code: &str, sentinels: &BTreeSet<String>) -> String {
    let lines: Vec<&str> = code.lines().collect();
    let mut keep = vec![true; lines.len()];

    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let is_placeholder_decl = (trimmed.starts_with("pub struct ")
            || trimmed.starts_with("pub enum ")
            || trimmed.starts_with("struct ")
            || trimmed.starts_with("enum "))
            && sentinels.iter().any(|s| trimmed.contains(s.as_str()));

        if !is_placeholder_decl {
            i += 1;
            continue;
        }

        // Drop preceding attribute / doc-comment lines belonging to this decl.
        let mut start = i;
        while start > 0 {
            let prev = lines[start - 1].trim_start();
            if prev.starts_with("#[")
                || prev.starts_with("///")
                || prev.starts_with("//!")
                || prev.is_empty()
            {
                start -= 1;
            } else {
                break;
            }
        }

        // Drop the decl + body up to the matching closing brace.
        let mut depth = 0;
        let mut end = i;
        for (j, line) in lines.iter().enumerate().skip(i) {
            depth += line.matches('{').count() as i64;
            depth -= line.matches('}').count() as i64;
            if depth == 0 && j >= i {
                end = j;
                break;
            }
        }

        for k in start..=end {
            keep[k] = false;
        }

        i = end + 1;
    }

    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        if keep[idx] {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Strip Python `class __STREAMLIB_REF_xxx__: ...` placeholder dataclasses.
fn strip_placeholder_decls_python(code: &str, sentinels: &BTreeSet<String>) -> String {
    let lines: Vec<&str> = code.lines().collect();
    let mut keep = vec![true; lines.len()];

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let is_class_decl = line.trim_start().starts_with("class ")
            && sentinels.iter().any(|s| line.contains(s.as_str()));
        if !is_class_decl {
            i += 1;
            continue;
        }

        // Drop preceding decorator / blank lines belonging to this class.
        let mut start = i;
        while start > 0 {
            let prev = lines[start - 1].trim_start();
            if prev.starts_with('@') || prev.is_empty() {
                start -= 1;
            } else {
                break;
            }
        }

        // Drop the class body — everything indented under it.
        let mut end = i;
        for (j, l) in lines.iter().enumerate().skip(i + 1) {
            if l.trim().is_empty() {
                end = j;
                continue;
            }
            let is_indented = l.starts_with(' ') || l.starts_with('\t');
            if !is_indented {
                break;
            }
            end = j;
        }

        for k in start..=end {
            keep[k] = false;
        }
        i = end + 1;
    }

    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        if keep[idx] {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Strip TypeScript `export interface __STREAMLIB_REF_xxx__ { ... }`
/// placeholder interfaces.
fn strip_placeholder_decls_typescript(code: &str, sentinels: &BTreeSet<String>) -> String {
    let lines: Vec<&str> = code.lines().collect();
    let mut keep = vec![true; lines.len()];

    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let is_decl = (trimmed.starts_with("export interface ")
            || trimmed.starts_with("export type ")
            || trimmed.starts_with("export class "))
            && sentinels.iter().any(|s| trimmed.contains(s.as_str()));

        if !is_decl {
            i += 1;
            continue;
        }

        let mut start = i;
        while start > 0 {
            let prev = lines[start - 1].trim_start();
            if prev.starts_with("//") || prev.is_empty() {
                start -= 1;
            } else {
                break;
            }
        }

        // `export type` is a single-line decl ending in `;`.
        let single_line = trimmed.starts_with("export type ");
        let mut end = i;
        if single_line {
            // Walk to first line that ends in `;` (handles multi-line union types).
            for (j, l) in lines.iter().enumerate().skip(i) {
                if l.trim_end().ends_with(';') {
                    end = j;
                    break;
                }
            }
        } else {
            let mut depth = 0i64;
            for (j, l) in lines.iter().enumerate().skip(i) {
                depth += l.matches('{').count() as i64;
                depth -= l.matches('}').count() as i64;
                if depth == 0 && j >= i {
                    end = j;
                    break;
                }
            }
        }

        for k in start..=end {
            keep[k] = false;
        }
        i = end + 1;
    }

    let mut out = String::new();
    for (idx, line) in lines.iter().enumerate() {
        if keep[idx] {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Insert `block` after the file's leading copyright/header comments.
fn insert_after_header(code: &str, block: &str) -> String {
    if block.is_empty() {
        return code.to_string();
    }

    let lines: Vec<&str> = code.lines().collect();
    let mut header_end = 0;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if t.starts_with("//")
            || t.starts_with("#")
            || t.starts_with("/*")
            || t.starts_with("*")
            || t.is_empty()
        {
            header_end = i + 1;
            continue;
        }
        break;
    }

    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i == header_end {
            out.push_str(block);
        }
        out.push_str(line);
        out.push('\n');
    }
    if header_end >= lines.len() {
        out.push_str(block);
    }
    out
}

// =============================================================================
// SemVer string-form helper, since SchemaIdent::new wants a SemVer struct
// =============================================================================
//
// streamlib-idents intentionally does NOT expose a public `parse` API on
// SemVer — but it does expose the typed-deserialization pathway. We piggy-back
// on that here by deserializing a YAML-quoted string.
mod private_semver {
    use streamlib_idents::SemVer;

    pub trait DeserializeFromStr: Sized {
        fn deserialize_from_str(s: &str) -> anyhow::Result<Self>;
    }

    impl DeserializeFromStr for SemVer {
        fn deserialize_from_str(s: &str) -> anyhow::Result<Self> {
            let yaml = format!("\"{}\"", s);
            serde_yaml::from_str::<SemVer>(&yaml).map_err(|e| anyhow::anyhow!("invalid semver `{s}`: {e}"))
        }
    }
}
use private_semver::DeserializeFromStr;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml::from_str as yaml_from_str;

    fn make_ident(org: &str, package: &str, ty: &str, version: (u32, u32, u32)) -> SchemaIdent {
        SchemaIdent::new(
            Org::new(org).unwrap(),
            Package::new(package).unwrap(),
            TypeName::new(ty).unwrap(),
            SemVer::new(version.0, version.1, version.2),
        )
    }

    #[test]
    fn sentinel_name_is_deterministic() {
        let ident = make_ident("tatolab", "core", "VideoFrame", (1, 0, 0));
        let n1 = sentinel_name(&ident);
        let n2 = sentinel_name(&ident);
        assert_eq!(n1, n2);
        assert!(n1.starts_with("__STREAMLIB_REF_"));
        assert!(n1.ends_with("__"));
    }

    #[test]
    fn sentinel_name_differs_per_ident() {
        let a = make_ident("tatolab", "core", "VideoFrame", (1, 0, 0));
        let b = make_ident("tatolab", "core", "AudioFrame", (1, 0, 0));
        let c = make_ident("tatolab", "jtdcodegenother", "VideoFrame", (1, 0, 0));
        let d = make_ident("tatolab", "core", "VideoFrame", (1, 0, 1));
        assert_ne!(sentinel_name(&a), sentinel_name(&b));
        assert_ne!(sentinel_name(&a), sentinel_name(&c));
        assert_ne!(sentinel_name(&a), sentinel_name(&d));
    }

    #[test]
    fn substitute_passes_through_when_no_imports() {
        let yaml = r#"
metadata:
  name: Foo
properties:
  x:
    type: uint32
"#;
        let mut value: Value = yaml_from_str(yaml).unwrap();
        let mut table = SentinelTable::default();
        substitute(&mut value, &mut table).unwrap();
        assert!(table.is_empty());
        assert!(value.get("imports").is_none());
        assert!(value.get("definitions").is_none());
    }

    #[test]
    fn substitute_replaces_local_aliases_with_sentinels() {
        let yaml = r#"
imports:
  EncodedVideoFrame:
    org: tatolab
    package: core
    type: EncodedVideoFrame
    version: "1.0.0"

metadata:
  name: JtdCodegenFixtureA

properties:
  source:
    ref: EncodedVideoFrame
  bitrate:
    type: uint32
"#;
        let mut value: Value = yaml_from_str(yaml).unwrap();
        let mut table = SentinelTable::default();
        substitute(&mut value, &mut table).unwrap();

        // imports block consumed
        assert!(value.get("imports").is_none());

        // sentinel registered
        assert_eq!(table.map.len(), 1);
        let sentinel = table.map.keys().next().unwrap().clone();

        // ref rewritten
        let new_ref = value
            .pointer("/properties/source/ref")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(new_ref, sentinel);

        // placeholder definition added
        let placeholder = value.pointer(&format!("/definitions/{}", sentinel));
        assert!(placeholder.is_some());
    }

    #[test]
    fn substitute_handles_nested_refs() {
        let yaml = r#"
imports:
  Foo:
    org: tatolab
    package: core
    type: Foo
    version: "1.0.0"

metadata:
  name: Container
properties:
  list:
    elements:
      ref: Foo
"#;
        let mut value: Value = yaml_from_str(yaml).unwrap();
        let mut table = SentinelTable::default();
        substitute(&mut value, &mut table).unwrap();

        let sentinel = table.map.keys().next().unwrap().clone();
        let new_ref = value
            .pointer("/properties/list/elements/ref")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(new_ref, sentinel);
    }

    #[test]
    fn restore_rust_strips_placeholder_and_emits_use() {
        let ident = make_ident("tatolab", "core", "VideoFrame", (1, 0, 0));
        let sentinel = sentinel_name(&ident);
        let mangled = mangle_sentinel_pascal(&sentinel);
        let mut table = SentinelTable::default();
        table.map.insert(sentinel, ident);

        // jtd-codegen PascalCases the sentinel before the post-pass runs;
        // the fixture matches that observed shape so restore_rust's
        // strip+replace finds something to act on.
        let code = format!(
            "// Copyright (c) 2025 Jonathan Fontanez\n\
             // SPDX-License-Identifier: BUSL-1.1\n\n\
             #[derive(Debug, Default, Serialize, Deserialize)]\n\
             pub struct {} {{\n}}\n\n\
             #[derive(Debug, Default, Serialize, Deserialize)]\n\
             pub struct JtdCodegenFixtureA {{\n    pub source: {},\n    pub bitrate: u32,\n}}\n",
            mangled, mangled
        );

        let restored = restore_rust(&code, &table);
        assert!(
            !restored.contains(&mangled),
            "mangled sentinel must be replaced: {}",
            restored
        );
        assert!(
            restored.contains("use crate::_generated_::tatolab__core::{VideoFrame};"),
            "missing import block: {}",
            restored
        );
        assert!(
            restored.contains("pub source: VideoFrame,"),
            "field type not rewritten: {}",
            restored
        );
    }

    #[test]
    fn restore_python_strips_placeholder_and_emits_import() {
        let ident = make_ident("tatolab", "core", "VideoFrame", (1, 0, 0));
        let sentinel = sentinel_name(&ident);
        let mangled = mangle_sentinel_pascal(&sentinel);
        let mut table = SentinelTable::default();
        table.map.insert(sentinel, ident);

        let code = format!(
            "# Copyright (c) 2025 Jonathan Fontanez\n\n\
             from dataclasses import dataclass\n\n\
             @dataclass\nclass {}:\n    pass\n\n\
             @dataclass\nclass JtdCodegenFixtureA:\n    source: '{}'\n    bitrate: 'int'\n",
            mangled, mangled
        );

        let restored = restore_python(&code, &table);
        assert!(
            !restored.contains(&mangled),
            "mangled sentinel must be replaced: {}",
            restored
        );
        assert!(
            restored.contains("from ..tatolab__core.video_frame import VideoFrame"),
            "missing import: {}",
            restored
        );
        assert!(restored.contains("source: 'VideoFrame'"));
    }

    #[test]
    fn restore_typescript_strips_placeholder_and_emits_import() {
        let ident = make_ident("tatolab", "core", "VideoFrame", (1, 0, 0));
        let sentinel = sentinel_name(&ident);
        let mangled = mangle_sentinel_pascal(&sentinel);
        let mut table = SentinelTable::default();
        table.map.insert(sentinel, ident);

        let code = format!(
            "// Copyright (c) 2025 Jonathan Fontanez\n\n\
             export interface {} {{\n}}\n\n\
             export interface JtdCodegenFixtureA {{\n  source: {};\n  bitrate: number;\n}}\n",
            mangled, mangled
        );

        let restored = restore_typescript(&code, &table);
        assert!(
            !restored.contains(&mangled),
            "mangled sentinel must be replaced: {}",
            restored
        );
        assert!(
            restored.contains("import { VideoFrame } from \"../tatolab__core/index.ts\";"),
            "missing import: {}",
            restored
        );
        assert!(restored.contains("source: VideoFrame;"));
    }

    #[test]
    fn restore_no_op_when_table_empty() {
        let table = SentinelTable::default();
        let code = "pub struct Foo {}";
        assert_eq!(restore_rust(code, &table), code.to_string());
        assert_eq!(restore_python(code, &table), code.to_string());
        assert_eq!(restore_typescript(code, &table), code.to_string());
    }
}
