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
//! [`GENERATED_CRATE_ROOT_DIR_NAME`] directory, which `streamlib-pack` excludes
//! from a shipped `.slpkg`, so the consumer regenerates it from the bundled
//! `processors/` tree on their own host.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use streamlib_idents::PACKAGE_PROCESSOR_SOURCE_DIR_NAME as PROCESSOR_SOURCE_DIR_NAME;

use crate::reachable::{
    ProcessorSourceModuleArm, enumerate_processor_source_module_arms,
    extract_processors_across_every_build_target,
};
use crate::{ExtractError, ExtractedProcessor};

/// Directory the generated crate root is written into, relative to the package
/// directory. Gitignored and excluded from a packed `.slpkg`.
///
/// Deliberately NOT the polyglot `_generated_/` unit: that directory's bare
/// presence is the "this package's Deno wire vocabulary has been provisioned"
/// oracle, and the build orchestrator promotes it as one atomically-swapped
/// unit. A Rust crate root sharing it would both satisfy that oracle for a
/// package whose Deno codegen never ran, and be deleted by the next Deno
/// promote. This directory is a single path component — the arm `#[path]`
/// prefix ([`generated_crate_root_arm_path_prefix`]) climbs exactly one level.
pub const GENERATED_CRATE_ROOT_DIR_NAME: &str = "_generated_rust_crate_root_";

/// File name of the generated crate root inside
/// [`GENERATED_CRATE_ROOT_DIR_NAME`]. A package's `Cargo.toml` points
/// `[lib] path` at [`generated_crate_root_lib_path_value`].
pub const GENERATED_CRATE_ROOT_FILE_NAME: &str = "lib.rs";

/// The build-dependency whose `build.rs` entrypoint writes the JTD shim the
/// generated crate root `include!`s.
const JTD_CODEGEN_BUILD_DEPENDENCY_NAME: &str = "streamlib-jtd-codegen";

/// What to generate for one package.
#[derive(Debug, Clone)]
pub struct RustCrateRootGenerationRequest<'request> {
    /// The package directory — the one holding `streamlib.yaml`, `Cargo.toml`
    /// and `processors/`.
    pub package_dir: &'request Path,
    /// Emit the `_generated_` JTD module (`include!` of the build script's
    /// `$OUT_DIR/_generated_shim.rs`). Keyed on the two things that together
    /// make that file exist — a `build.rs` and a `streamlib-jtd-codegen`
    /// build-dependency — never on a `build.rs` alone, which a package may own
    /// for shader or `cc` compilation and which would then leave the generated
    /// root `include!`ing a file nothing writes.
    pub emits_jtd_generated_module: bool,
    /// Emit the `export_plugin!` envelope. Keyed on the crate declaring a
    /// `cdylib` crate-type, never on "the package has processors" — a host
    /// package (`crate-type = ["rlib"]`) has processors and is statically
    /// linked, so it must emit no plugin declaration.
    pub emits_plugin_export_envelope: bool,
}

impl<'request> RustCrateRootGenerationRequest<'request> {
    /// Derive both emission decisions from the package directory: the JTD
    /// preamble from a `build.rs` plus the JTD codegen build-dependency, the
    /// plugin envelope from the `Cargo.toml`'s `[lib] crate-type` containing
    /// `cdylib`.
    pub fn for_package_dir(
        package_dir: &'request Path,
    ) -> Result<Self, RustCrateRootGenerationError> {
        Ok(Self::from_manifest(
            package_dir,
            &read_package_cargo_manifest(package_dir)?,
        ))
    }

    /// The same derivation, but `None` for a package that commits its own crate
    /// root — one manifest read answering both "does it opt in?" and "what does
    /// it emit?", so a generation site does not read and parse the same
    /// `Cargo.toml` twice per build.
    pub fn for_package_dir_if_generation_is_declared(
        package_dir: &'request Path,
    ) -> Result<Option<Self>, RustCrateRootGenerationError> {
        let manifest = read_package_cargo_manifest(package_dir)?;
        if !declares_generated_crate_root(&manifest) {
            return Ok(None);
        }
        Ok(Some(Self::from_manifest(package_dir, &manifest)))
    }

