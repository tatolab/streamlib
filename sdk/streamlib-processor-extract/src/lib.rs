// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Manifest extraction as a runtime/loader capability.
//!
//! Ports-in-code (#1437) made the `#[processor(...)]` attribute the single
//! source of truth for a processor's identity, execution mode, and ports.
//! This crate is the inverse of the old flow: instead of the macro reading a
//! hand-authored `processors:` list, a `syn`-based source scan *derives* that
//! list from the `#[processor(...)]` usage under a package's `processors/`
//! directory — the same discovery root the Python and Deno extractors walk —
//! so `streamlib pkg publish` (and any future live-submit path) obtains the
//! processor manifest from code rather than trusting a committed enumeration.
//!
//! The same scan drives [`crate_root`], which writes the Rust crate root the
//! package's `[lib] path` points at: one `#[path]`-attributed `pub mod` per
//! `processors/` arm plus, for a cdylib, the `export_plugin!` envelope. One
//! scan, two consumers — a hand-written `mod` list can no longer disagree with
//! what the manifest says ships.
//!
//! The scan runs over a crate's source **without compiling it into the host**:
//! `syn::parse_file` builds each module's AST, the walk finds every struct
//! carrying `#[processor(...)]` / `#[streamlib::processor(...)]`, and the
//! attribute tokens are parsed through the SAME [`grammar`] the proc-macro uses
//! — never a second, drift-prone parser. See
//! `docs/decisions/manifest-extraction-capability.md` for why the grammar lives
//! here rather than the proc-macro crate, and the identity/version model the
//! scan produces.

pub mod crate_root;
pub mod derive;
pub mod grammar;
pub mod reachable;

use std::path::{Path, PathBuf};

use streamlib_idents::PACKAGE_PROCESSOR_SOURCE_DIR_NAME as PROCESSOR_SOURCE_DIR_NAME;
use streamlib_processor_schema::ProcessorSchema;

pub use crate_root::{
    GeneratedCrateRootSource, RustCrateRootGenerationRequest, generate_rust_crate_root_source,
    write_generated_rust_crate_root,
};
pub use derive::{
    DeriveError, DerivedProcessorSet, ExtractedManifestPort, ExtractedManifestProcessor,
    ManifestDriftReport, PackageLanguage, PortSchemaSurface, PortSurface, ProcessorSurface,
    SkippedLanguage, SubprocessProcessorExtractor, SystemSubprocessProcessorExtractor,
    check_processor_manifest_drift, derive_package_processor_surfaces, detect_package_languages,
    filter_committed_to_languages, parse_subprocess_manifest_json_full,
};
pub use grammar::{ParsedPort, ParsedProcessorAttr};
pub use reachable::{
    ModuleReachabilityTarget, ProcessorSourceModuleArm, enumerate_processor_source_module_arms,
    extract_processors_across_every_build_target, extract_reachable_rust_processors,
};

/// One processor derived from a `#[processor(...)]` attribute in source.
///
/// Carries the manifest-shaped [`ProcessorSchema`] the catalog / `.slpkg`
/// assembly consumes, plus the source location it was found at (for diagnostics)
/// and the config binding the attribute declared. The config binding is surfaced
/// separately rather than folded into `schema.config` because reconciling the
/// attribute's version-free config-schema id with the catalog's release-core
/// projection is the consuming layer's job, not the scanner's.
#[derive(Debug, Clone)]
pub struct ExtractedProcessor {
    /// The manifest-shaped processor schema derived from the attribute.
    pub schema: ProcessorSchema,
    /// The version-free config-schema identity the attribute declared (or
    /// synthesized from the config type), if the processor binds a config.
    pub config_schema_id: Option<String>,
    /// The struct field the runtime binds the typed config into.
    pub config_field_name: String,
    /// The Rust struct the attribute was written on (the `Type` segment source).
    pub struct_name: String,
    /// The source file, relative to the scanned package directory, the
    /// attribute was found in.
    pub source_file: PathBuf,
    /// The module path from the crate root to the declaring module, one
    /// segment per `mod` — `["linux", "camera"]` for a struct in
    /// `processors/linux/camera.rs`. Empty for the raw whole-tree scan, which
    /// resolves no module graph. The `#[processor]` macro turns the struct into
    /// a module of the same name holding a `Processor` type, so the generated
    /// `export_plugin!` entry is `<segments>::<struct_name>::Processor`.
    pub module_path_segments: Vec<String>,
    /// The `#[cfg(...)]` predicates in force at the struct, outermost first,
    /// as written in source. Populated only by
    /// [`extract_processors_across_every_build_target`]; a target-resolved scan
    /// has already pruned by them and leaves this empty.
    pub cfg_predicates: Vec<String>,
}

