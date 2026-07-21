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
//! A spawn / non-zero-exit / malformed-output failure is a HARD
//! [`BuildError`] — a session package must never register silent empty ports.
//! The single bounded fallback is [`DeriveError::ExtractorUnconfigured`] (the
//! Deno extractor script couldn't be located off a link and with no env
//! override): that language is skipped with a warning rather than failing the
//! whole materialize.

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
    if python_dir.is_dir()
        && let Some(procs) = run_language_extractor("python", &python_dir, package_label, || {
            extractor.extract_python(&python_dir)
        })?
    {
        extracted.extend(procs);
    }

    // Deno source is staged under `deno/`; the extractor scans the top-level
    // `*.ts` there.
    let deno_dir = staged_dir.join("deno");
    if deno_dir.is_dir()
        && let Some(procs) = run_language_extractor("deno", &deno_dir, package_label, || {
            extractor.extract_deno(&deno_dir)
        })?
    {
        extracted.extend(procs);
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

/// Run one language's extractor, mapping the [`DeriveError`] taxonomy onto the
/// hard-vs-bounded contract: a spawn / non-zero-exit / malformed-output failure
/// is a hard [`BuildError`]; [`DeriveError::ExtractorUnconfigured`] is the sole
/// bounded fallback (that language is skipped, `Ok(None)`).
fn run_language_extractor(
    language: &'static str,
    language_dir: &Path,
    package_label: &str,
    run: impl FnOnce() -> Result<String, DeriveError>,
) -> Result<Option<Vec<ExtractedManifestProcessor>>, BuildError> {
    match run() {
        Ok(json) => {
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
            Ok(Some(procs))
        }
        Err(DeriveError::ExtractorUnconfigured { hint, .. }) => {
            tracing::warn!(
                package = %package_label,
                language,
                hint,
                "session port extraction skipped: no extractor configured for this language"
            );
            Ok(None)
        }
        Err(other) => Err(map_extract_err(language, package_label, other)),
    }
}

/// Splice each extracted processor's execution / scheduling / description /
/// ports onto the staged manifest processor of the same `Type` name. Returns
/// the number of processors spliced.
fn apply_extracted_to_manifest(
    manifest: &mut serde_yaml::Value,
    extracted: &[ExtractedManifestProcessor],
    package_label: &str,
) -> Result<usize, BuildError> {
    let processors = manifest
        .as_mapping_mut()
        .and_then(|m| m.get_mut(serde_yaml::Value::String("processors".to_string())))
        .and_then(|p| p.as_sequence_mut())
        .ok_or_else(|| {
            build_failed(
                package_label,
                "staged session manifest has no `processors:` sequence to splice into".to_string(),
            )
        })?;

    let mut spliced = 0usize;
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
        spliced += 1;
    }
    Ok(spliced)
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

/// The build orchestrator's real [`SubprocessProcessorExtractor`]: spawns the
/// staged package's provisioned venv Python (`.venv/bin/python -m
/// streamlib.extract_processors <dir>`) and, for Deno, the SDK's
/// `extract_processors.ts` resolved off the active link (or the
/// `STREAMLIB_DENO_EXTRACTOR` override), running it against the staged
/// `deno.json` import map so the source's `streamlib` specifier resolves.
struct SessionSourceExtractor {
    venv_python: PathBuf,
    deno_binary: String,
    deno_extractor_script: Option<PathBuf>,
    deno_config: PathBuf,
}

impl SessionSourceExtractor {
    fn for_staged(staged_dir: &Path, link: Option<&ActiveBuildLink>) -> Self {
        #[cfg(unix)]
        let venv_python = staged_dir.join(".venv").join("bin").join("python");
        #[cfg(windows)]
        let venv_python = staged_dir.join(".venv").join("Scripts").join("python.exe");

        // Resolve the Deno extractor script: an explicit override wins, else the
        // linked checkout's Deno SDK entrypoint sibling. With neither, Deno
        // extraction is unconfigured (the bounded fallback).
        let deno_extractor_script = std::env::var_os("STREAMLIB_DENO_EXTRACTOR")
            .map(PathBuf::from)
            .or_else(|| {
                link.and_then(|l| {
                    l.deno_sdk_entrypoint_path
                        .parent()
                        .map(|dir| dir.join("extract_processors.ts"))
                })
            });

        Self {
            venv_python,
            deno_binary: std::env::var("STREAMLIB_DENO").unwrap_or_else(|_| "deno".to_string()),
            deno_extractor_script,
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
        let Some(script) = &self.deno_extractor_script else {
            return Err(DeriveError::ExtractorUnconfigured {
                language: "deno",
                package: package_dir.to_path_buf(),
                hint: "set STREAMLIB_DENO_EXTRACTOR to the Deno SDK's extract_processors.ts, \
                       or run under an active `streamlib link`"
                    .to_string(),
            });
        };
        let mut command = Command::new(&self.deno_binary);
        command
            .arg("run")
            .arg("--allow-all")
            .arg("--config")
            .arg(&self.deno_config)
            .arg(script)
            .arg(package_dir);
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
                .map(|s| s.clone())
                .map_err(clone_derive_err)
        }
        fn extract_deno(&self, _dir: &Path) -> Result<String, DeriveError> {
            self.deno.as_ref().map(|s| s.clone()).map_err(clone_derive_err)
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
    fn deno_unconfigured_is_a_bounded_skip() {
        // A staged Deno session package whose extractor script can't be located
        // (no link, no env override) is skipped with a warning, not failed —
        // the single bounded fallback. The placeholder ports remain.
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
                hint: "no script".to_string(),
            }),
        };
        rewrite_staged_manifest_ports(dir.path(), "session/widget", &extractor)
            .expect("unconfigured deno must be a bounded skip, not a hard error");

        let body = std::fs::read_to_string(dir.path().join("streamlib.yaml")).unwrap();
        assert!(
            body.contains("inputs: []") && body.contains("outputs: []"),
            "placeholder ports must remain untouched when extraction is skipped: {body}"
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
            deno_extractor_script: None,
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
}
