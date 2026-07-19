// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-language processor derivation + manifest-drift gate.
//!
//! [`crate::extract_reachable_rust_processors`] derives the Rust half of a
//! package's `processors:` section from `#[processor(...)]` attributes. Python
//! and Deno packages carry the same truth in their `@processor` decorators,
//! surfaced by the import-and-enumerate subprocess CLIs (`python -m
//! streamlib.extract_processors <dir>` / `deno run --allow-read
//! extract_processors.ts <dir>`), which emit the manifest processor list as JSON
//! on stdout. This module unifies all three into one derivation and one **drift
//! gate**: `streamlib pkg build` derives the processor set from code and refuses
//! to build a package whose committed `processors:` disagrees with it.
//!
//! ## The comparison surface
//!
//! The drift check compares the *identity surface* every extractor produces
//! uniformly — processor `Type` name, execution mode, and each port's name +
//! schema-type (or `any`). It deliberately excludes fields not every runtime's
//! extractor emits or that are authored/build-derived rather than code-derived:
//! `version` (release-core projection is a build concern), `entrypoint`
//! (author/loader concern), `config` binding, `description`, and the consumer-
//! side port policies (`read_mode` / `overflow` / `buffer_size`, which the
//! Python/Deno wire shape does not carry). What remains is exactly the surface a
//! stale hand-authored `processors:` would misstate: a processor added, removed,
//! or renamed in code; a port added, removed, reordered, or re-typed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use streamlib_processor_schema::{
    PortSchemaSpec, ProcessorLanguage, ProcessorPortSchema, ProcessorSchema,
    ProcessorSchemaExecution,
};

use crate::reachable::{ModuleReachabilityTarget, extract_reachable_rust_processors};
use crate::{ExtractError, ExtractedProcessor};

/// A source language a package hosts processors in. Detected structurally from
/// the package directory, independent of the (possibly-stale) `processors:`
/// list, so derivation never trusts the manifest to decide what to derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PackageLanguage {
    Rust,
    Python,
    TypeScript,
}

impl PackageLanguage {
    fn label(self) -> &'static str {
        match self {
            PackageLanguage::Rust => "rust",
            PackageLanguage::Python => "python",
            PackageLanguage::TypeScript => "deno",
        }
    }

    /// The [`PackageLanguage`] a committed processor's `runtime.language`
    /// belongs to — the bridge for filtering the committed `processors:` list to
    /// the languages actually derived.
    pub fn of_processor(schema: &ProcessorSchema) -> Self {
        match schema.runtime.language {
            ProcessorLanguage::Rust => PackageLanguage::Rust,
            ProcessorLanguage::Python => PackageLanguage::Python,
            ProcessorLanguage::TypeScript => PackageLanguage::TypeScript,
        }
    }
}

/// Detect which languages a package hosts processors in, from files on disk.
///
/// - **Rust** — a `Cargo.toml` beside a `src/lib.rs` or `src/main.rs` crate root.
/// - **Python** — a `pyproject.toml` (the Python extractor scans the top-level
///   `*.py` beside it).
/// - **Deno** — a `deno.json`.
///
/// A package may host more than one (a polyglot package), or none (a schema-only
/// package like `@tatolab/core`).
pub fn detect_package_languages(package_dir: &Path) -> BTreeSet<PackageLanguage> {
    let mut out = BTreeSet::new();
    let has_crate_root =
        package_dir.join("src/lib.rs").is_file() || package_dir.join("src/main.rs").is_file();
    if package_dir.join("Cargo.toml").is_file() && has_crate_root {
        out.insert(PackageLanguage::Rust);
    }
    if package_dir.join("pyproject.toml").is_file() {
        out.insert(PackageLanguage::Python);
    }
    if package_dir.join("deno.json").is_file() {
        out.insert(PackageLanguage::TypeScript);
    }
    out
}

/// The language-uniform identity surface of one processor — the shape the drift
/// gate compares. Derived identically from a code-derived [`ProcessorSchema`]
/// (Rust scan) and from the Python/Deno subprocess wire JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorSurface {
    /// The `Type` segment of the processor's identity (PascalCase short name).
    pub name: String,
    /// Execution mode.
    pub execution: ProcessorSchemaExecution,
    /// Input ports, in declaration order.
    pub inputs: Vec<PortSurface>,
    /// Output ports, in declaration order.
    pub outputs: Vec<PortSurface>,
}

