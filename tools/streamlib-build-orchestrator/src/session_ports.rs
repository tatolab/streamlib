// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Session-source port extraction tail for the build orchestrator.
//!
//! A live-submitted `@session/<name>` package is staged with a placeholder
//! `inputs: []` / `outputs: []` manifest — the submit site can't know the
//! processor's ports without running its source. This tail closes that gap:
//! once the subprocess runtime is provisioned (Python venv / Deno
//! `_generated_`), it runs the language's import-and-enumerate extractor over
//! the staged source, then splices the real ports, execution mode, scheduling,
//! and description into the staged `streamlib.yaml` before the atomic rename
//! carries it into the package cache. The engine's registration path then mints
//! CONNECTABLE ports straight from that populated manifest.
//!
//! Extraction lives OUTSIDE the engine on purpose: it depends on
//! [`streamlib_processor_extract`] (which pulls the `#[processor]` grammar +
//! `syn`), a build-tool concern. The engine consumes only the populated
//! manifest; it never imports the extractor.
//!
//! EVERY extractor failure is a HARD [`BuildError`] — a session package must
//! never register silent empty (unconnectable) ports. Off-link Deno extraction
//! mirrors off-link Python: with no `STREAMLIB_DENO_EXTRACTOR` override and no
//! active `streamlib link`, the extractor is the npm-published SDK's
//! `extract_processors.ts` (pinned to the Deno SDK's own published version), run over the staged
//! `deno.json` — the direct analogue of running `.venv/bin/python -m
//! streamlib.extract_processors` off-link. A resolution that genuinely fails
//! (the `deno run npm:` fetch itself) surfaces as a hard [`BuildError`] rather
//! than a portless registration, just as Rust-from-source is refused.

use std::path::{Path, PathBuf};
use std::process::Command;

use streamlib_engine::core::runtime::BuildError;
use streamlib_processor_extract::{
    DeriveError, ExtractedManifestPort, ExtractedManifestProcessor, SubprocessProcessorExtractor,
    parse_subprocess_manifest_json_full,
};
use streamlib_processor_schema::{PortSchemaSpec, ProcessorPortSchema};

use crate::ActiveBuildLink;

/// Derive the real ports for a staged `@session/<name>` package and splice them
/// into its manifest. Constructs the real [`SessionSourceExtractor`] from the
/// staged venv + the active link, then runs the language-agnostic splice.
pub(crate) fn splice_session_manifest_ports(
    staged_dir: &Path,
    link: Option<&ActiveBuildLink>,
    package_label: &str,
) -> Result<(), BuildError> {
    let extractor = SessionSourceExtractor::for_staged(staged_dir, link);
    rewrite_staged_manifest_ports(staged_dir, package_label, &extractor)
}

/// The language-agnostic splice: detect which subprocess languages the staged
/// package carries, run each present language's extractor through `extractor`,
/// and rewrite the staged manifest's processors with the derived ports. The
/// `extractor` seam is injectable so the splice is unit-testable without a live
/// Python/Deno runtime.
fn rewrite_staged_manifest_ports(
    staged_dir: &Path,
    package_label: &str,
    extractor: &dyn SubprocessProcessorExtractor,
) -> Result<(), BuildError> {
    let mut extracted: Vec<ExtractedManifestProcessor> = Vec::new();

    // Python source is staged under `python/` (beside the pyproject); the
    // extractor scans the top-level `*.py` there.
    let python_dir = staged_dir.join("python");
    if python_dir.is_dir() {
        extracted.extend(run_language_extractor(
            "python",
            &python_dir,
            package_label,
            || extractor.extract_python(&python_dir),
        )?);
    }

    // Deno source is staged under `deno/`; the extractor scans the top-level
    // `*.ts` there.
    let deno_dir = staged_dir.join("deno");
    if deno_dir.is_dir() {
        extracted.extend(run_language_extractor(
            "deno",
            &deno_dir,
            package_label,
            || extractor.extract_deno(&deno_dir),
        )?);
    }

    if extracted.is_empty() {
        tracing::debug!(
            package = %package_label,
            "no session processors extracted — leaving the staged manifest's port placeholders"
        );
        return Ok(());
    }

    let manifest_path = staged_dir.join(streamlib_idents::Manifest::FILE_NAME);
    let body = std::fs::read_to_string(&manifest_path).map_err(|e| {
        build_failed(
            package_label,
            format!("read staged session manifest {}: {e}", manifest_path.display()),
        )
    })?;
    let mut manifest: serde_yaml::Value = serde_yaml::from_str(&body).map_err(|e| {
        build_failed(package_label, format!("parse staged session manifest: {e}"))
    })?;

    let spliced = apply_extracted_to_manifest(&mut manifest, &extracted, package_label)?;
    tracing::info!(
        package = %package_label,
        spliced,
        extracted = extracted.len(),
        "spliced live-extracted ports into staged session manifest"
    );

    let rewritten = serde_yaml::to_string(&manifest).map_err(|e| {
        build_failed(package_label, format!("re-serialize staged session manifest: {e}"))
    })?;
    std::fs::write(&manifest_path, rewritten).map_err(|e| {
        build_failed(
            package_label,
            format!("write spliced session manifest {}: {e}", manifest_path.display()),
        )
    })?;
    Ok(())
}