/// Why source-scan extraction failed. Every variant carries the offending path
/// and enough context to act on; extraction never panics and never silently
/// drops a `#[processor(...)]` it could not parse.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// The package has no `processors/` directory to scan.
    #[error(
        "no `processors/` directory under package {root} — nothing to scan for processors"
    )]
    NoProcessorSourceDir { root: PathBuf },

    /// A source file could not be read off disk.
    #[error("read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A `.rs` file did not parse as Rust. The scan requires a parseable AST;
    /// a syntactically-broken file is surfaced, never skipped (skipping could
    /// hide a `#[processor]` the author expects to ship).
    #[error("parse {path} as Rust: {source}")]
    Syntax {
        path: PathBuf,
        #[source]
        source: syn::Error,
    },

    /// A `#[processor(...)]` attribute failed the shared grammar. The message is
    /// the grammar's own spanned diagnostic, re-anchored to the file it came
    /// from so a build-time scan points at the offending source.
    #[error("`#[processor(...)]` on `{struct_name}` in {path}: {message}")]
    Grammar {
        path: PathBuf,
        struct_name: String,
        message: String,
    },

    /// A `mod <name>;` declaration reachable from the crate root resolved to no
    /// file on disk (neither `<name>.rs` nor `<name>/mod.rs`, nor a `#[path]`
    /// override). A compilable crate never hits this — the module-reachability
    /// walk surfaces it rather than silently dropping a subtree that the build
    /// target would have compiled.
    #[error(
        "`mod {module}` declared in {declared_in} resolves to no file \
         (looked for {candidates})"
    )]
    UnresolvedModule {
        module: String,
        declared_in: PathBuf,
        candidates: String,
    },
}

/// Derive the `processors:` manifest section from a Rust package's source.
///
/// Scans every `.rs` file under `<package_dir>/processors` for structs carrying
/// a `#[processor(...)]` attribute and parses each through the shared
/// [`grammar`]. The returned list is deterministic: files are walked in sorted
/// path order and attributes in source order within a file.
///
/// This raw walk resolves no module graph, so it over-collects — two platform
/// arms declaring the same processor both surface, as does a parked arm that
/// compiles on no target. [`extract_reachable_rust_processors`] is the
/// module-graph-resolved counterpart every consuming path uses.
#[tracing::instrument(skip_all, fields(package_dir = %package_dir.display()))]
pub fn extract_rust_processors(package_dir: &Path) -> Result<Vec<ExtractedProcessor>, ExtractError> {
    let processor_source_dir = package_dir.join(PROCESSOR_SOURCE_DIR_NAME);
    if !processor_source_dir.is_dir() {
        return Err(ExtractError::NoProcessorSourceDir {
            root: package_dir.to_path_buf(),
        });
    }

    let mut rs_files = Vec::new();
    collect_rs_files(&processor_source_dir, &mut rs_files)?;
    rs_files.sort();

    let mut out = Vec::new();
    for path in &rs_files {
        extract_from_file(path, package_dir, &mut out)?;
    }
    tracing::debug!(processors = out.len(), files = rs_files.len(), "extracted");
    Ok(out)
}