/// The identity surface of one port: its name and schema-type reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSurface {
    pub name: String,
    pub schema: PortSchemaSurface,
}

/// A port's schema reference, normalized across the bare-`Named`, resolved-
/// `Specific`, and version-free wire representations to just its `Type` segment
/// (or the `any` wildcard). Two representations of the same schema type compare
/// equal — a committed `schema: VideoFrame` (Named) and a code-derived
/// `@tatolab/core/VideoFrame@0.0.0` (Specific) are the same surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortSchemaSurface {
    Any,
    Type(String),
}

impl PortSchemaSurface {
    fn from_spec(spec: &PortSchemaSpec) -> Self {
        match spec {
            PortSchemaSpec::Any => PortSchemaSurface::Any,
            PortSchemaSpec::Named(name) => PortSchemaSurface::Type(name.as_str().to_string()),
            PortSchemaSpec::Specific(ident) => {
                PortSchemaSurface::Type(ident.r#type.as_str().to_string())
            }
        }
    }
}

impl PortSurface {
    fn from_port_schema(port: &ProcessorPortSchema) -> Self {
        PortSurface {
            name: port.name.clone(),
            schema: PortSchemaSurface::from_spec(&port.schema),
        }
    }
}

impl ProcessorSurface {
    /// Project a manifest-shaped [`ProcessorSchema`] (committed or Rust-derived)
    /// onto the drift comparison surface.
    pub fn from_processor_schema(schema: &ProcessorSchema) -> Self {
        ProcessorSurface {
            name: schema.name.clone(),
            execution: schema.execution.clone(),
            inputs: schema
                .inputs
                .iter()
                .map(PortSurface::from_port_schema)
                .collect(),
            outputs: schema
                .outputs
                .iter()
                .map(PortSurface::from_port_schema)
                .collect(),
        }
    }

