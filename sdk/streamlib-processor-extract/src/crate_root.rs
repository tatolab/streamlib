// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generation of the Rust crate root a folder-backed package's `[lib] path`
//! points at.
//!
//! A package authors its processor modules under `processors/` and commits no
//! crate root at all. The root this module writes is the mechanical projection
//! of that directory: one `#[path]`-attributed `pub mod` per top-level arm, the
//! JTD `_generated_` preamble for a schema-bearing package, and — only for a
//! cdylib — one `export_plugin!` naming every processor the package declares on
//! any target.
//!
//! Two rules make the output cross-build stable rather than host-shaped:
//!
//! - every `#[cfg(...)]` is **mirrored verbatim** from the author's source, never
//!   re-derived, so a root generated on Linux is byte-identical to one generated
//!   on macOS;
//! - the `export_plugin!` invocation is gated on the disjunction of its entry
//!   predicates, so a target that compiles none of the package's processors emits
//!   no `STREAMLIB_PLUGIN` declaration instead of an unanchored one. An
//!   unconditional entry means no outer gate at all.
//!
//! The file is a build artifact: it lands under the package's gitignored
//! `_generated_/` directory, which `streamlib-pack` already excludes from a
//! shipped `.slpkg`, so the consumer regenerates it from the bundled
//! `processors/` tree on their own host.

use std::path::{Path, PathBuf};

use streamlib_idents::PACKAGE_PROCESSOR_SOURCE_DIR_NAME as PROCESSOR_SOURCE_DIR_NAME;

use crate::reachable::{
    ProcessorSourceModuleArm, enumerate_processor_source_module_arms,
    extract_processors_across_every_build_target,
};
use crate::{ExtractError, ExtractedProcessor};

/// Directory the generated crate root is written into, relative to the package
/// directory. Gitignored and excluded from a packed `.slpkg`.
pub const GENERATED_CRATE_ROOT_DIR_NAME: &str = "_generated_";

/// File name of the generated crate root inside
/// [`GENERATED_CRATE_ROOT_DIR_NAME`]. A package's `Cargo.toml` points
/// `[lib] path` at `_generated_/lib.rs`.
pub const GENERATED_CRATE_ROOT_FILE_NAME: &str = "lib.rs";

/// What to generate for one package.
#[derive(Debug, Clone)]
pub struct RustCrateRootGenerationRequest<'request> {
    /// The package directory — the one holding `streamlib.yaml`, `Cargo.toml`
    /// and `processors/`.
    pub package_dir: &'request Path,
    /// Emit the `_generated_` JTD module (`include!` of the build script's
    /// `$OUT_DIR/_generated_shim.rs`). True for any package whose `build.rs`
    /// runs the JTD codegen.
    pub emits_jtd_generated_module: bool,
    /// Emit the `export_plugin!` envelope. Keyed on the crate declaring a
    /// `cdylib` crate-type, never on "the package has processors" — a host
    /// package (`crate-type = ["rlib"]`) has processors and is statically
    /// linked, so it must emit no plugin declaration.
    pub emits_plugin_export_envelope: bool,
}

impl<'request> RustCrateRootGenerationRequest<'request> {
    /// Derive both emission decisions from the package directory: the JTD
    /// preamble from the presence of a `build.rs`, the plugin envelope from the
    /// `Cargo.toml`'s `[lib] crate-type` containing `cdylib`.
    pub fn for_package_dir(
        package_dir: &'request Path,
    ) -> Result<Self, RustCrateRootGenerationError> {
        let cargo_toml_path = package_dir.join("Cargo.toml");
        let manifest_body =
            std::fs::read_to_string(&cargo_toml_path).map_err(|source| {
                RustCrateRootGenerationError::ReadCargoManifest {
                    path: cargo_toml_path.clone(),
                    source,
                }
            })?;
        let manifest: toml::Value = manifest_body.parse().map_err(|source| {
            RustCrateRootGenerationError::ParseCargoManifest {
                path: cargo_toml_path.clone(),
                source,
            }
        })?;
        let emits_plugin_export_envelope = manifest
            .get("lib")
            .and_then(|lib| lib.get("crate-type"))
            .and_then(|crate_type| crate_type.as_array())
            .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("cdylib")));

        Ok(Self {
            package_dir,
            emits_jtd_generated_module: package_dir.join("build.rs").is_file(),
            emits_plugin_export_envelope,
        })
    }
}