    fn from_manifest(package_dir: &'request Path, manifest: &toml::Value) -> Self {
        let emits_plugin_export_envelope = manifest
            .get("lib")
            .and_then(|lib| lib.get("crate-type"))
            .and_then(|crate_type| crate_type.as_array())
            .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("cdylib")));
        let declares_jtd_codegen_build_dependency = manifest
            .get("build-dependencies")
            .and_then(|deps| deps.get(JTD_CODEGEN_BUILD_DEPENDENCY_NAME))
            .is_some();

        Self {
            package_dir,
            emits_jtd_generated_module: declares_jtd_codegen_build_dependency
                && package_dir.join("build.rs").is_file(),
            emits_plugin_export_envelope,
        }
    }
}

/// Read and parse a package's `Cargo.toml`.
fn read_package_cargo_manifest(
    package_dir: &Path,
) -> Result<toml::Value, RustCrateRootGenerationError> {
    let cargo_toml_path = package_dir.join("Cargo.toml");
    let manifest_body = std::fs::read_to_string(&cargo_toml_path).map_err(|source| {
        RustCrateRootGenerationError::ReadCargoManifest {
            path: cargo_toml_path.clone(),
            source,
        }
    })?;
    manifest_body
        .parse()
        .map_err(|source| RustCrateRootGenerationError::ParseCargoManifest {
            path: cargo_toml_path,
            source,
        })
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

    /// A directory could not be read while discovering folder-backed packages.
    #[error("read directory {path}: {source}")]
    ReadPackageSearchDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A generation sweep discovered no folder-backed package. Refused rather
    /// than reported as success: a sweep that generates nothing leaves every
    /// package's crate root stale, and cargo then fails far away at target
    /// resolution with a missing `[lib] path`.
    #[error(
        "no folder-backed package found under {search_root} — a generation sweep that \
         generates nothing would let every package's crate root go stale unnoticed"
    )]
    NoFolderBackedPackageFound { search_root: PathBuf },
}

/// The literal `[lib] path` value a folder-backed package's `Cargo.toml`
/// declares — the one string every generation site keys opt-in on.
pub fn generated_crate_root_lib_path_value() -> String {
    format!("{GENERATED_CRATE_ROOT_DIR_NAME}/{GENERATED_CRATE_ROOT_FILE_NAME}")
}

/// Whether the package's `Cargo.toml` points `[lib] path` at the generated
/// crate root.
///
/// Generation is opt-in per package, keyed on the manifest rather than on "has
/// a `processors/` directory": a crate that commits its own root is never
/// overwritten, and a schema-only Rust package — no `processors/` at all —
/// still gets its JTD preamble. Nothing on the generation path notices that an
/// opted-in package's `processors/` went missing; it writes an arm-free root.
/// The seam that refuses that is `streamlib-pack`'s publish-time
/// `enforce_processor_manifest_matches_code`, which re-derives the processor
/// set from code and rejects a `.slpkg` whose committed `processors:` disagrees.
fn declares_generated_crate_root(manifest: &toml::Value) -> bool {
    manifest
        .get("lib")
        .and_then(|lib| lib.get("path"))
        .and_then(|path| path.as_str())
        .is_some_and(|path| path == generated_crate_root_lib_path_value())
}