    fn from_extracted(extracted: &ExtractedProcessor) -> Self {
        Self::from_processor_schema(&extracted.schema)
    }
}

/// Why cross-language derivation failed. Extraction never silently drops a
/// processor it could not derive; every failure surfaces with the language and
/// enough context to act on.
#[derive(Debug, thiserror::Error)]
pub enum DeriveError {
    /// The Rust source scan failed (see [`ExtractError`]).
    #[error(transparent)]
    RustScan(#[from] ExtractError),

    /// A per-language subprocess extractor could not be spawned.
    #[error("spawn {language} extractor for {package}: {source}")]
    SpawnExtractor {
        language: &'static str,
        package: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A per-language subprocess extractor exited non-zero. Carries the
    /// captured stderr so the author sees the underlying import/scan failure.
    #[error("{language} extractor for {package} failed (exit {code}): {stderr}")]
    ExtractorFailed {
        language: &'static str,
        package: PathBuf,
        code: String,
        stderr: String,
    },

    /// A subprocess extractor is required for a present language but no way to
    /// invoke it was configured (Deno's extractor script path, specifically).
    #[error("{language} processors present in {package} but no extractor is configured: {hint}")]
    ExtractorUnconfigured {
        language: &'static str,
        package: PathBuf,
        hint: String,
    },

    /// A subprocess extractor's stdout JSON did not parse into the manifest
    /// processor shape.
    #[error("parse {language} extractor output as manifest JSON: {source}")]
    MalformedExtractorJson {
        language: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

/// Spawns the Python / Deno import-and-enumerate subprocess extractors and
/// returns their manifest JSON stdout. Injectable so the derivation + drift
/// logic is unit-testable without a live Python/Deno runtime (which a sandboxed
/// build cannot observe — exit 144); the real implementation is
/// [`SystemSubprocessProcessorExtractor`].
pub trait SubprocessProcessorExtractor {
    /// Run the Python extractor over `package_dir`, returning stdout JSON.
    fn extract_python(&self, package_dir: &Path) -> Result<String, DeriveError>;
    /// Run the Deno extractor over `package_dir`, returning stdout JSON.
    fn extract_deno(&self, package_dir: &Path) -> Result<String, DeriveError>;
}

/// The real subprocess extractor: spawns `python -m streamlib.extract_processors`
/// and `deno run --allow-read <extract_processors.ts>`.
///
/// The Python interpreter defaults to `python3` (override with
/// `STREAMLIB_PYTHON`); the Deno binary defaults to `deno` (override with
/// `STREAMLIB_DENO`). The Deno extractor script has no reliable default path —
/// it ships with the Deno SDK, whose on-disk location depends on the install —
/// so it is taken from `STREAMLIB_DENO_EXTRACTOR`; a Deno package built without
/// it set surfaces [`DeriveError::ExtractorUnconfigured`] rather than a guess.
#[derive(Debug, Default, Clone)]
pub struct SystemSubprocessProcessorExtractor;

impl SystemSubprocessProcessorExtractor {
    fn run(
        language: &'static str,
        package_dir: &Path,
        mut command: Command,
    ) -> Result<String, DeriveError> {
        let output = command
            .output()
            .map_err(|source| DeriveError::SpawnExtractor {
                language,
                package: package_dir.to_path_buf(),
                source,
            })?;
        if !output.status.success() {
            return Err(DeriveError::ExtractorFailed {
                language,
                package: package_dir.to_path_buf(),
                code: output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".to_string()),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl SubprocessProcessorExtractor for SystemSubprocessProcessorExtractor {
    fn extract_python(&self, package_dir: &Path) -> Result<String, DeriveError> {
        let python = std::env::var("STREAMLIB_PYTHON").unwrap_or_else(|_| "python3".to_string());
        let mut command = Command::new(python);
        command
            .arg("-m")
            .arg("streamlib.extract_processors")
            .arg(package_dir);
        Self::run("python", package_dir, command)
    }

    fn extract_deno(&self, package_dir: &Path) -> Result<String, DeriveError> {
        let script = std::env::var("STREAMLIB_DENO_EXTRACTOR").map_err(|_| {
            DeriveError::ExtractorUnconfigured {
                language: "deno",
                package: package_dir.to_path_buf(),
                hint: "set STREAMLIB_DENO_EXTRACTOR to the Deno SDK's extract_processors.ts"
                    .to_string(),
            }
        })?;
        let deno = std::env::var("STREAMLIB_DENO").unwrap_or_else(|_| "deno".to_string());
        let mut command = Command::new(deno);
        command
            .arg("run")
            .arg("--allow-read")
            .arg(script)
            .arg(package_dir);
        Self::run("deno", package_dir, command)
    }
}

/// Parse a Python/Deno extractor's stdout JSON into the drift comparison
/// surface. The wire shape is the `_to_manifest_json` / `toManifestJson` payload
/// the subprocess CLIs emit: a list of `{ name, schema_ident, execution,
/// scheduling, description, inputs[], outputs[] }`, where each port is `{ name,
/// schema: {org,package,type,version} | null, description }`.
pub fn parse_subprocess_manifest_json(
    language: &'static str,
    json: &str,
) -> Result<Vec<ProcessorSurface>, DeriveError> {
    let wire: Vec<WireProcessor> = serde_json::from_str(json)
        .map_err(|source| DeriveError::MalformedExtractorJson { language, source })?;
    Ok(wire.into_iter().map(WireProcessor::into_surface).collect())
}

#[derive(Debug, Deserialize)]
struct WireProcessor {
    name: String,
    execution: ProcessorSchemaExecution,
    #[serde(default)]
    inputs: Vec<WirePort>,
    #[serde(default)]
    outputs: Vec<WirePort>,
}

#[derive(Debug, Deserialize)]
struct WirePort {
    name: String,
    /// `null` for an `any` wildcard port, else the 4-field schema ident.
    #[serde(default)]
    schema: Option<WireIdent>,
}

#[derive(Debug, Deserialize)]
struct WireIdent {
    #[serde(rename = "type")]
    type_name: String,
}

impl WirePort {
    fn into_surface(self) -> PortSurface {
        PortSurface {
            name: self.name,
            schema: match self.schema {
                None => PortSchemaSurface::Any,
                Some(ident) => PortSchemaSurface::Type(ident.type_name),
            },
        }
    }
}

impl WireProcessor {
    fn into_surface(self) -> ProcessorSurface {
        ProcessorSurface {
            name: self.name,
            execution: self.execution,
            inputs: self.inputs.into_iter().map(WirePort::into_surface).collect(),
            outputs: self
                .outputs
                .into_iter()
                .map(WirePort::into_surface)
                .collect(),
        }
    }
}

/// One language whose derivation was skipped because its subprocess extractor
/// could not run — the runtime was absent, failed to import, or was
/// unconfigured. The build cannot prove agreement for a skipped language, so it
/// neither fabricates a pass nor a drift; the drift comparison simply excludes
/// that language's committed processors (see
/// [`filter_committed_to_languages`]).
#[derive(Debug, Clone)]
pub struct SkippedLanguage {
    pub language: PackageLanguage,
    pub reason: String,
}

/// The code-derived processor surface for a package, plus which languages were
/// actually derived and which were skipped for want of a runtime.
#[derive(Debug, Default)]
pub struct DerivedProcessorSet {
    /// The derived processor surfaces, sorted by `Type` name (a set).
    pub surfaces: Vec<ProcessorSurface>,
    /// The languages successfully derived — the languages whose committed
    /// processors the drift check may compare against.
    pub derived_languages: BTreeSet<PackageLanguage>,
    /// Languages present but skipped (extractor runtime absent / unconfigured).
    pub skipped: Vec<SkippedLanguage>,
}

/// Derive the processor identity surface for a package from code, across every
/// language it hosts. Rust is scanned in-process
/// ([`extract_reachable_rust_processors`] against `target`) and is always
/// derived; Python and Deno are derived by running their import-and-enumerate
/// subprocess extractors through `extractor`.
///
/// A Python/Deno extractor that cannot **run** — the runtime is absent, its
/// import failed, or (Deno) it is unconfigured — is a [`SkippedLanguage`], not a
/// hard error: extraction-is-import needs the runtime present, and a `pkg build`
/// that merely bundles a Python/Deno package as source on a host without that
/// runtime must still work. A malformed *output* from an extractor that DID run
/// is a hard [`DeriveError`] (the extractor ran and produced garbage — a real
/// bug, not an absent runtime). Rust scan failures are always hard.
#[tracing::instrument(skip_all, fields(package = %package_dir.display()))]
pub fn derive_package_processor_surfaces(
    package_dir: &Path,
    target: &ModuleReachabilityTarget,
    extractor: &dyn SubprocessProcessorExtractor,
) -> Result<DerivedProcessorSet, DeriveError> {
    let languages = detect_package_languages(package_dir);
    let mut set = DerivedProcessorSet::default();

    if languages.contains(&PackageLanguage::Rust) {
        let rust = extract_reachable_rust_processors(package_dir, target)?;
        set.surfaces
            .extend(rust.iter().map(ProcessorSurface::from_extracted));
        set.derived_languages.insert(PackageLanguage::Rust);
    }
    for (language, run) in [
        (PackageLanguage::Python, extractor.extract_python(package_dir)),
        (PackageLanguage::TypeScript, extractor.extract_deno(package_dir)),
    ] {
        if !languages.contains(&language) {
            continue;
        }
        match run {
            Ok(json) => {
                set.surfaces
                    .extend(parse_subprocess_manifest_json(language.label(), &json)?);
                set.derived_languages.insert(language);
            }
            Err(err @ (DeriveError::SpawnExtractor { .. }
            | DeriveError::ExtractorFailed { .. }
            | DeriveError::ExtractorUnconfigured { .. })) => {
                let reason = err.to_string();
                tracing::warn!(
                    language = language.label(),
                    reason = %reason,
                    "skipping processor drift check for a language whose extractor runtime is \
                     unavailable — its committed processors are not verified against code"
                );
                set.skipped.push(SkippedLanguage { language, reason });
            }
            Err(other) => return Err(other),
        }
    }

    set.surfaces.sort_by(|a, b| a.name.cmp(&b.name));
    tracing::debug!(processors = set.surfaces.len(), "derived across languages");
    Ok(set)
}

/// Filter a committed `processors:` list to only the processors whose runtime
/// language is in `languages` — the languages actually derived. A drift check
/// must never flag a committed processor whose language was skipped for want of
/// a runtime as "only in manifest".
pub fn filter_committed_to_languages(
    committed: &[ProcessorSchema],
    languages: &BTreeSet<PackageLanguage>,
) -> Vec<ProcessorSchema> {
    committed
        .iter()
        .filter(|schema| languages.contains(&PackageLanguage::of_processor(schema)))
        .cloned()
        .collect()
}

/// A committed `processors:` section that disagrees with what the code derives.
/// Carries the specific disagreement so the `pkg build` error is actionable.
#[derive(Debug)]
pub struct ManifestDriftReport {
    pub package_dir: PathBuf,
    /// Processors the code declares that the committed `processors:` omits.
    pub only_in_code: Vec<String>,
    /// Processors the committed `processors:` declares that the code does not.
    pub only_in_manifest: Vec<String>,
    /// Processors present in both whose identity surface differs, each with a
    /// one-line description of the disagreement.
    pub differing: Vec<String>,
}

impl std::fmt::Display for ManifestDriftReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "the committed `processors:` section in {} is stale — it disagrees with the \
             `#[processor(...)]` / `@processor(...)` declarations in code, which are the source \
             of truth:",
            self.package_dir.join("streamlib.yaml").display()
        )?;
        for name in &self.only_in_code {
            writeln!(
                f,
                "  - `{name}` is declared in code but missing from `processors:`"
            )?;
        }
        for name in &self.only_in_manifest {
            writeln!(
                f,
                "  - `{name}` is listed in `processors:` but no longer declared in code"
            )?;
        }
        for detail in &self.differing {
            writeln!(f, "  - {detail}")?;
        }
        write!(
            f,
            "fix-it: the `processors:` section is derived from code — update it to match the \
             declarations above (or remove the section entirely; it is regenerated at build), \
             then re-run `streamlib pkg build`"
        )
    }
}

impl std::error::Error for ManifestDriftReport {}

/// Compare the code-derived processor surface against a package's committed
/// `processors:` list. Returns `Ok(())` when they agree, else a
/// [`ManifestDriftReport`] naming every disagreement.
///
/// Both sides are keyed by processor `Type` name; a processor present on only
/// one side, or present on both with a differing execution mode or port set, is
/// drift. See the module docs for exactly which fields participate.
#[tracing::instrument(skip_all, fields(package = %package_dir.display()))]
pub fn check_processor_manifest_drift(
    package_dir: &Path,
    committed: &[ProcessorSchema],
    derived: &[ProcessorSurface],
) -> Result<(), ManifestDriftReport> {
    use std::collections::BTreeMap;

    let derived_by_name: BTreeMap<&str, &ProcessorSurface> =
        derived.iter().map(|p| (p.name.as_str(), p)).collect();
    let committed_surfaces: Vec<ProcessorSurface> = committed
        .iter()
        .map(ProcessorSurface::from_processor_schema)
        .collect();
    let committed_by_name: BTreeMap<&str, &ProcessorSurface> = committed_surfaces
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();

    let mut only_in_code = Vec::new();
    let mut only_in_manifest = Vec::new();
    let mut differing = Vec::new();

    for (name, code_surface) in &derived_by_name {
        match committed_by_name.get(name) {
            None => only_in_code.push((*name).to_string()),
            Some(committed_surface) => {
                if code_surface != committed_surface {
                    differing.push(describe_difference(name, committed_surface, code_surface));
                }
            }
        }
    }
    for name in committed_by_name.keys() {
        if !derived_by_name.contains_key(name) {
            only_in_manifest.push((*name).to_string());
        }
    }

    if only_in_code.is_empty() && only_in_manifest.is_empty() && differing.is_empty() {
        return Ok(());
    }
    only_in_code.sort();
    only_in_manifest.sort();
    differing.sort();
    Err(ManifestDriftReport {
        package_dir: package_dir.to_path_buf(),
        only_in_code,
        only_in_manifest,
        differing,
    })
}

/// One-line description of how two surfaces of the same-named processor differ.
fn describe_difference(name: &str, manifest: &ProcessorSurface, code: &ProcessorSurface) -> String {
    if manifest.execution != code.execution {
        return format!(
            "`{name}` execution differs: `processors:` says {:?}, code declares {:?}",
            manifest.execution, code.execution
        );
    }
    if port_names(&manifest.inputs) != port_names(&code.inputs) {
        return format!(
            "`{name}` input ports differ: `processors:` has [{}], code declares [{}]",
            port_names(&manifest.inputs).join(", "),
            port_names(&code.inputs).join(", ")
        );
    }
    if port_names(&manifest.outputs) != port_names(&code.outputs) {
        return format!(
            "`{name}` output ports differ: `processors:` has [{}], code declares [{}]",
            port_names(&manifest.outputs).join(", "),
            port_names(&code.outputs).join(", ")
        );
    }
    format!("`{name}` port schema types differ between `processors:` and code")
}

fn port_names(ports: &[PortSurface]) -> Vec<String> {
    ports.iter().map(|p| p.name.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// An extractor that returns canned JSON, so the derivation + drift logic is
    /// exercised without a live Python/Deno runtime.
    struct FakeExtractor {
        python: String,
        deno: String,
    }
    impl SubprocessProcessorExtractor for FakeExtractor {
        fn extract_python(&self, _dir: &Path) -> Result<String, DeriveError> {
            Ok(self.python.clone())
        }
        fn extract_deno(&self, _dir: &Path) -> Result<String, DeriveError> {
            Ok(self.deno.clone())
        }
    }

    fn linux() -> ModuleReachabilityTarget {
        ModuleReachabilityTarget::new()
            .with_key_value("target_os", "linux")
            .with_key_value("target_family", "unix")
            .with_flag("unix")
    }

    fn parse_committed(yaml: &str) -> Vec<ProcessorSchema> {
        let cfg: streamlib_processor_schema::ProjectConfigMinimal =
            serde_yaml::from_str(yaml).unwrap();
        cfg.processors
    }

    #[test]
    fn detects_rust_python_and_deno_structurally() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "Cargo.toml", "[package]\nname='x'\n");
        write(root, "src/lib.rs", "");
        write(root, "pyproject.toml", "");
        write(root, "deno.json", "{}");
        let langs = detect_package_languages(root);
        assert!(langs.contains(&PackageLanguage::Rust));
        assert!(langs.contains(&PackageLanguage::Python));
        assert!(langs.contains(&PackageLanguage::TypeScript));
    }

    #[test]
    fn cargo_without_crate_root_is_not_rust() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "Cargo.toml", "[package]\nname='x'\n");
        assert!(!detect_package_languages(root).contains(&PackageLanguage::Rust));
    }

    #[test]
    fn parses_python_wire_json_into_surface() {
        // The exact shape `_to_manifest_json` emits: bare-string execution, a
        // 4-field schema ident on a typed port, and `null` on an `any` port.
        let json = r#"[
          {
            "name": "PassThrough",
            "schema_ident": {"org":"tatolab","package":"camera","type":"PassThrough","version":"0.0.0"},
            "execution": "reactive",
            "scheduling": null,
            "description": null,
            "inputs": [
              {"name":"any_in","schema":null,"description":null}
            ],
            "outputs": [
              {"name":"video_out","schema":{"org":"tatolab","package":"core","type":"VideoFrame","version":"0.0.0"},"description":null}
            ]
          }
        ]"#;
        let surfaces = parse_subprocess_manifest_json("python", json).unwrap();
        assert_eq!(surfaces.len(), 1);
        assert_eq!(surfaces[0].name, "PassThrough");
        assert_eq!(surfaces[0].execution, ProcessorSchemaExecution::Reactive);
        assert_eq!(surfaces[0].inputs[0].schema, PortSchemaSurface::Any);
        assert_eq!(
            surfaces[0].outputs[0].schema,
            PortSchemaSurface::Type("VideoFrame".to_string())
        );
    }

    #[test]
    fn parses_continuous_map_execution_from_wire() {
        let json = r#"[{"name":"Gen","schema_ident":{"org":"a","package":"b","type":"Gen","version":"0.0.0"},
            "execution":{"type":"continuous","interval_ms":10},"scheduling":null,"description":null,
            "inputs":[],"outputs":[]}]"#;
        let surfaces = parse_subprocess_manifest_json("deno", json).unwrap();
        assert_eq!(
            surfaces[0].execution,
            ProcessorSchemaExecution::Continuous { interval_ms: 10 }
        );
    }