/// Recursively gather every `*.rs` file under `dir`.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ExtractError> {
    let entries = std::fs::read_dir(dir).map_err(|e| ExtractError::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| ExtractError::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| ExtractError::Io {
            path: path.clone(),
            source: e,
        })?;
        if file_type.is_dir() {
            collect_rs_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Parse one `.rs` file and push every `#[processor(...)]`-bearing struct into
/// `out`, in source order.
fn extract_from_file(
    path: &Path,
    package_dir: &Path,
    out: &mut Vec<ExtractedProcessor>,
) -> Result<(), ExtractError> {
    let body = std::fs::read_to_string(path).map_err(|e| ExtractError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let file = syn::parse_file(&body).map_err(|e| ExtractError::Syntax {
        path: path.to_path_buf(),
        source: e,
    })?;

    let rel = path.strip_prefix(package_dir).unwrap_or(path).to_path_buf();
    for item in &file.items {
        walk_item(item, &rel, out)?;
    }
    Ok(())
}

/// Walk one item, descending into inline `mod { ... }` blocks, collecting
/// `#[processor(...)]`-bearing structs.
fn walk_item(
    item: &syn::Item,
    rel_path: &Path,
    out: &mut Vec<ExtractedProcessor>,
) -> Result<(), ExtractError> {
    match item {
        syn::Item::Struct(item_struct) => {
            if let Some(attr) = processor_attr(&item_struct.attrs) {
                let extracted = parse_processor_attr(attr, &item_struct.ident, rel_path)?;
                out.push(extracted);
            }
        }
        syn::Item::Mod(item_mod) => {
            if let Some((_, items)) = &item_mod.content {
                for inner in items {
                    walk_item(inner, rel_path, out)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// The `#[processor(...)]` attribute on an item, if present. Matches both the
/// bare `#[processor(...)]` and the path-qualified `#[streamlib::processor(...)]`
/// forms by their final path segment.
pub(crate) fn processor_attr(attrs: &[syn::Attribute]) -> Option<&syn::Attribute> {
    attrs.iter().find(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "processor")
    })
}

/// Parse a single `#[processor(...)]` attribute through the shared grammar and
/// build the manifest-shaped [`ExtractedProcessor`].
pub(crate) fn parse_processor_attr(
    attr: &syn::Attribute,
    struct_ident: &syn::Ident,
    rel_path: &Path,
) -> Result<ExtractedProcessor, ExtractError> {
    // A `#[processor]` with no argument list (`Meta::Path`) synthesizes an
    // app-local identity from the struct name — parse an empty token stream so
    // the same grammar path handles it. A `#[processor(...)]` (`Meta::List`)
    // carries the args verbatim.
    let tokens = match &attr.meta {
        syn::Meta::List(list) => list.tokens.clone(),
        _ => proc_macro2::TokenStream::new(),
    };

    let parsed = grammar::parse2(tokens, struct_ident).map_err(|e| ExtractError::Grammar {
        path: rel_path.to_path_buf(),
        struct_name: struct_ident.to_string(),
        message: e.to_string(),
    })?;

    Ok(ExtractedProcessor {
        schema: parsed.to_processor_schema(),
        config_schema_id: parsed.config_schema_id.clone(),
        config_field_name: parsed.config_field_name.clone(),
        struct_name: struct_ident.to_string(),
        source_file: rel_path.to_path_buf(),
        module_path_segments: Vec::new(),
        cfg_predicates: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_processor_schema::{PortSchemaSpec, ProcessorSchemaExecution};

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn golden_extraction_over_a_fixture_crate() {
        let tmp = tempdir();
        let root = tmp.path();
        // Two processors across two files + a nested module, plus a plain
        // struct with no attribute (must be ignored).
        write(
            root,
            "processors/blur.rs",
            r#"
            #[streamlib::sdk::processor(
                "@tatolab/demo/Blur",
                execution = reactive,
                input("frames_in", "@tatolab/core/VideoFrame", delivery_profile = "latest"),
                output("frames_out", "@tatolab/core/VideoFrame"),
            )]
            pub struct Blur;

            pub struct NotAProcessor;
            "#,
        );
        write(
            root,
            "processors/camera.rs",
            r#"
            #[processor(
                "@tatolab/demo/Camera",
                execution = manual,
                scheduling = high,
                output("video", "@tatolab/core/VideoFrame"),
            )]
            pub struct Camera;

            mod inner {
                #[processor("@tatolab/demo/Inner", execution = continuous(interval_ms = 10))]
                pub struct Inner;
            }
            "#,
        );

        let mut procs = extract_rust_processors(root).unwrap();
        procs.sort_by(|a, b| a.schema.name.cmp(&b.schema.name));
        let names: Vec<&str> = procs.iter().map(|p| p.schema.name.as_str()).collect();
        assert_eq!(names, vec!["Blur", "Camera", "Inner"]);

        let blur = procs.iter().find(|p| p.schema.name == "Blur").unwrap();
        assert_eq!(blur.schema.execution, ProcessorSchemaExecution::Reactive);
        assert_eq!(blur.schema.inputs.len(), 1);
        assert_eq!(blur.schema.inputs[0].name, "frames_in");
        assert_eq!(blur.schema.inputs[0].delivery_profile.as_deref(), Some("latest"));
        assert!(matches!(
            blur.schema.inputs[0].schema,
            PortSchemaSpec::Specific(_)
        ));
        assert_eq!(blur.schema.outputs[0].name, "frames_out");
        assert_eq!(blur.source_file, PathBuf::from("processors/blur.rs"));

        let camera = procs.iter().find(|p| p.schema.name == "Camera").unwrap();
        assert_eq!(camera.schema.execution, ProcessorSchemaExecution::Manual);
        assert_eq!(
            camera.schema.scheduling.as_ref().map(|s| s.priority),
            Some(streamlib_processor_schema::ThreadPriority::High)
        );

        let inner = procs.iter().find(|p| p.schema.name == "Inner").unwrap();
        assert_eq!(
            inner.schema.execution,
            ProcessorSchemaExecution::Continuous { interval_ms: 10 }
        );
    }

    #[test]
    fn scan_schema_equals_the_macro_descriptor_projection() {
        // The 'one grammar, no drift' invariant, made structural: the schema the
        // source scan puts in the manifest and the descriptor the proc-macro
        // emits are BOTH `ParsedProcessorAttr::to_processor_schema()`. This locks
        // that the scanner routes through the shared projection rather than a
        // reintroduced parallel copy — a whole-struct serde comparison catches
        // any field the two would otherwise disagree on. Mentally reroute the
        // scanner to a hand-rolled builder and this fails.
        let attr = r#""@tatolab/demo/Blur",
            execution = reactive,
            scheduling = high,
            unsafe_send,
            description = "Blurs frames",
            input("frames_in", "@tatolab/core/VideoFrame", delivery_profile = "latest"),
            output("frames_out", "@tatolab/core/VideoFrame")"#;

        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/blur.rs",
            &format!("#[processor({attr})]\npub struct Blur;\n"),
        );
        let procs = extract_rust_processors(root).unwrap();
        assert_eq!(procs.len(), 1);
        let scanned = &procs[0].schema;

        // The proc-macro's own descriptor path: parse the same attribute tokens
        // through the shared grammar and project them the same way the macro does.
        let tokens: proc_macro2::TokenStream = attr.parse().unwrap();
        let struct_ident = syn::Ident::new("Blur", proc_macro2::Span::call_site());
        let macro_descriptor = grammar::parse2(tokens, &struct_ident)
            .unwrap()
            .to_processor_schema();

        assert_eq!(
            serde_json::to_value(scanned).unwrap(),
            serde_json::to_value(&macro_descriptor).unwrap(),
        );
    }

    #[test]
    fn bare_app_local_crate_extracts_cleanly() {
        // A bare crate with an identity-free `#[processor(...)]` synthesizes an
        // @app/local/<StructName> identity — extraction must handle it without a
        // streamlib.yaml anywhere in sight.
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/my_local_thing.rs",
            r#"
            #[processor(execution = reactive)]
            struct MyLocalThing;
            "#,
        );
        let procs = extract_rust_processors(root).unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].schema.name, "MyLocalThing");
        assert_eq!(procs[0].struct_name, "MyLocalThing");
    }

    #[test]
    fn schema_only_crate_yields_no_processors() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/types.rs", "pub struct JustAType { pub x: u32 }\n");
        let procs = extract_rust_processors(root).unwrap();
        assert!(procs.is_empty());
    }

    #[test]
    fn malformed_attribute_surfaces_a_grammar_error_with_the_file() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/broken.rs",
            r#"
            #[processor("@tatolab/demo/Broken")]
            pub struct Broken;
            "#,
        );
        let err = extract_rust_processors(root).unwrap_err();
        match err {
            ExtractError::Grammar {
                struct_name,
                path,
                message,
            } => {
                assert_eq!(struct_name, "Broken");
                assert_eq!(path, PathBuf::from("processors/broken.rs"));
                assert!(message.contains("missing required `execution`"), "got: {message}");
            }
            other => panic!("expected Grammar error, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_rust_is_a_syntax_error_not_a_skip() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/broken.rs", "fn broken( {\n");
        let err = extract_rust_processors(root).unwrap_err();
        assert!(matches!(err, ExtractError::Syntax { .. }));
    }

    #[test]
    fn missing_processors_dir_is_a_typed_error() {
        let tmp = tempdir();
        let err = extract_rust_processors(tmp.path()).unwrap_err();
        assert!(matches!(err, ExtractError::NoProcessorSourceDir { .. }));
    }

    /// A crate root left under `src/` is not a discovery root: only
    /// `processors/` is scanned, so a stale `src/lib.rs` contributes nothing.
    #[test]
    fn a_stale_src_crate_root_is_not_scanned() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "src/lib.rs",
            r#"#[processor("@tatolab/demo/Stale", execution = reactive)]
            pub struct Stale;"#,
        );
        write(
            root,
            "processors/live.rs",
            r#"#[processor("@tatolab/demo/Live", execution = reactive)]
            pub struct Live;"#,
        );
        let procs = extract_rust_processors(root).unwrap();
        let names: Vec<&str> = procs.iter().map(|p| p.schema.name.as_str()).collect();
        assert_eq!(names, vec!["Live"]);
    }

    /// Minimal tempdir without pulling the `tempfile` crate into this lean
    /// dependency set: a unique dir under the OS temp root, removed on drop.
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
        let dir = std::env::temp_dir().join(format!("slextract-{pid}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        TmpDir(dir)
    }
}