/// The generated crate root plus what went into it, so a caller can log or
/// assert on the shape without re-parsing the source it just wrote.
#[derive(Debug, Clone)]
pub struct GeneratedCrateRootSource {
    /// The complete crate-root source text.
    pub source: String,
    /// How many `processors/` arms it declares.
    pub module_arm_count: usize,
    /// How many `export_plugin!` entries it names (zero when the package emits
    /// no plugin envelope).
    pub exported_processor_entry_count: usize,
}

/// Why crate-root generation failed. Distinct from [`ExtractError`] because the
/// generator also reads the Cargo manifest and writes to disk, neither of which
/// the source scan does.
#[derive(Debug, thiserror::Error)]
pub enum RustCrateRootGenerationError {
    /// The package's processor source could not be scanned.
    #[error(transparent)]
    Scan(#[from] ExtractError),

    /// The package's `Cargo.toml` could not be read.
    #[error("read Cargo manifest {path}: {source}")]
    ReadCargoManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The package's `Cargo.toml` is not parseable TOML.
    #[error("parse Cargo manifest {path}: {source}")]
    ParseCargoManifest {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// The generated crate root could not be written.
    #[error("write generated crate root {path}: {source}")]
    WriteCrateRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Build the crate-root source for one package.
#[tracing::instrument(skip_all, fields(package_dir = %request.package_dir.display()))]
pub fn generate_rust_crate_root_source(
    request: &RustCrateRootGenerationRequest<'_>,
) -> Result<GeneratedCrateRootSource, RustCrateRootGenerationError> {
    let arms = enumerate_processor_source_module_arms(request.package_dir)?;
    let processors = if request.emits_plugin_export_envelope {
        extract_processors_across_every_build_target(request.package_dir)?
    } else {
        Vec::new()
    };

    let mut source = String::new();
    source.push_str("// Copyright (c) 2025 Jonathan Fontanez\n");
    source.push_str("// SPDX-License-Identifier: BUSL-1.1\n\n");
    source.push_str(
        "//! Generated crate root — the mechanical projection of this package's\n\
         //! `processors/` directory. Do not edit: it is rewritten before every\n\
         //! cargo invocation and is excluded from the packed `.slpkg`.\n\n",
    );

    if request.emits_jtd_generated_module {
        source.push_str(
            "#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]\n\
             pub mod _generated_ {\n\
             \x20   include!(concat!(env!(\"OUT_DIR\"), \"/_generated_shim.rs\"));\n\
             }\n\n",
        );
    }

    for arm in &arms {
        source.push_str(&render_module_arm_declaration(arm));
    }

    let mut exported_processor_entry_count = 0;
    if request.emits_plugin_export_envelope
        && let Some(envelope) = render_plugin_export_envelope(&processors)
    {
        exported_processor_entry_count = processors.len();
        source.push('\n');
        source.push_str(&envelope);
    }

    tracing::debug!(
        arms = arms.len(),
        entries = exported_processor_entry_count,
        "generated crate root"
    );
    Ok(GeneratedCrateRootSource {
        source,
        module_arm_count: arms.len(),
        exported_processor_entry_count,
    })
}

/// Generate and write `<package_dir>/_generated_/lib.rs`, returning its path.
///
/// The write is unconditional but content-compared first: rewriting identical
/// bytes would bump the file's mtime and force cargo to rebuild the crate on
/// every invocation.
#[tracing::instrument(skip_all, fields(package_dir = %request.package_dir.display()))]
pub fn write_generated_rust_crate_root(
    request: &RustCrateRootGenerationRequest<'_>,
) -> Result<PathBuf, RustCrateRootGenerationError> {
    let generated = generate_rust_crate_root_source(request)?;
    let dir = request.package_dir.join(GENERATED_CRATE_ROOT_DIR_NAME);
    std::fs::create_dir_all(&dir).map_err(|source| {
        RustCrateRootGenerationError::WriteCrateRoot {
            path: dir.clone(),
            source,
        }
    })?;
    let path = dir.join(GENERATED_CRATE_ROOT_FILE_NAME);
    if std::fs::read_to_string(&path).is_ok_and(|existing| existing == generated.source) {
        return Ok(path);
    }
    std::fs::write(&path, &generated.source).map_err(|source| {
        RustCrateRootGenerationError::WriteCrateRoot {
            path: path.clone(),
            source,
        }
    })?;
    Ok(path)
}

/// One `#[path]`-attributed `pub mod` declaration, carrying the arm's own
/// file-level `#![cfg]` mirrored verbatim.
fn render_module_arm_declaration(arm: &ProcessorSourceModuleArm) -> String {
    let mut out = String::new();
    for predicate in &arm.file_level_cfg_predicates {
        out.push_str(&format!("#[cfg({predicate})]\n"));
    }
    out.push_str(&format!(
        "#[path = \"{}\"]\npub mod {};\n",
        arm.crate_root_relative_module_path, arm.module_name
    ));
    out
}

/// The `export_plugin!` invocation, or `None` when the package declares no
/// processor on any target (a cdylib whose only arm is parked ships no
/// `STREAMLIB_PLUGIN` symbol, which is what it did before folder-backed
/// discovery too).
fn render_plugin_export_envelope(processors: &[ExtractedProcessor]) -> Option<String> {
    if processors.is_empty() {
        return None;
    }

    let entries: Vec<(Option<String>, String)> = processors
        .iter()
        .map(|processor| {
            (
                conjoin_cfg_predicates(&processor.cfg_predicates),
                processor_export_type_path(processor),
            )
        })
        .collect();

    let mut out = String::new();
    // The declaration must not be emitted on a target that compiles none of
    // these entries: `export_plugin!` anchors its fingerprint on the first
    // surviving entry, and an all-stripped invocation is a compile error. An
    // unconditional entry makes the gate unnecessary.
    if entries.iter().all(|(predicate, _)| predicate.is_some()) {
        let disjunction = entries
            .iter()
            .filter_map(|(predicate, _)| predicate.clone())
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("#[cfg(any({disjunction}))]\n"));
    }
    out.push_str("streamlib_plugin_abi::export_plugin!(\n");
    for (predicate, type_path) in &entries {
        if let Some(predicate) = predicate {
            out.push_str(&format!("    #[cfg({predicate})]\n"));
        }
        out.push_str(&format!("    {type_path},\n"));
    }
    out.push_str(");\n");
    Some(out)
}

/// Fold the `#[cfg]` predicates in force at a processor into one predicate.
/// Several nested predicates are ANDed the way `rustc` applies them.
fn conjoin_cfg_predicates(predicates: &[String]) -> Option<String> {
    match predicates {
        [] => None,
        [single] => Some(single.clone()),
        many => Some(format!("all({})", many.join(", "))),
    }
}

/// The path an `export_plugin!` entry names: the module path from the crate
/// root, then the module the `#[processor]` macro derives from the struct name,
/// then its `Processor` type.
fn processor_export_type_path(processor: &ExtractedProcessor) -> String {
    let mut segments = processor.module_path_segments.clone();
    segments.push(processor.struct_name.clone());
    segments.push("Processor".to_string());
    format!("crate::{}", segments.join("::"))
}

/// The `#[path]` prefix every arm declaration uses, exposed so a consumer can
/// assert the generated root reaches out of `_generated_/` into `processors/`.
pub fn generated_crate_root_arm_path_prefix() -> String {
    format!("../{PROCESSOR_SOURCE_DIR_NAME}/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn request<'a>(package_dir: &'a Path, cdylib: bool, jtd: bool) -> RustCrateRootGenerationRequest<'a> {
        RustCrateRootGenerationRequest {
            package_dir,
            emits_jtd_generated_module: jtd,
            emits_plugin_export_envelope: cdylib,
        }
    }

    #[test]
    fn arms_are_declared_with_a_path_attribute_and_the_authors_cfg_verbatim() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/blur.rs",
            r#"#[processor("@tatolab/demo/Blur", execution = reactive)]
            pub struct BlurProcessor;"#,
        );
        write(
            root,
            "processors/linux/mod.rs",
            r#"#![cfg(target_os = "linux")]
            #[processor("@tatolab/demo/Camera", execution = reactive)]
            pub struct CameraProcessor;"#,
        );

        let generated = generate_rust_crate_root_source(&request(root, true, false)).unwrap();
        assert!(
            generated
                .source
                .contains("#[path = \"../processors/blur.rs\"]\npub mod blur;"),
            "{}",
            generated.source
        );
        assert!(
            generated.source.contains(
                "#[cfg(target_os = \"linux\")]\n#[path = \"../processors/linux/mod.rs\"]\npub mod linux;"
            ),
            "{}",
            generated.source
        );
        assert_eq!(generated.module_arm_count, 2);
    }