    #[test]
    fn malformed_extractor_json_is_typed_error() {
        let err = parse_subprocess_manifest_json("python", "not json").unwrap_err();
        assert!(matches!(err, DeriveError::MalformedExtractorJson { .. }));
    }

    #[test]
    fn rust_and_python_surfaces_union_and_match_committed() {
        // A polyglot package: one Rust processor (scanned) + one Python
        // processor (fake subprocess), both present in the committed manifest.
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "Cargo.toml", "[package]\nname='cam'\n");
        write(
            root,
            "src/lib.rs",
            r#"#[processor("@tatolab/camera/Camera", execution = manual,
                output("video", "@tatolab/core/VideoFrame"))]
            pub struct Camera;"#,
        );
        write(root, "pyproject.toml", "");
        let python = r#"[{"name":"PassThrough",
            "schema_ident":{"org":"tatolab","package":"camera","type":"PassThrough","version":"0.0.0"},
            "execution":"reactive","scheduling":null,"description":null,
            "inputs":[{"name":"any_in","schema":null,"description":null}],
            "outputs":[{"name":"video_out","schema":{"org":"tatolab","package":"core","type":"VideoFrame","version":"0.0.0"},"description":null}]}]"#;
        let extractor = FakeExtractor {
            python: python.to_string(),
            deno: String::new(),
        };

        let derived = derive_package_processor_surfaces(root, &linux(), &extractor)
            .unwrap()
            .surfaces;
        assert_eq!(derived.len(), 2);
        assert_eq!(derived[0].name, "Camera"); // sorted
        assert_eq!(derived[1].name, "PassThrough");

        let committed = parse_committed(
            r#"
processors:
- name: Camera
  version: 1.0.0
  runtime: rust
  execution: manual
  outputs:
  - name: video
    schema: VideoFrame
- name: PassThrough
  version: 1.0.0
  runtime: python
  entrypoint: src.pass:PassThrough
  execution: reactive
  inputs:
  - name: any_in
    schema: any
    read_mode: skip_to_latest
  outputs:
  - name: video_out
    schema: VideoFrame
"#,
        );
        // In sync — a committed manifest that names both processors with the
        // same execution + ports as code. `read_mode` / version / entrypoint on
        // the committed side are outside the drift surface, so they don't trip it.
        check_processor_manifest_drift(root, &committed, &derived).unwrap();
    }

    #[test]
    fn processor_only_in_code_is_drift() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "Cargo.toml", "[package]\nname='x'\n");
        write(
            root,
            "src/lib.rs",
            r#"
            #[processor("@tatolab/demo/Alpha", execution = reactive)]
            pub struct Alpha;
            #[processor("@tatolab/demo/Beta", execution = reactive)]
            pub struct Beta;
            "#,
        );
        let extractor = FakeExtractor {
            python: String::new(),
            deno: String::new(),
        };
        let derived = derive_package_processor_surfaces(root, &linux(), &extractor)
            .unwrap()
            .surfaces;

        // Committed manifest lists only Alpha — Beta was added in code and never
        // written to `processors:`. Mentally revert the drift check to `Ok(())`
        // and `pkg build` would ship a manifest missing Beta.
        let committed = parse_committed(
            r#"
processors:
- name: Alpha
  version: 1.0.0
  runtime: rust
  execution: reactive
"#,
        );
        let report = check_processor_manifest_drift(root, &committed, &derived).unwrap_err();
        assert_eq!(report.only_in_code, vec!["Beta"]);
        assert!(report.only_in_manifest.is_empty());
        assert!(report.to_string().contains("Beta"));
        assert!(report.to_string().contains("fix-it"));
    }

    #[test]
    fn processor_only_in_manifest_is_drift() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "Cargo.toml", "[package]\nname='x'\n");
        write(
            root,
            "src/lib.rs",
            r#"#[processor("@tatolab/demo/Alpha", execution = reactive)]
            pub struct Alpha;"#,
        );
        let extractor = FakeExtractor {
            python: String::new(),
            deno: String::new(),
        };
        let derived = derive_package_processor_surfaces(root, &linux(), &extractor)
            .unwrap()
            .surfaces;
        let committed = parse_committed(
            r#"
processors:
- name: Alpha
  version: 1.0.0
  runtime: rust
  execution: reactive
- name: Ghost
  version: 1.0.0
  runtime: rust
  execution: reactive
"#,
        );
        let report = check_processor_manifest_drift(root, &committed, &derived).unwrap_err();
        assert_eq!(report.only_in_manifest, vec!["Ghost"]);
    }

    #[test]
    fn execution_mismatch_is_drift() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "Cargo.toml", "[package]\nname='x'\n");
        write(
            root,
            "src/lib.rs",
            r#"#[processor("@tatolab/demo/Alpha", execution = manual)]
            pub struct Alpha;"#,
        );
        let extractor = FakeExtractor {
            python: String::new(),
            deno: String::new(),
        };
        let derived = derive_package_processor_surfaces(root, &linux(), &extractor)
            .unwrap()
            .surfaces;
        let committed = parse_committed(
            r#"
processors:
- name: Alpha
  version: 1.0.0
  runtime: rust
  execution: reactive
"#,
        );
        let report = check_processor_manifest_drift(root, &committed, &derived).unwrap_err();
        assert_eq!(report.differing.len(), 1);
        assert!(report.differing[0].contains("execution differs"));
    }

    #[test]
    fn added_port_is_drift() {
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "Cargo.toml", "[package]\nname='x'\n");
        write(
            root,
            "src/lib.rs",
            r#"#[processor("@tatolab/demo/Alpha", execution = reactive,
                output("a", "@tatolab/core/VideoFrame"),
                output("b", "@tatolab/core/VideoFrame"))]
            pub struct Alpha;"#,
        );
        let extractor = FakeExtractor {
            python: String::new(),
            deno: String::new(),
        };
        let derived = derive_package_processor_surfaces(root, &linux(), &extractor)
            .unwrap()
            .surfaces;
        let committed = parse_committed(
            r#"
processors:
- name: Alpha
  version: 1.0.0
  runtime: rust
  execution: reactive
  outputs:
  - name: a
    schema: VideoFrame
"#,
        );
        let report = check_processor_manifest_drift(root, &committed, &derived).unwrap_err();
        assert!(report.differing[0].contains("output ports differ"));
    }

    /// A present Python package whose extractor cannot run is a skip, not a hard
    /// error: derivation succeeds with the language recorded in `skipped`, and
    /// filtering the committed list to the derived languages drops the Python
    /// processors so they are not falsely flagged as drift. Mentally reroute the
    /// `ExtractorFailed` arm to `return Err(other)` and this build path would
    /// hard-fail whenever python/streamlib is absent — breaking source-only
    /// bundling.
    #[test]
    fn python_extractor_failure_is_skipped_not_hard_error() {
        struct FailingPython;
        impl SubprocessProcessorExtractor for FailingPython {
            fn extract_python(&self, dir: &Path) -> Result<String, DeriveError> {
                Err(DeriveError::ExtractorFailed {
                    language: "python",
                    package: dir.to_path_buf(),
                    code: "1".to_string(),
                    stderr: "No module named 'streamlib'".to_string(),
                })
            }
            fn extract_deno(&self, _dir: &Path) -> Result<String, DeriveError> {
                Ok("[]".to_string())
            }
        }
        let tmp = tempdir();
        let root = tmp.path();
        write(root, "pyproject.toml", "");
        let set = derive_package_processor_surfaces(root, &linux(), &FailingPython).unwrap();
        assert!(set.surfaces.is_empty());
        assert!(!set.derived_languages.contains(&PackageLanguage::Python));
        assert_eq!(set.skipped.len(), 1);
        assert_eq!(set.skipped[0].language, PackageLanguage::Python);

        let committed = parse_committed(
            r#"
processors:
- name: PassThrough
  version: 1.0.0
  runtime: python
  entrypoint: src.pass:PassThrough
  execution: reactive
"#,
        );
        // Python was skipped → its committed processor is filtered out → no drift.
        let filtered = filter_committed_to_languages(&committed, &set.derived_languages);
        assert!(filtered.is_empty());
        check_processor_manifest_drift(root, &filtered, &set.surfaces).unwrap();
    }

    #[test]
    fn schema_only_package_has_no_processors_and_no_drift() {
        let tmp = tempdir();
        let root = tmp.path();
        // No Cargo crate root, no pyproject, no deno.json — a schema-only package.
        let extractor = FakeExtractor {
            python: String::new(),
            deno: String::new(),
        };
        let derived = derive_package_processor_surfaces(root, &linux(), &extractor)
            .unwrap()
            .surfaces;
        assert!(derived.is_empty());
        check_processor_manifest_drift(root, &[], &derived).unwrap();
    }

    #[test]
    fn deno_unconfigured_is_typed_error() {
        // With STREAMLIB_DENO_EXTRACTOR unset, the system extractor reports an
        // actionable unconfigured error rather than guessing a script path.
        let tmp = tempdir();
        let root = tmp.path();
        // SAFETY: single-threaded test; no other thread reads the env here.
        unsafe {
            std::env::remove_var("STREAMLIB_DENO_EXTRACTOR");
        }
        let err = SystemSubprocessProcessorExtractor
            .extract_deno(root)
            .unwrap_err();
        assert!(matches!(err, DeriveError::ExtractorUnconfigured { .. }));
    }

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
        let dir = std::env::temp_dir().join(format!("slderive-{pid}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        TmpDir(dir)
    }
}