/// Run one language's extractor for a live-submitted SESSION package. Every
/// [`DeriveError`] — spawn / non-zero-exit / malformed-output — is a HARD
/// [`BuildError`]: a session submit must never register a silent portless
/// (unconnectable) processor. Off-link Deno resolves to the npm-published
/// extractor, so a genuine resolution failure surfaces as a non-zero `deno run`
/// exit ([`DeriveError::ExtractorFailed`]), still a hard [`BuildError`], rather
/// than shipping empty ports — the same refusal Rust-from-source gets.
fn run_language_extractor(
    language: &'static str,
    language_dir: &Path,
    package_label: &str,
    run: impl FnOnce() -> Result<String, DeriveError>,
) -> Result<Vec<ExtractedManifestProcessor>, BuildError> {
    let json = run().map_err(|e| map_extract_err(language, package_label, e))?;
    let procs = parse_subprocess_manifest_json_full(language, &json)
        .map_err(|e| map_extract_err(language, package_label, e))?;
    if procs.is_empty() {
        tracing::warn!(
            package = %package_label,
            language,
            dir = %language_dir.display(),
            "session extractor ran but registered no processors — the submitted \
             source declares none for this language; staged ports left empty"
        );
    }
    Ok(procs)
}

/// Splice each extracted processor's execution / scheduling / description /
/// ports onto the staged manifest processor of the same `Type` name, and
/// synthesize the `schemas:` / `dependencies:` maps that let the engine rebind
/// each port's bare `Named` schema ref to its fully-qualified `Specific` ident.
/// Returns the number of processors spliced.
fn apply_extracted_to_manifest(
    manifest: &mut serde_yaml::Value,
    extracted: &[ExtractedManifestProcessor],
    package_label: &str,
) -> Result<usize, BuildError> {
    // Bare `Type` → owning `@org/package`, and `@org/package` → its concrete
    // version, harvested from the extracted ports whose schema retains a full
    // ident. The staged port lines carry only the bare `Type`; without these
    // maps the engine's `resolve_bare_schema_refs` has nothing to rebind the
    // bare ref against and the port is left a dangling (unconnectable) `Named`.
    let mut schema_owner_by_type: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    let mut version_by_owner: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();

    let mut spliced = 0usize;
    {
        let processors = manifest
            .as_mapping_mut()
            .and_then(|m| m.get_mut(serde_yaml::Value::String("processors".to_string())))
            .and_then(|p| p.as_sequence_mut())
            .ok_or_else(|| {
                build_failed(
                    package_label,
                    "staged session manifest has no `processors:` sequence to splice into"
                        .to_string(),
                )
            })?;

        for entry in processors.iter_mut() {
            let Some(map) = entry.as_mapping_mut() else {
                continue;
            };
            let name = map
                .get(serde_yaml::Value::String("name".to_string()))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let Some(name) = name else { continue };

            // Match the staged processor to the extractor's projection by `Type`
            // short name — a session package stages exactly one processor per
            // submitted source, keyed by the minted type name.
            let Some(proc) = extracted
                .iter()
                .find(|p| p.schema_ident.r#type.as_str() == name)
            else {
                continue;
            };

            let execution = serde_yaml::to_value(&proc.execution)
                .map_err(|e| build_failed(package_label, format!("encode execution: {e}")))?;
            map.insert(serde_yaml::Value::String("execution".to_string()), execution);

            if let Some(scheduling) = &proc.scheduling {
                let scheduling = serde_yaml::to_value(scheduling)
                    .map_err(|e| build_failed(package_label, format!("encode scheduling: {e}")))?;
                map.insert(
                    serde_yaml::Value::String("scheduling".to_string()),
                    scheduling,
                );
            }
            if let Some(description) = &proc.description {
                map.insert(
                    serde_yaml::Value::String("description".to_string()),
                    serde_yaml::Value::String(description.clone()),
                );
            }

            map.insert(
                serde_yaml::Value::String("inputs".to_string()),
                ports_to_yaml(&proc.inputs, true, package_label)?,
            );
            map.insert(
                serde_yaml::Value::String("outputs".to_string()),
                ports_to_yaml(&proc.outputs, false, package_label)?,
            );

            for port in proc.inputs.iter().chain(proc.outputs.iter()) {
                if let Some(ident) = &port.schema {
                    collect_schema_binding(&mut schema_owner_by_type, &mut version_by_owner, ident);
                }
            }
            spliced += 1;
        }
    }

    splice_schema_bindings(manifest, &schema_owner_by_type, &version_by_owner, package_label)?;
    Ok(spliced)
}

/// Record the `schemas:`/`dependencies:` binding a single extracted port's
/// schema ident implies: the bare `Type` maps to its owning `@org/package`,
/// which pins to the ident's concrete version. A `@session/<name>`-owned type
/// is skipped — it names no external dependency to import from.
fn collect_schema_binding(
    schema_owner_by_type: &mut std::collections::BTreeMap<String, String>,
    version_by_owner: &mut std::collections::BTreeMap<String, String>,
    ident: &streamlib_processor_schema::SchemaIdent,
) {
    if ident.org.as_str() == streamlib_idents::SESSION_ORG {
        return;
    }
    let owner =
        streamlib_idents::PackageRef::new(ident.org.clone(), ident.package.clone()).to_string();
    schema_owner_by_type.insert(ident.r#type.as_str().to_string(), owner.clone());
    version_by_owner.insert(owner, ident.version.to_string());
}

/// Merge the synthesized `schemas:` (bare `Type` → `{ package: @org/pkg }`
/// External imports) and `dependencies:` (`@org/pkg` → `{ version }`) maps into
/// the staged manifest, so the engine's bare-name schema resolution rebinds
/// each port's `Named` ref to a `Specific` ident. Existing entries are
/// preserved; the synthesized bindings never clobber a hand-authored key.
fn splice_schema_bindings(
    manifest: &mut serde_yaml::Value,
    schema_owner_by_type: &std::collections::BTreeMap<String, String>,
    version_by_owner: &std::collections::BTreeMap<String, String>,
    package_label: &str,
) -> Result<(), BuildError> {
    if schema_owner_by_type.is_empty() {
        return Ok(());
    }
    let root = manifest.as_mapping_mut().ok_or_else(|| {
        build_failed(
            package_label,
            "staged session manifest is not a mapping".to_string(),
        )
    })?;

    let schemas = mapping_entry(root, "schemas");
    for (type_name, owner) in schema_owner_by_type {
        let mut entry = serde_yaml::Mapping::new();
        entry.insert(
            serde_yaml::Value::String("package".to_string()),
            serde_yaml::Value::String(owner.clone()),
        );
        schemas
            .entry(serde_yaml::Value::String(type_name.clone()))
            .or_insert(serde_yaml::Value::Mapping(entry));
    }

    let dependencies = mapping_entry(root, "dependencies");
    for (owner, version) in version_by_owner {
        let mut entry = serde_yaml::Mapping::new();
        entry.insert(
            serde_yaml::Value::String("version".to_string()),
            serde_yaml::Value::String(version.clone()),
        );
        dependencies
            .entry(serde_yaml::Value::String(owner.clone()))
            .or_insert(serde_yaml::Value::Mapping(entry));
    }
    Ok(())
}

/// Borrow (creating if absent) a nested `serde_yaml` mapping under `key` in the
/// manifest root.
fn mapping_entry<'a>(
    root: &'a mut serde_yaml::Mapping,
    key: &str,
) -> &'a mut serde_yaml::Mapping {
    let key = serde_yaml::Value::String(key.to_string());
    let slot = root
        .entry(key)
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if !slot.is_mapping() {
        *slot = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    slot.as_mapping_mut()
        .expect("slot was just ensured to be a mapping")
}

/// Project a port list onto its manifest YAML sequence. `is_input` gates the
/// `delivery_profile` override (output ports never carry one).
fn ports_to_yaml(
    ports: &[ExtractedManifestPort],
    is_input: bool,
    package_label: &str,
) -> Result<serde_yaml::Value, BuildError> {
    let mut seq = Vec::with_capacity(ports.len());
    for port in ports {
        // The manifest use-site references a schema by bare `Type` name (or the
        // `any` wildcard); the full ident's org/package/version live in the
        // package's `schemas:`/dependency resolution, not the port line.
        let schema = match &port.schema {
            Some(ident) => PortSchemaSpec::Named(ident.r#type.clone()),
            None => PortSchemaSpec::Any,
        };
        let manifest_port = ProcessorPortSchema {
            name: port.name.clone(),
            schema,
            description: port.description.clone(),
            delivery_profile: if is_input {
                port.delivery_profile.clone()
            } else {
                None
            },
        };
        seq.push(
            serde_yaml::to_value(&manifest_port)
                .map_err(|e| build_failed(package_label, format!("encode port: {e}")))?,
        );
    }
    Ok(serde_yaml::Value::Sequence(seq))
}

fn map_extract_err(language: &str, package_label: &str, err: DeriveError) -> BuildError {
    build_failed(
        package_label,
        format!("session {language} port extraction failed: {err}"),
    )
}

fn build_failed(package: &str, detail: String) -> BuildError {
    BuildError::BuildFailed {
        tool: "session-ports".to_string(),
        package: package.to_string(),
        detail,
    }
}

/// How the Deno processor extractor for a staged session package is resolved,
/// in priority order: an explicit `STREAMLIB_DENO_EXTRACTOR` override or the
/// linked checkout's `extract_processors.ts` (both a local `.ts` script on disk)
/// win; off-link with neither, the extractor is the npm-published SDK's
/// `extract_processors.ts` pinned to the Deno SDK's own published version — the
/// exact mirror of how off-link Python runs `.venv/bin/python -m
/// streamlib.extract_processors`.
#[derive(Debug, PartialEq, Eq)]
enum DenoExtractorSource {
    /// A local `extract_processors.ts` on disk (the `STREAMLIB_DENO_EXTRACTOR`
    /// override, or the active link's checkout).
    LocalScript(PathBuf),
    /// The npm-published `@tatolab/streamlib-deno@<sdk_version>/extract_processors.ts`,
    /// resolved (off-link) through the staged `deno.json` import map.
    PublishedNpm { sdk_version: String },
}

/// Resolve the Deno extractor source for a staged session package: the
/// `STREAMLIB_DENO_EXTRACTOR` override wins, then the linked checkout's
/// `extract_processors.ts` sibling, and off-link with neither the npm-published
/// SDK pinned to `sdk_version`. Pure over its inputs so the resolution priority
/// is unit-testable without mutating the process env.
fn resolve_deno_extractor_source(
    env_override: Option<PathBuf>,
    link: Option<&ActiveBuildLink>,
    sdk_version: &str,
) -> DenoExtractorSource {
    if let Some(script) = env_override {
        return DenoExtractorSource::LocalScript(script);
    }
    if let Some(linked_script) = link.and_then(|l| {
        l.deno_sdk_entrypoint_path
            .parent()
            .map(|dir| dir.join("extract_processors.ts"))
    }) {
        return DenoExtractorSource::LocalScript(linked_script);
    }
    DenoExtractorSource::PublishedNpm {
        sdk_version: sdk_version.to_string(),
    }
}

/// The build orchestrator's real [`SubprocessProcessorExtractor`]: spawns the
/// staged package's provisioned venv Python (`.venv/bin/python -m
/// streamlib.extract_processors <dir>`) and, for Deno, the SDK's
/// `extract_processors.ts` — resolved from the `STREAMLIB_DENO_EXTRACTOR`
/// override, the active link's checkout, or (off-link) the npm-published SDK by
/// pinned version — run against the staged `deno.json` import map so the
/// source's `streamlib` specifier resolves.
struct SessionSourceExtractor {
    venv_python: PathBuf,
    deno_binary: String,
    deno_extractor_source: DenoExtractorSource,
    deno_config: PathBuf,
}

impl SessionSourceExtractor {
    fn for_staged(staged_dir: &Path, link: Option<&ActiveBuildLink>) -> Self {
        #[cfg(unix)]
        let venv_python = staged_dir.join(".venv").join("bin").join("python");
        #[cfg(windows)]
        let venv_python = staged_dir.join(".venv").join("Scripts").join("python.exe");

        let deno_extractor_source = resolve_deno_extractor_source(
            std::env::var_os("STREAMLIB_DENO_EXTRACTOR").map(PathBuf::from),
            link,
            env!("STREAMLIB_DENO_SDK_VERSION"),
        );

        Self {
            venv_python,
            deno_binary: std::env::var("STREAMLIB_DENO").unwrap_or_else(|_| "deno".to_string()),
            deno_extractor_source,
            deno_config: staged_dir.join("deno.json"),
        }
    }
}

/// Run a spawned extractor command, mapping spawn / non-zero-exit into the
/// [`DeriveError`] taxonomy the shared splice consumes.
fn run_extractor_command(
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

impl SubprocessProcessorExtractor for SessionSourceExtractor {
    fn extract_python(&self, package_dir: &Path) -> Result<String, DeriveError> {
        let mut command = Command::new(&self.venv_python);
        command
            .arg("-m")
            .arg("streamlib.extract_processors")
            .arg(package_dir);
        run_extractor_command("python", package_dir, command)
    }

    fn extract_deno(&self, package_dir: &Path) -> Result<String, DeriveError> {
        let mut command = Command::new(&self.deno_binary);
        command
            .arg("run")
            .arg("--allow-all")
            .arg("--config")
            .arg(&self.deno_config);
        match &self.deno_extractor_source {
            DenoExtractorSource::LocalScript(script) => {
                command.arg(script);
            }
            DenoExtractorSource::PublishedNpm { sdk_version } => {
                command.arg(format!(
                    "npm:@tatolab/streamlib-deno@{sdk_version}/extract_processors.ts"
                ));
            }
        }
        command.arg(package_dir);
        run_extractor_command("deno", package_dir, command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canned-JSON extractor so the splice is exercised without a live
    /// Python/Deno runtime (the FakeExtractor pattern).
    struct FakeExtractor {
        python: Result<String, DeriveError>,
        deno: Result<String, DeriveError>,
    }
    impl FakeExtractor {
        fn python_only(json: &str) -> Self {
            Self {
                python: Ok(json.to_string()),
                deno: Ok("[]".to_string()),
            }
        }
    }
    impl SubprocessProcessorExtractor for FakeExtractor {
        fn extract_python(&self, _dir: &Path) -> Result<String, DeriveError> {
            self.python
                .as_ref()
                .map(String::clone)
                .map_err(clone_derive_err)
        }
        fn extract_deno(&self, _dir: &Path) -> Result<String, DeriveError> {
            self.deno.as_ref().map(String::clone).map_err(clone_derive_err)
        }
    }

    fn clone_derive_err(err: &DeriveError) -> DeriveError {
        // DeriveError isn't Clone; reconstruct the variants the tests use.
        match err {
            DeriveError::ExtractorFailed {
                language,
                package,
                code,
                stderr,
            } => DeriveError::ExtractorFailed {
                language,
                package: package.clone(),
                code: code.clone(),
                stderr: stderr.clone(),
            },
            DeriveError::ExtractorUnconfigured {
                language,
                package,
                hint,
            } => DeriveError::ExtractorUnconfigured {
                language,
                package: package.clone(),
                hint: hint.clone(),
            },
            _ => DeriveError::MalformedExtractorJson {
                language: "python",
                source: serde_json::from_str::<serde_json::Value>("x").unwrap_err(),
            },
        }
    }

    /// Stage a placeholder `@session/<name>` package: a Python source subdir and
    /// the portless manifest `stage_submitted_source` writes.
    fn stage_placeholder_python(dir: &Path, type_name: &str) {
        std::fs::create_dir_all(dir.join("python")).unwrap();
        std::fs::write(dir.join("python").join("widget.py"), "class Widget: pass\n").unwrap();
        std::fs::write(
            dir.join("streamlib.yaml"),
            format!(
                "package:\n  org: session\n  name: widget\n  version: \"0.0.1\"\n\
                 processors:\n  - name: {type_name}\n    description: live-submitted session processor\n    \
                 runtime: python\n    execution: manual\n    entrypoint: \"widget:{type_name}\"\n    \
                 inputs: []\n    outputs: []\n"
            ),
        )
        .unwrap();
    }

    fn full_json(type_name: &str) -> String {
        format!(
            r#"[{{
              "name": "{type_name}",
              "schema_ident": {{"org":"session","package":"widget","type":"{type_name}","version":"0.0.1"}},
              "execution": "reactive",
              "scheduling": {{"priority":"high"}},
              "description": "extracted description",
              "inputs": [
                {{"name":"in0","schema":null,"description":"an input","delivery_profile":"latest"}}
              ],
              "outputs": [
                {{"name":"out0","schema":{{"org":"tatolab","package":"core","type":"VideoFrame","version":"0.0.0"}},"description":null}}
              ]
            }}]"#
        )
    }

    #[test]
    fn splice_rewrites_placeholder_manifest_with_real_ports() {
        // The injectable-splice lock: with canned full-fidelity extractor JSON,
        // the staged `inputs: []` / `outputs: []` placeholders become real ports,
        // and execution / scheduling / description are rewritten from the
        // extracted surface. Mentally-revert the splice and the manifest keeps
        // its empty placeholders → a portless (unconnectable) session processor.
        let dir = tempfile::tempdir().unwrap();
        stage_placeholder_python(dir.path(), "Widget");
        let extractor = FakeExtractor::python_only(&full_json("Widget"));

        rewrite_staged_manifest_ports(dir.path(), "session/widget", &extractor)
            .expect("splice must succeed");

        let cfg: streamlib_processor_schema::ProjectConfigMinimal = serde_yaml::from_str(
            &std::fs::read_to_string(dir.path().join("streamlib.yaml")).unwrap(),
        )
        .expect("spliced manifest must re-parse");
        assert_eq!(cfg.processors.len(), 1);
        let proc = &cfg.processors[0];
        assert_eq!(proc.name, "Widget");
        assert_eq!(
            proc.execution,
            streamlib_processor_schema::ProcessorSchemaExecution::Reactive
        );
        assert_eq!(
            proc.scheduling.map(|s| s.priority),
            Some(streamlib_processor_schema::ThreadPriority::High)
        );
        assert_eq!(proc.description.as_deref(), Some("extracted description"));
        assert_eq!(proc.inputs.len(), 1, "the placeholder input must be spliced");
        assert_eq!(proc.inputs[0].name, "in0");
        assert_eq!(proc.inputs[0].delivery_profile.as_deref(), Some("latest"));
        assert_eq!(proc.outputs.len(), 1, "the placeholder output must be spliced");
        assert_eq!(proc.outputs[0].name, "out0");
        assert_eq!(proc.outputs[0].schema.to_string(), "VideoFrame");
    }

    #[test]
    fn extractor_failure_is_a_hard_build_error_not_empty_ports() {
        // Contract lock: a spawn / non-zero-exit failure must fail the whole
        // materialize, never leave the placeholder ports silently empty.
        let dir = tempfile::tempdir().unwrap();
        stage_placeholder_python(dir.path(), "Widget");
        let extractor = FakeExtractor {
            python: Err(DeriveError::ExtractorFailed {
                language: "python",
                package: dir.path().join("python"),
                code: "1".to_string(),
                stderr: "No module named 'streamlib'".to_string(),
            }),
            deno: Ok("[]".to_string()),
        };
        let err = rewrite_staged_manifest_ports(dir.path(), "session/widget", &extractor)
            .expect_err("a failed extractor must hard-fail the splice");
        assert!(matches!(err, BuildError::BuildFailed { .. }), "got {err:?}");
    }

    #[test]
    fn deno_extractor_error_hard_fails_the_splice() {
        // Contract lock: a Deno extractor `DeriveError` (here an unconfigured
        // extractor) hard-fails the whole splice when a `deno/` dir is staged —
        // a session submit never registers silent portless (unconnectable) ports.
        // Mentally-revert the DeriveError→hard-`BuildError` mapping and this
        // yields an Ok with empty placeholder ports. (Off-link resolution itself
        // now falls through to the npm-published extractor — see
        // `deno_extractor_source_prefers_override_then_link_then_npm`.)
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("deno")).unwrap();
        std::fs::write(dir.path().join("deno").join("widget.ts"), "export class Widget {}\n")
            .unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: session\n  name: widget\n  version: \"0.0.1\"\n\
             processors:\n  - name: Widget\n    description: d\n    runtime: deno\n    \
             execution: manual\n    entrypoint: \"widget.ts:Widget\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        let extractor = FakeExtractor {
            python: Ok("[]".to_string()),
            deno: Err(DeriveError::ExtractorUnconfigured {
                language: "deno",
                package: dir.path().join("deno"),
                hint: "set STREAMLIB_DENO_EXTRACTOR or run under `streamlib link`".to_string(),
            }),
        };
        let err = rewrite_staged_manifest_ports(dir.path(), "session/widget", &extractor)
            .expect_err("an unconfigured deno extractor must hard-fail the session submit");
        assert!(matches!(err, BuildError::BuildFailed { .. }), "got {err:?}");
    }

    #[test]
    fn deno_extractor_source_prefers_override_then_link_then_npm() {
        // Resolution-priority lock for off-link Deno extraction: an explicit
        // `STREAMLIB_DENO_EXTRACTOR` override wins, then the linked checkout's
        // `extract_processors.ts` sibling, and off-link with neither the
        // extractor is the npm-published SDK pinned to the Deno SDK's own
        // published version — the mirror of off-link Python's `.venv/bin/python -m
        // streamlib.extract_processors`. Mentally-revert the npm tier and the
        // `neither` case has no extractor (the old hard-refusal / portless gap).
        let override_script = PathBuf::from("/opt/custom/extract_processors.ts");
        assert_eq!(
            resolve_deno_extractor_source(Some(override_script.clone()), None, "9.9.9"),
            DenoExtractorSource::LocalScript(override_script),
            "an explicit STREAMLIB_DENO_EXTRACTOR override must win",
        );

        let link = ActiveBuildLink {
            checkout: PathBuf::from("/co"),
            consumer_cargo_config: None,
            python_sdk_path: PathBuf::from("/co/sdk/streamlib-python"),
            deno_sdk_entrypoint_path: PathBuf::from("/co/sdk/streamlib-deno/mod.ts"),
        };
        assert_eq!(
            resolve_deno_extractor_source(None, Some(&link), "9.9.9"),
            DenoExtractorSource::LocalScript(PathBuf::from(
                "/co/sdk/streamlib-deno/extract_processors.ts"
            )),
            "an active link must resolve the checkout's extract_processors.ts sibling",
        );

        assert_eq!(
            resolve_deno_extractor_source(None, None, "9.9.9"),
            DenoExtractorSource::PublishedNpm {
                sdk_version: "9.9.9".to_string()
            },
            "off-link with no override must resolve the npm-published SDK by pinned version",
        );

        // The override outranks a present link.
        assert_eq!(
            resolve_deno_extractor_source(
                Some(PathBuf::from("/env/extract_processors.ts")),
                Some(&link),
                "9.9.9"
            ),
            DenoExtractorSource::LocalScript(PathBuf::from("/env/extract_processors.ts")),
            "the env override must outrank a present link",
        );
    }

    #[test]
    #[serial_test::serial]
    fn real_extractor_spawns_python3_and_parses_manifest_json() {
        // Real-extractor subprocess lock: the actual
        // `SessionSourceExtractor::extract_python` path — spawn python3, run
        // `-m streamlib.extract_processors <dir>`, capture stdout — is exercised
        // end-to-end. Subprocess spawn is sandbox-OK (exit 144 is GPU/IPC only).
        // Gated on `python3` being present, like `streamlib-pack`'s subprocess
        // tests. A hermetic fake `streamlib.extract_processors` module (planted
        // on PYTHONPATH) stands in for the SDK's, so the test doesn't require a
        // provisioned venv; it still drives the real method and asserts the
        // spawn → stdout → full-fidelity-parse pipeline is wired.
        let Some(python3) = which_python3() else {
            eprintln!("skipping: `python3` not on PATH");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("python");
        std::fs::create_dir_all(&pkg).unwrap();

        let sitedir = dir.path().join("site");
        let streamlib_pkg = sitedir.join("streamlib");
        std::fs::create_dir_all(&streamlib_pkg).unwrap();
        std::fs::write(streamlib_pkg.join("__init__.py"), "").unwrap();
        std::fs::write(
            streamlib_pkg.join("extract_processors.py"),
            "import json\n\
             print(json.dumps([{\n\
             \"name\": \"Widget\",\n\
             \"schema_ident\": {\"org\": \"session\", \"package\": \"widget\", \"type\": \"Widget\", \"version\": \"0.0.1\"},\n\
             \"execution\": \"reactive\", \"scheduling\": None, \"description\": None,\n\
             \"inputs\": [], \"outputs\": []}]))\n",
        )
        .unwrap();

        let extractor = SessionSourceExtractor {
            venv_python: python3,
            deno_binary: "deno".to_string(),
            deno_extractor_source: DenoExtractorSource::PublishedNpm {
                sdk_version: env!("STREAMLIB_DENO_SDK_VERSION").to_string(),
            },
            deno_config: dir.path().join("deno.json"),
        };

        // The extractor spawns the interpreter inheriting the process env, so
        // point PYTHONPATH at the fake site dir for the duration of the call.
        // SAFETY: `#[serial]` serializes the process-global env mutation, and no
        // concurrent test spawns a Python interpreter that would observe it.
        let prev = std::env::var_os("PYTHONPATH");
        unsafe { std::env::set_var("PYTHONPATH", &sitedir) };
        let result = extractor.extract_python(&pkg);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("PYTHONPATH", v),
                None => std::env::remove_var("PYTHONPATH"),
            }
        }

        let json = result.expect("real python extractor must run against the fixture");
        let procs = parse_subprocess_manifest_json_full("python", &json)
            .expect("fixture output must parse as full-fidelity manifest JSON");
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].schema_ident.r#type.as_str(), "Widget");
    }

    fn which_python3() -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("python3");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    /// A full-fidelity extractor JSON whose OUTPUT port carries a full
    /// `@tatolab/core/VideoFrame@1.0.0` schema ident — the shape the splice
    /// harvests into the synthesized `schemas:` / `dependencies:` maps.
    fn json_with_core_output_port() -> String {
        r#"[{
          "name": "Widget",
          "schema_ident": {"org":"session","package":"widget","type":"Widget","version":"0.0.1"},
          "execution": "reactive",
          "scheduling": null,
          "description": null,
          "inputs": [],
          "outputs": [
            {"name":"out0","schema":{"org":"tatolab","package":"core","type":"VideoFrame","version":"1.0.0"},"description":null}
          ]
        }]"#
        .to_string()
    }

    /// Write `<checkout>/packages/core` declaring `VideoFrame` as a Local schema
    /// — the owning package the synthesized `@tatolab/core` dep resolves to
    /// under a link.
    fn write_checkout_core(checkout: &Path) {
        let core = checkout.join("packages").join("core");
        std::fs::create_dir_all(core.join("schemas")).unwrap();
        std::fs::write(
            core.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: core\n  version: 1.0.0\nschemas:\n  VideoFrame:\n    file: schemas/video_frame.yaml\n",
        )
        .unwrap();
        std::fs::write(
            core.join("schemas/video_frame.yaml"),
            "metadata:\n  type: VideoFrame\nproperties: {}\n",
        )
        .unwrap();
    }

    /// Clear the resolution env so the load resolves the synthesized dep from
    /// the checkout only (zero registry). Restores prior values on drop.
    struct CleanResolutionEnv {
        prev_registry: Option<String>,
        prev_link: Option<String>,
    }
    impl CleanResolutionEnv {
        fn new() -> Self {
            let prev_registry = std::env::var("STREAMLIB_REGISTRY_URL").ok();
            let prev_link = std::env::var("STREAMLIB_LINK_CHECKOUT").ok();
            // SAFETY: `#[serial]` serializes the process-global env mutation.
            unsafe {
                std::env::remove_var("STREAMLIB_REGISTRY_URL");
                std::env::remove_var("STREAMLIB_LINK_CHECKOUT");
            }
            Self {
                prev_registry,
                prev_link,
            }
        }
    }
    impl Drop for CleanResolutionEnv {
        fn drop(&mut self) {
            unsafe {
                match self.prev_registry.take() {
                    Some(v) => std::env::set_var("STREAMLIB_REGISTRY_URL", v),
                    None => std::env::remove_var("STREAMLIB_REGISTRY_URL"),
                }
                match self.prev_link.take() {
                    Some(v) => std::env::set_var("STREAMLIB_LINK_CHECKOUT", v),
                    None => std::env::remove_var("STREAMLIB_LINK_CHECKOUT"),
                }
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn extracted_schema_port_rebinds_to_a_specific_ident() {
        // Schema-rebind lock: an extracted port carrying a full schema ident
        // must yield a synthesized `schemas:` + `dependencies:` pair so the
        // engine's bare-name resolution rebinds the staged bare `Named` port to
        // a `Specific` FQ ident. Mentally-revert `splice_schema_bindings` and the
        // port stays a dangling bare `Named` — the engine load then fails to
        // resolve it (unconnectable, can't size an iceoryx2 slot).
        use streamlib_engine::core::ProjectConfig;
        use streamlib_processor_schema::PortSchemaSpec;

        let _env = CleanResolutionEnv::new();
        let staged = tempfile::tempdir().unwrap();
        stage_placeholder_python(staged.path(), "Widget");
        let extractor = FakeExtractor::python_only(&json_with_core_output_port());
        rewrite_staged_manifest_ports(staged.path(), "session/widget", &extractor)
            .expect("splice must succeed");

        // The synthesized maps are present in the rewritten manifest.
        let body = std::fs::read_to_string(staged.path().join("streamlib.yaml")).unwrap();
        assert!(
            body.contains("VideoFrame") && body.contains("@tatolab/core"),
            "the spliced manifest must synthesize a schemas/dependencies binding: {body}"
        );

        // The engine resolver rebinds the bare `Named` port to `Specific`,
        // resolving `@tatolab/core` from the checkout with zero registry.
        let checkout = tempfile::tempdir().unwrap();
        write_checkout_core(checkout.path());
        let config = ProjectConfig::load_with_link(staged.path(), Some(checkout.path()))
            .expect("the spliced manifest must load and rebind its bare schema ref");
        let port = &config.processors[0].outputs[0];
        match &port.schema {
            PortSchemaSpec::Specific(ident) => {
                assert_eq!(ident.to_string(), "@tatolab/core/VideoFrame@1.0.0");
            }
            other => panic!("extracted port must rebind to a Specific ident, got {other:?}"),
        }
    }
}