    #[test]
    fn export_entries_carry_the_processor_path_and_its_gating_predicate() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/blur.rs",
            r#"#[processor("@tatolab/demo/Blur", execution = reactive)]
            pub struct BlurProcessor;"#,
        );
        write(
            root,
            "processors/linux/mod.rs",
            r#"#![cfg(target_os = "linux")]
            pub mod camera;"#,
        );
        write(
            root,
            "processors/linux/camera.rs",
            r#"#[processor("@tatolab/demo/Camera", execution = reactive)]
            pub struct CameraProcessor;"#,
        );

        let generated = generate_rust_crate_root_source(&request(root, true, false)).unwrap();
        assert!(
            generated
                .source
                .contains("streamlib_plugin_abi::export_plugin!(\n"),
            "{}",
            generated.source
        );
        assert!(
            generated.source.contains("    crate::blur::BlurProcessor::Processor,\n"),
            "{}",
            generated.source
        );
        assert!(
            generated.source.contains(
                "    #[cfg(target_os = \"linux\")]\n    crate::linux::camera::CameraProcessor::Processor,\n"
            ),
            "{}",
            generated.source
        );
        // One entry is unconditional, so the invocation itself needs no gate.
        assert!(!generated.source.contains("#[cfg(any(target_os"), "{}", generated.source);
        assert_eq!(generated.exported_processor_entry_count, 2);
    }

    /// Every entry conditional ⇒ the whole invocation is gated on the
    /// disjunction, so a target compiling none of them emits no declaration
    /// rather than an unanchored one (which `export_plugin!` refuses).
    #[test]
    fn an_all_conditional_package_gates_the_whole_invocation() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/apple_only.rs",
            r#"#![cfg(any(target_os = "macos", target_os = "ios"))]
            #[processor("@tatolab/demo/ClapEffect", execution = manual)]
            pub struct ClapEffectProcessor;"#,
        );

        let generated = generate_rust_crate_root_source(&request(root, true, false)).unwrap();
        assert!(
            generated.source.contains(
                "#[cfg(any(any(target_os = \"macos\", target_os = \"ios\")))]\n\
                 streamlib_plugin_abi::export_plugin!("
            ),
            "{}",
            generated.source
        );
    }

    /// A cdylib whose only arm is parked declares no processor on any target,
    /// so it emits no `export_plugin!` at all.
    #[test]
    fn a_fully_parked_cdylib_emits_no_plugin_envelope() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/_apple_impl_pending_/mod.rs",
            r#"#![cfg(any())]
            #[processor("@tatolab/demo/Parked", execution = reactive)]
            pub struct ParkedProcessor;"#,
        );

        let generated = generate_rust_crate_root_source(&request(root, true, false)).unwrap();
        assert!(!generated.source.contains("export_plugin!"), "{}", generated.source);
        assert_eq!(generated.exported_processor_entry_count, 0);
        // The arm is still declared — parked source must still be a module the
        // crate names, so unparking is a cfg edit and nothing else.
        assert!(
            generated
                .source
                .contains("#[path = \"../processors/_apple_impl_pending_/mod.rs\"]"),
            "{}",
            generated.source
        );
    }

    /// An rlib host package has processors and must NOT emit a plugin
    /// declaration — the envelope is keyed on crate-type, not on processors.
    #[test]
    fn a_non_cdylib_package_declares_arms_but_no_plugin_envelope() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/control_plane.rs",
            r#"#[processor("@tatolab/demo/ApiServer", execution = manual)]
            pub struct ApiServerProcessor;"#,
        );

        let generated = generate_rust_crate_root_source(&request(root, false, false)).unwrap();
        assert!(generated.source.contains("pub mod control_plane;"));
        assert!(!generated.source.contains("export_plugin!"));
    }

    #[test]
    fn the_jtd_preamble_is_emitted_only_for_a_schema_bearing_package() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/blur.rs", "pub struct NotAProcessor;\n");

        let with_jtd = generate_rust_crate_root_source(&request(root, false, true)).unwrap();
        assert!(with_jtd.source.contains("pub mod _generated_ {"));
        assert!(with_jtd.source.contains("_generated_shim.rs"));

        let without = generate_rust_crate_root_source(&request(root, false, false)).unwrap();
        assert!(!without.source.contains("pub mod _generated_ {"));
    }

    /// Nested predicates are ANDed onto the entry the way rustc applies them.
    #[test]
    fn nested_predicates_are_conjoined_onto_the_entry() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "processors/gated.rs",
            r#"#![cfg(unix)]

            #[cfg(feature = "cuda")]
            #[processor("@tatolab/demo/CudaOnly", execution = reactive)]
            pub struct CudaOnlyProcessor;"#,
        );
        let generated = generate_rust_crate_root_source(&request(root, true, false)).unwrap();
        assert!(
            generated
                .source
                .contains("    #[cfg(all(unix, feature = \"cuda\"))]\n"),
            "{}",
            generated.source
        );
    }

    /// The generated root is a pure function of `processors/`, so generating it
    /// twice is byte-identical — that is what makes it safe to run before every
    /// cargo invocation without churning mtimes.
    #[test]
    fn generation_is_deterministic() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/b.rs", "pub struct B;\n");
        write(root, "processors/a.rs", "pub struct A;\n");
        let first = generate_rust_crate_root_source(&request(root, false, false)).unwrap();
        let second = generate_rust_crate_root_source(&request(root, false, false)).unwrap();
        assert_eq!(first.source, second.source);
        assert!(
            first.source.find("pub mod a;").unwrap() < first.source.find("pub mod b;").unwrap()
        );
    }

    #[test]
    fn writing_the_crate_root_lands_under_the_generated_dir_and_is_idempotent() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/blur.rs", "pub struct NotAProcessor;\n");

        let path = write_generated_rust_crate_root(&request(root, false, false)).unwrap();
        assert_eq!(path, root.join("_generated_").join("lib.rs"));
        let first = std::fs::metadata(&path).unwrap().modified().unwrap();
        let again = write_generated_rust_crate_root(&request(root, false, false)).unwrap();
        assert_eq!(
            std::fs::metadata(&again).unwrap().modified().unwrap(),
            first,
            "an unchanged crate root must not be rewritten — the mtime bump would \
             force cargo to rebuild on every invocation"
        );
        assert!(path.starts_with(root));
        assert!(generated_crate_root_arm_path_prefix().starts_with("../"));
    }

    #[test]
    fn the_envelope_decision_is_read_off_the_cargo_manifest_crate_type() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lib]\ncrate-type = [\"rlib\", \"cdylib\"]\n",
        );
        write(root, "build.rs", "fn main() {}\n");
        let derived = RustCrateRootGenerationRequest::for_package_dir(root).unwrap();
        assert!(derived.emits_plugin_export_envelope);
        assert!(derived.emits_jtd_generated_module);

        let host = tempdir();
        write(
            host.path(),
            "Cargo.toml",
            "[package]\nname = \"h\"\nversion = \"0.1.0\"\n\n[lib]\ncrate-type = [\"rlib\"]\n",
        );
        let derived = RustCrateRootGenerationRequest::for_package_dir(host.path()).unwrap();
        assert!(!derived.emits_plugin_export_envelope);
        assert!(!derived.emits_jtd_generated_module);
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
        let dir = std::env::temp_dir().join(format!("slcrateroot-{pid}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        TmpDir(dir)
    }
}