/// Every directory at or under `search_root` whose `Cargo.toml` points
/// `[lib] path` at the generated crate root, sorted.
///
/// This is the one discovery every generation site shares — `cargo xtask
/// generate-crate-roots`, and the engine integration tests that shell out to
/// `cargo build -p <package>`. Two walks with different skip rules would let a
/// package be generated by one caller and silently missed by the other.
/// `target/`, `node_modules/` and dot-directories are skipped; a `Cargo.toml`
/// that is not parseable TOML is skipped rather than refused, so a fixture that
/// deliberately holds a malformed manifest does not break discovery.
pub fn discover_package_dirs_declaring_a_generated_crate_root(
    search_root: &Path,
) -> Result<Vec<PathBuf>, RustCrateRootGenerationError> {
    let mut out = Vec::new();
    collect_package_dirs_declaring_a_generated_crate_root(search_root, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_package_dirs_declaring_a_generated_crate_root(
    dir: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), RustCrateRootGenerationError> {
    // Matched rather than `?`-propagated: a directory with no manifest, or a
    // fixture holding a deliberately malformed one, is skipped rather than
    // failing discovery for the whole tree.
    if let Ok(manifest) = read_package_cargo_manifest(dir)
        && declares_generated_crate_root(&manifest)
    {
        out.push(dir.to_path_buf());
    }

    let entries = std::fs::read_dir(dir).map_err(|source| {
        RustCrateRootGenerationError::ReadPackageSearchDir {
            path: dir.to_path_buf(),
            source,
        }
    })?;
    for entry in entries {
        let entry = entry.map_err(
            |source| RustCrateRootGenerationError::ReadPackageSearchDir {
                path: dir.to_path_buf(),
                source,
            },
        )?;
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "target" || name == "node_modules" || name.starts_with('.') {
            continue;
        }
        collect_package_dirs_declaring_a_generated_crate_root(&entry.path(), out)?;
    }
    Ok(())
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

/// Generate and write the package's generated crate root, returning its path.
///
/// Content-compared first: rewriting identical bytes would bump the file's
/// mtime and force cargo to rebuild the crate on every invocation. A changed
/// root is written to a sibling temp file and renamed into place — the
/// orchestrator materializes packages concurrently and the engine's integration
/// tests regenerate every in-tree root from parallel test binaries, so a reader
/// (cargo, resolving `[lib] path`) must never observe a half-written root.
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

    let temp_path = dir.join(format!(
        ".{GENERATED_CRATE_ROOT_FILE_NAME}.{}.{}.partial",
        std::process::id(),
        GENERATED_CRATE_ROOT_TEMP_FILE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&temp_path, &generated.source).map_err(|source| {
        RustCrateRootGenerationError::WriteCrateRoot {
            path: temp_path.clone(),
            source,
        }
    })?;
    // Same-directory rename is atomic on POSIX: a concurrent reader sees either
    // the previous root or the complete new one, never a truncated file.
    std::fs::rename(&temp_path, &path).map_err(|source| {
        let _ = std::fs::remove_file(&temp_path);
        RustCrateRootGenerationError::WriteCrateRoot {
            path: path.clone(),
            source,
        }
    })?;
    Ok(path)
}

static GENERATED_CRATE_ROOT_TEMP_FILE_COUNTER: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Write every folder-backed package's crate root at or under `search_root`,
/// returning the written paths in discovery order.
///
/// The one generation sweep both in-tree sites share — `cargo xtask
/// generate-crate-roots` and the engine integration tests that shell out to
/// `cargo build -p <package>`. Off the monorepo the pre-cargo staging step
/// generates one package at a time and never reaches this. Two sweeps spelled
/// separately would let a step added to the contract land in only one of them.
#[tracing::instrument(skip_all, fields(search_root = %search_root.display()))]
pub fn write_generated_rust_crate_roots_under(
    search_root: &Path,
) -> Result<Vec<PathBuf>, RustCrateRootGenerationError> {
    let package_dirs = discover_package_dirs_declaring_a_generated_crate_root(search_root)?;
    if package_dirs.is_empty() {
        return Err(RustCrateRootGenerationError::NoFolderBackedPackageFound {
            search_root: search_root.to_path_buf(),
        });
    }

    let mut written = Vec::with_capacity(package_dirs.len());
    for package_dir in &package_dirs {
        let request = RustCrateRootGenerationRequest::for_package_dir(package_dir)?;
        written.push(write_generated_rust_crate_root(&request)?);
    }
    Ok(written)
}

/// One `#[path]`-attributed `pub mod` declaration, carrying the arm's own
/// file-level `#![cfg]` mirrored verbatim.
fn render_module_arm_declaration(arm: &ProcessorSourceModuleArm) -> String {
    let mut out = String::new();
    for predicate in &arm.file_level_cfg_predicates {
        let _ = writeln!(out, "#[cfg({predicate})]");
    }
    let _ = writeln!(
        out,
        "#[path = \"{}\"]\npub mod {};",
        arm.crate_root_relative_module_path, arm.module_name
    );
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
        let mut distinct: Vec<&str> = Vec::new();
        for predicate in entries
            .iter()
            .filter_map(|(predicate, _)| predicate.as_deref())
        {
            if !distinct.contains(&predicate) {
                distinct.push(predicate);
            }
        }
        let gate = match distinct.as_slice() {
            [single] => (*single).to_string(),
            many => format!("any({})", many.join(", ")),
        };
        let _ = writeln!(out, "#[cfg({gate})]");
    }
    out.push_str("streamlib_plugin_abi::export_plugin!(\n");
    for (predicate, type_path) in &entries {
        if let Some(predicate) = predicate {
            let _ = writeln!(out, "    #[cfg({predicate})]");
        }
        let _ = writeln!(out, "    {type_path},");
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

/// The `#[path]` prefix every generated arm declaration carries — the single
/// owner of the climb out of [`GENERATED_CRATE_ROOT_DIR_NAME`] into
/// `processors/`, so the `..` depth is stated once beside the directory name it
/// is a function of.
pub fn generated_crate_root_arm_path_prefix() -> String {
    debug_assert_eq!(
        Path::new(GENERATED_CRATE_ROOT_DIR_NAME)
            .components()
            .count(),
        1,
        "the generated crate root dir must be one path component — the arm \
         `#[path]` prefix climbs exactly one level"
    );
    format!("../{PROCESSOR_SOURCE_DIR_NAME}/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_fixture_tempdir::{
        ScanFixtureTempDir, scan_fixture_tempdir_named, write_scan_fixture_file as write,
    };

    fn tempdir() -> ScanFixtureTempDir {
        scan_fixture_tempdir_named("slcrateroot")
    }

    fn request<'a>(
        package_dir: &'a Path,
        cdylib: bool,
        jtd: bool,
    ) -> RustCrateRootGenerationRequest<'a> {
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
            "processors/capture_backends/mod.rs",
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
                "#[cfg(target_os = \"linux\")]\n#[path = \"../processors/capture_backends/mod.rs\"]\npub mod capture_backends;"
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
            "processors/capture_backends/mod.rs",
            r#"#![cfg(target_os = "linux")]
            pub mod camera;"#,
        );
        write(
            root,
            "processors/capture_backends/camera.rs",
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
            generated
                .source
                .contains("    crate::blur::BlurProcessor::Processor,\n"),
            "{}",
            generated.source
        );
        assert!(
            generated.source.contains(
                "    #[cfg(target_os = \"linux\")]\n    crate::capture_backends::camera::CameraProcessor::Processor,\n"
            ),
            "{}",
            generated.source
        );
        // One entry is unconditional, so the invocation itself needs no gate.
        assert!(
            !generated.source.contains("#[cfg(any(target_os"),
            "{}",
            generated.source
        );
        assert_eq!(generated.exported_processor_entry_count, 2);
    }

    /// Every entry conditional ⇒ the whole invocation is gated on the
    /// disjunction of the DISTINCT entry predicates, so a target compiling none
    /// of them emits no declaration rather than an unanchored one (which
    /// `export_plugin!` refuses). One distinct predicate folds to itself rather
    /// than a one-armed `any(...)`.
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
                "#[cfg(any(target_os = \"macos\", target_os = \"ios\"))]\n\
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
        assert!(
            !generated.source.contains("export_plugin!"),
            "{}",
            generated.source
        );
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

    /// A package with no `processors/` directory at all — a schema-only Rust
    /// package — generates an arm-free root rather than failing: generation is
    /// keyed on the manifest opt-in, and nothing on this path re-derives whether
    /// the package ought to have had processors. An opted-in package whose
    /// `processors/` went missing is caught one seam later, by
    /// `streamlib-pack`'s publish-time processor-manifest drift gate.
    #[test]
    fn a_package_with_no_processor_source_dir_generates_an_arm_free_root() {
        let tmp = tempdir();
        let root = tmp.path();

        let generated = generate_rust_crate_root_source(&request(root, true, true)).unwrap();
        assert_eq!(generated.module_arm_count, 0);
        assert_eq!(generated.exported_processor_entry_count, 0);
        assert!(
            !generated.source.contains("export_plugin!"),
            "{}",
            generated.source
        );
        assert!(
            generated.source.contains("pub mod _generated_ {"),
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
        assert_eq!(path, root.join(generated_crate_root_lib_path_value()));
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

    /// The generated root must never share the polyglot `_generated_/` unit: its
    /// bare presence is the Deno "wire vocabulary provisioned" oracle, and the
    /// build orchestrator atomically swaps that whole directory on a Deno
    /// promote.
    #[test]
    fn the_generated_crate_root_does_not_land_in_the_polyglot_generated_dir() {
        assert_ne!(GENERATED_CRATE_ROOT_DIR_NAME, "_generated_");
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/blur.rs", "pub struct NotAProcessor;\n");
        write_generated_rust_crate_root(&request(root, false, false)).unwrap();
        assert!(
            !root.join("_generated_").exists(),
            "generating a Rust crate root must not create the Deno provisioning \
             oracle's directory"
        );
    }

    /// A changed root is renamed into place, so a concurrent cargo resolving
    /// `[lib] path` never reads a truncated file. The discriminator is a reader
    /// opened BEFORE the rewrite: a same-directory rename leaves that reader on
    /// the old inode holding the whole previous root, while an in-place
    /// truncating write would change the bytes under it. Reverting the temp
    /// file + rename to a plain `std::fs::write` turns both assertions red —
    /// asserting only "no temp file survives" would not, since a plain write
    /// never creates one. Unix-gated because same-directory rename atomicity is
    /// the POSIX guarantee this depends on; Linux and Apple are the targets.
    #[test]
    #[cfg(unix)]
    fn a_changed_crate_root_is_renamed_into_place_and_leaves_no_temp_file() {
        use std::io::Read as _;
        use std::os::unix::fs::MetadataExt as _;

        let tmp = tempdir();
        let root = tmp.path();
        write(root, "processors/first.rs", "pub struct NotAProcessor;\n");
        let path = write_generated_rust_crate_root(&request(root, false, false)).unwrap();
        let inode_before_the_rewrite = std::fs::metadata(&path).unwrap().ino();
        let mut reader_opened_before_the_rewrite = std::fs::File::open(&path).unwrap();

        write(root, "processors/second.rs", "pub struct AlsoNot;\n");
        write_generated_rust_crate_root(&request(root, false, false)).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("pub mod second;")
        );

        let mut root_as_seen_by_the_earlier_reader = String::new();
        reader_opened_before_the_rewrite
            .read_to_string(&mut root_as_seen_by_the_earlier_reader)
            .unwrap();
        assert!(
            root_as_seen_by_the_earlier_reader.contains("pub mod first;")
                && !root_as_seen_by_the_earlier_reader.contains("pub mod second;"),
            "a reader that opened the previous root must keep reading it whole; \
             an in-place write would rewrite the file under it: \
             {root_as_seen_by_the_earlier_reader}"
        );
        assert_ne!(
            inode_before_the_rewrite,
            std::fs::metadata(&path).unwrap().ino(),
            "the published root must be a new inode renamed over the old one"
        );

        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name != GENERATED_CRATE_ROOT_FILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn the_envelope_decision_is_read_off_the_cargo_manifest_crate_type() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lib]\ncrate-type = [\"rlib\", \"cdylib\"]\n\n\
             [build-dependencies]\nstreamlib-jtd-codegen = { path = \"../../sdk/streamlib-jtd-codegen\" }\n",
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

    /// The JTD preamble `include!`s a file only the JTD codegen build script
    /// writes, so a package owning a `build.rs` for anything else (shader
    /// compilation, `cc`) must get no preamble — otherwise its generated root
    /// `include!`s a path nothing produces.
    #[test]
    fn a_build_script_that_is_not_the_jtd_codegen_emits_no_preamble() {
        let tmp = tempdir();
        let root = tmp.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"shaders\"\nversion = \"0.1.0\"\n\n\
             [build-dependencies]\ncc = \"1\"\n",
        );
        write(root, "build.rs", "fn main() {}\n");
        let derived = RustCrateRootGenerationRequest::for_package_dir(root).unwrap();
        assert!(!derived.emits_jtd_generated_module);
    }

    /// Generation is opt-in per package, keyed on the declared `[lib] path`, so
    /// a crate that commits its own root is never overwritten.
    #[test]
    fn only_a_package_declaring_the_generated_lib_path_opts_into_generation() {
        let opted_in = tempdir();
        write(
            opted_in.path(),
            "Cargo.toml",
            &format!(
                "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"{}\"\n",
                generated_crate_root_lib_path_value()
            ),
        );
        assert!(
            RustCrateRootGenerationRequest::for_package_dir_if_generation_is_declared(
                opted_in.path()
            )
            .unwrap()
            .is_some()
        );

        let hand_rooted = tempdir();
        write(
            hand_rooted.path(),
            "Cargo.toml",
            "[package]\nname = \"h\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"src/lib.rs\"\n",
        );
        assert!(
            RustCrateRootGenerationRequest::for_package_dir_if_generation_is_declared(
                hand_rooted.path()
            )
            .unwrap()
            .is_none()
        );

        let no_lib_section = tempdir();
        write(
            no_lib_section.path(),
            "Cargo.toml",
            "[package]\nname = \"n\"\nversion = \"0.1.0\"\n",
        );
        assert!(
            RustCrateRootGenerationRequest::for_package_dir_if_generation_is_declared(
                no_lib_section.path()
            )
            .unwrap()
            .is_none()
        );
    }

    /// One discovery walk backs both generation sites (the xtask verb and the
    /// engine integration tests' pre-build step). It must reach a nested crate
    /// (`examples/<example>/plugin/`), skip `target/`, and tolerate a fixture
    /// with a deliberately malformed manifest.
    #[test]
    fn discovery_finds_nested_opted_in_crates_and_skips_artifact_dirs() {
        let tmp = tempdir();
        let root = tmp.path();
        let opted_in = &format!(
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"{}\"\n",
            generated_crate_root_lib_path_value()
        );
        write(root, "packages/camera/Cargo.toml", opted_in);
        write(root, "examples/demo/plugin/Cargo.toml", opted_in);
        write(
            root,
            "packages/api-server/Cargo.toml",
            "[package]\nname = \"h\"\nversion = \"0.1.0\"\n",
        );
        write(root, "target/debug/build/stale/Cargo.toml", opted_in);
        write(root, ".cargo-cache/dep/Cargo.toml", opted_in);
        write(
            root,
            "packages/broken-fixture/Cargo.toml",
            ":::: not toml ::::\n",
        );

        let found = discover_package_dirs_declaring_a_generated_crate_root(root).unwrap();
        assert_eq!(
            found,
            vec![
                root.join("examples").join("demo").join("plugin"),
                root.join("packages").join("camera"),
            ],
            "discovery must reach nested crates and skip `target/` + dot-dirs"
        );
    }

    /// The sweep both in-tree generation sites call: it writes every discovered
    /// package's root and refuses a search root that discovers none, so a
    /// generation pass can never report success having generated nothing.
    #[test]
    fn the_generation_sweep_writes_every_discovered_package_and_refuses_an_empty_search_root() {
        let tmp = tempdir();
        let root = tmp.path();
        let opted_in = &format!(
            "[package]\nname = \"p\"\nversion = \"0.1.0\"\n\n[lib]\npath = \"{}\"\n",
            generated_crate_root_lib_path_value()
        );
        write(root, "packages/camera/Cargo.toml", opted_in);
        write(
            root,
            "packages/camera/processors/blur.rs",
            "pub struct B;\n",
        );
        write(root, "examples/demo/plugin/Cargo.toml", opted_in);
        write(
            root,
            "examples/demo/plugin/processors/sink.rs",
            "pub struct S;\n",
        );

        let written = write_generated_rust_crate_roots_under(root).unwrap();
        assert_eq!(
            written,
            vec![
                root.join("examples")
                    .join("demo")
                    .join("plugin")
                    .join(generated_crate_root_lib_path_value()),
                root.join("packages")
                    .join("camera")
                    .join(generated_crate_root_lib_path_value()),
            ]
        );
        assert!(
            std::fs::read_to_string(&written[1])
                .unwrap()
                .contains("pub mod blur;")
        );

        let no_packages = tempdir();
        assert!(matches!(
            write_generated_rust_crate_roots_under(no_packages.path()),
            Err(RustCrateRootGenerationError::NoFolderBackedPackageFound { .. })
        ));
    }
}
