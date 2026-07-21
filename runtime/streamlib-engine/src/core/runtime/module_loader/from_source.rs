// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! [`Runner::register_processor_from_source`] — register a processor
//! definition submitted as source text into a live runtime.
//!
//! Live source submit is the disk-backed sibling of [`super::session`]'s
//! `add_local` (which registers an already-compiled host type). The submitted
//! source is staged into a `@session/<name>@0.0.N` package directory, then
//! driven through the SAME transactional [`Runner::add_module_with`] /
//! [`Strategy::Path`] staging → build → commit → [`ledger`] seam a disk-backed
//! module load uses — so the build orchestrator provisions the subprocess
//! runtime (Python venv / Deno `_generated_/`), derives the manifest from the
//! staged source, and a failed submit drops the staging with zero registry
//! residue. [`Runner::remove_module`] unregisters it symmetrically.
//!
//! Live submit covers the subprocess languages (Python / TypeScript): they run
//! from source with no host compile. Rust-from-source at runtime is a full
//! cargo build — the `streamlib pkg build` flow, not a live graph mutation —
//! and is refused here with a typed error.
//!
//! [`Runner::register_processor_from_source`]: super::super::Runner::register_processor_from_source
//! [`Runner::add_module_with`]: super::super::Runner::add_module_with
//! [`Runner::remove_module`]: super::super::Runner::remove_module
//! [`Strategy::Path`]: super::Strategy::Path
//! [`ledger`]: super::ledger

use std::path::{Path, PathBuf};

use streamlib_processor_schema::ProcessorLanguage;

use super::super::Runner;
use super::super::operations::{ReplaceProcessorFromSource, SubmittedProcessorSource};
use super::build_orchestrator::BuildPolicy;
use super::errors::AddModuleError;
use super::ledger;
use super::session::kebab_case;
use super::source::Strategy;
use crate::core::error::{Error, Result};

/// The per-language staging profile for a live source submit: the manifest
/// runtime key, the staged source file's relative path (given the module
/// stem), the subprocess entrypoint (given stem + type name), and the
/// subprocess dependency-resolution artifacts staged beside the source so the
/// build orchestrator's venv / Deno provisioning has a project to work from.
/// A single `match request.language` yields one of these; Rust returns the
/// unsupported-language refusal instead.
struct LiveSubmitLanguage {
    runtime_key: &'static str,
    source_rel: fn(module_stem: &str) -> String,
    entrypoint: fn(module_stem: &str, type_name: &str) -> String,
    /// `(relative_path, contents)` for the dependency-resolution artifacts a
    /// build of this language needs — a `pyproject.toml` declaring the
    /// `streamlib` SDK dep (Python), a `deno.json` import map (TypeScript).
    /// Without these the orchestrator's provision tail has no project to
    /// resolve the SDK from and hard-fails.
    dep_artifacts: fn(package_name: &str, version: streamlib_idents::SemVer) -> Vec<(String, String)>,
}

/// A submitted-source package staged to disk, ready to load through
/// [`Strategy::Path`].
#[derive(Debug)]
struct StagedSessionSource {
    module: streamlib_idents::ModuleIdent,
    dir: PathBuf,
}

impl Runner {
    /// Register a processor definition submitted as source text into the live
    /// runtime, minting it a `@session/<name>@0.0.N` identity. The source is
    /// staged to disk and loaded through the transactional
    /// [`Runner::add_module_with`] / [`Strategy::Path`] seam; a failed submit
    /// leaves zero registry residue. Returns the minted registration
    /// [`ModuleIdent`](streamlib_idents::ModuleIdent) — a registration ident,
    /// NOT an `add_processor` instance id.
    ///
    /// [`Runner::add_module_with`]: Self::add_module_with
    /// [`Strategy::Path`]: super::Strategy::Path
    #[tracing::instrument(
        skip(self, request),
        fields(language = ?request.language, name = ?request.requested_name),
    )]
    pub async fn register_processor_from_source(
        &self,
        request: SubmittedProcessorSource,
    ) -> Result<streamlib_idents::ModuleIdent> {
        let staged = stage_submitted_source(&request)?;
        self.load_staged_session_source(staged).await
    }

    /// Load an already-staged session source through the transactional
    /// [`Runner::add_module_with`] / [`Strategy::Path`] seam. The staging half
    /// of [`Self::register_processor_from_source`], split out as the shared
    /// seam the transactional [`Self::replace_processor_from_source`] pre-stages
    /// through. On load failure the staged scratch dir (and its now-empty parent)
    /// is reclaimed — the module_loader already discarded its registration
    /// staging, so this only cleans the filesystem.
    ///
    /// [`Runner::add_module_with`]: Self::add_module_with
    /// [`Strategy::Path`]: super::Strategy::Path
    async fn load_staged_session_source(
        &self,
        staged: StagedSessionSource,
    ) -> Result<streamlib_idents::ModuleIdent> {
        let StagedSessionSource { module, dir } = staged;
        let added = self.add_module_with(
            module.clone(),
            Strategy::Path {
                path: dir.clone(),
                build: BuildPolicy::IfStale,
            },
        );
        match added.await {
            Ok(_loaded) => Ok(module),
            Err(load_error) => {
                remove_staged_dir_and_prune_parent(&dir);
                Err(Error::from(load_error))
            }
        }
    }

    /// Transactionally replace a live `@session/<name>` source registration:
    /// pre-validate, pre-stage the replacement, remove the target, load the
    /// replacement — and on any failure of the replacement, restore the target's
    /// exact prior registration from its on-disk staged source so a failed
    /// replacement never destroys the working processor. Returns the minted
    /// registration [`ModuleIdent`](streamlib_idents::ModuleIdent) for the new
    /// definition.
    ///
    /// Removal-before-registration ordering is invariant-forced (the
    /// coexistence gates reject two live registrations of the same session
    /// name), so atomicity comes from compensation, not from reordering: the
    /// old staged dir is the restore artifact and is deleted only AFTER the new
    /// registration commits.
    #[tracing::instrument(skip(self, request), fields(target = %request.target_session_module))]
    pub async fn replace_processor_from_source(
        &self,
        request: ReplaceProcessorFromSource,
    ) -> Result<streamlib_idents::ModuleIdent> {
        let target = request.target_session_module.clone();

        // (1) PRE-VALIDATE — no mutation.
        //
        // (1a) The replacement must resolve to the target's `@session/<name>`;
        // a replace re-registers the same name, never renames.
        let (_replacement_type, replacement_name) =
            derive_session_type_and_name(&request.replacement)?;
        if !target.org.is_reserved_for_session() || replacement_name != target.name.as_str() {
            return Err(AddModuleError::ReplaceTargetNameMismatch {
                target,
                replacement_name,
            }
            .into());
        }

        // (1b) Restorability preflight. The target's LOADED (concrete) version
        // must be source-backed on disk — otherwise there is nothing to restore
        // from if the replacement fails. A target with no live registration (or
        // whose loaded version doesn't satisfy the requested range) falls through
        // to `remove_module`, which surfaces the typed not-loaded error before
        // any registration runs.
        let loaded_version =
            ledger::with_loaded_module_registration_record(&target.package_ref(), |r| r.version)
                .filter(|v| target.version.matches(*v));
        let Some(loaded_version) = loaded_version else {
            self.remove_module(target.clone()).map_err(Error::from)?;
            return self.register_processor_from_source(request.replacement).await;
        };
        let old_dir = session_source_staging_root()
            .join(target.name.as_str())
            .join(loaded_version.to_string());
        if !old_dir.is_dir() {
            return Err(AddModuleError::ReplaceTargetNotSourceBacked {
                target,
                expected_dir: old_dir,
            }
            .into());
        }

        // (2) PRE-STAGE the replacement (filesystem-only) BEFORE removal — a
        // language / mint / I/O refusal here leaves the target untouched.
        let staged = match stage_submitted_source(&request.replacement) {
            Ok(staged) => staged,
            Err(stage_error) => return Err(Error::from(stage_error)),
        };
        let new_dir = staged.dir.clone();

        // (3) Remove the target. A refusal (still required / in use) leaves the
        // target untouched; drop the pre-staged replacement scratch.
        if let Err(remove_error) = self.remove_module(target.clone()) {
            remove_staged_dir_and_prune_parent(&new_dir);
            return Err(Error::from(remove_error));
        }

        // (4) Load the pre-staged replacement. `load_staged_session_source`
        // reclaims the replacement's scratch dir on its own failure.
        match self.load_staged_session_source(staged).await {
            Ok(new_module) => {
                // (6) HOUSEKEEPING: the old staged dir was the restore artifact;
                // delete it only now that the new registration has committed.
                remove_staged_dir_and_prune_parent(&old_dir);
                Ok(new_module)
            }
            Err(cause) => {
                // (5) COMPENSATE: restore the target's EXACT prior ModuleIdent
                // from its still-present staged source. `Strategy::Path` +
                // `IfStale` hits the warm orchestrator sidecar slot and bypasses
                // re-minting, so the old `0.0.M` identity is restored verbatim.
                let restore = self.add_module_with(
                    streamlib_idents::ModuleIdent::new(
                        target.org.clone(),
                        target.name.clone(),
                        streamlib_idents::SemVerRange::Exact(loaded_version),
                    ),
                    Strategy::Path {
                        path: old_dir,
                        build: BuildPolicy::IfStale,
                    },
                );
                match restore.await {
                    Ok(_restored) => {
                        Err(AddModuleError::ReplacementRegistrationFailedPriorRegistrationRestored {
                            target,
                            cause: cause.to_string(),
                        }
                        .into())
                    }
                    Err(restore_error) => {
                        tracing::error!(
                            target = %target,
                            replacement_cause = %cause,
                            restore_error = %restore_error,
                            "replace_processor_from_source: replacement failed and \
                             restoring the prior registration also failed",
                        );
                        Err(AddModuleError::ReplacementRegistrationFailedRestoreAlsoFailed {
                            target,
                        }
                        .into())
                    }
                }
            }
        }
    }
}

/// Best-effort reclamation of a staged session-source version dir plus its
/// now-empty `@session/<name>/` parent. `remove_dir` on the parent removes it
/// only when empty, so a sibling version dir keeps the parent intact.
fn remove_staged_dir_and_prune_parent(dir: &Path) {
    if let Err(cleanup) = std::fs::remove_dir_all(dir) {
        tracing::debug!(
            dir = %dir.display(),
            "failed to remove staged session-source dir: {cleanup}",
        );
    }
    if let Some(parent) = dir.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Derive the `(PascalCase type name, kebab `@session/<name>` segment)` a
/// submission mints under. The type name falls back to a PascalCase projection
/// of the requested package name; the name segment falls back to a kebab
/// projection of the type name. One of the two must be present.
fn derive_session_type_and_name(
    request: &SubmittedProcessorSource,
) -> std::result::Result<(String, String), AddModuleError> {
    let type_name = request
        .processor_type_name
        .clone()
        .or_else(|| request.requested_name.as_deref().map(pascal_case))
        .filter(|t| !t.is_empty())
        .ok_or(AddModuleError::SubmittedSourceMissingName)?;

    let name_segment = request
        .requested_name
        .clone()
        .unwrap_or_else(|| kebab_case(&type_name));
    let name_segment = kebab_case(&name_segment);
    Ok((type_name, name_segment))
}

/// Resolve the per-language staging profile, or the unsupported-language
/// refusal for Rust (a full cargo build, never a live graph mutation).
fn live_submit_language(
    language: &ProcessorLanguage,
) -> std::result::Result<LiveSubmitLanguage, AddModuleError> {
    match language {
        ProcessorLanguage::Python => Ok(LiveSubmitLanguage {
            runtime_key: "python",
            source_rel: |stem| format!("python/{stem}.py"),
            entrypoint: |stem, type_name| format!("{stem}:{type_name}"),
            dep_artifacts: |name, _version| {
                vec![("pyproject.toml".to_string(), session_pyproject_toml(name))]
            },
        }),
        ProcessorLanguage::TypeScript => Ok(LiveSubmitLanguage {
            runtime_key: "deno",
            source_rel: |stem| format!("deno/{stem}.ts"),
            entrypoint: |stem, type_name| format!("{stem}.ts:{type_name}"),
            dep_artifacts: |_name, _version| {
                vec![("deno.json".to_string(), session_deno_json())]
            },
        }),
        ProcessorLanguage::Rust => Err(AddModuleError::SourceLanguageUnsupportedForLiveSubmit {
            language: "rust".to_string(),
        }),
    }
}

/// Stage a submitted source into a `@session/<name>@0.0.N` package directory:
/// mint the identity, write a generated `streamlib.yaml`, the source file in
/// the subprocess runtime's expected layout, and the language's
/// dependency-resolution artifacts (`pyproject.toml` / `deno.json`) so the
/// build orchestrator's venv / Deno provision tail has a project to resolve the
/// SDK from. Returns the staged dir + minted module ident, or a typed refusal
/// (unsupported language / missing name / un-mintable name / I/O).
fn stage_submitted_source(
    request: &SubmittedProcessorSource,
) -> std::result::Result<StagedSessionSource, AddModuleError> {
    let lang = live_submit_language(&request.language)?;
    let (type_name, name_segment) = derive_session_type_and_name(request)?;

    let minted = streamlib_idents::mint_session_module_ident(&name_segment).map_err(|e| {
        AddModuleError::SessionProcessorNameInvalid {
            type_name: type_name.clone(),
            detail: e.to_string(),
        }
    })?;

    let module_stem = name_segment.replace('-', "_");
    let dir = session_source_staging_root()
        .join(minted.package.as_str())
        .join(minted.version.to_string());

    let stage_err = |detail: String| AddModuleError::SubmittedSourceStagingFailed {
        module: minted.module.clone(),
        detail,
    };

    std::fs::create_dir_all(&dir)
        .map_err(|e| stage_err(format!("creating staging dir {}: {e}", dir.display())))?;

    let source_rel = (lang.source_rel)(&module_stem);
    let entrypoint = (lang.entrypoint)(&module_stem, &type_name);

    let source_path = dir.join(&source_rel);
    if let Some(parent) = source_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| stage_err(format!("creating source dir {}: {e}", parent.display())))?;
    }
    std::fs::write(&source_path, request.source_text.as_bytes())
        .map_err(|e| stage_err(format!("writing source {}: {e}", source_path.display())))?;

    for (rel, contents) in (lang.dep_artifacts)(minted.package.as_str(), minted.version) {
        let artifact_path = dir.join(&rel);
        std::fs::write(&artifact_path, contents.as_bytes()).map_err(|e| {
            stage_err(format!(
                "writing dependency artifact {}: {e}",
                artifact_path.display()
            ))
        })?;
    }

    let manifest = generate_session_manifest(
        minted.package.as_str(),
        minted.version,
        &type_name,
        lang.runtime_key,
        &entrypoint,
    );
    let manifest_path = dir.join(streamlib_idents::Manifest::FILE_NAME);
    std::fs::write(&manifest_path, manifest.as_bytes())
        .map_err(|e| stage_err(format!("writing manifest {}: {e}", manifest_path.display())))?;

    Ok(StagedSessionSource {
        module: minted.module,
        dir,
    })
}

/// A minimal `pyproject.toml` for a live-submitted Python session package: it
/// declares the `streamlib` SDK dependency so the orchestrator's venv tail
/// resolves it (from the linked checkout under an active `streamlib link`, or
/// from the registry by version otherwise) — without it the venv is empty and
/// the `import streamlib` probe hard-fails.
fn session_pyproject_toml(name: &str) -> String {
    format!(
        "[project]\n\
         name = \"{name}\"\n\
         version = \"0.0.0\"\n\
         dependencies = [\"streamlib\"]\n\
         \n\
         [build-system]\n\
         requires = [\"hatchling\"]\n\
         build-backend = \"hatchling.build\"\n"
    )
}

/// A minimal `deno.json` import map for a live-submitted TypeScript session
/// package: maps the `streamlib` specifier the source imports at the SDK so the
/// Deno subprocess resolves it (redirected at the linked checkout under an
/// active `streamlib link`).
fn session_deno_json() -> String {
    "{\n  \
       \"imports\": {\n    \
         \"streamlib\": \"npm:@tatolab/streamlib-deno\",\n    \
         \"streamlib/\": \"npm:/@tatolab/streamlib-deno/\"\n  \
       }\n\
     }\n"
        .to_string()
}

/// The root under which live-submitted session sources are staged:
/// `<STREAMLIB_DATA_DIR>/session-source/`.
fn session_source_staging_root() -> PathBuf {
    crate::core::streamlib_home::get_streamlib_data_dir().join("session-source")
}

/// Generate the `streamlib.yaml` for a single-processor `@session/<name>`
/// package staged from live-submitted source.
fn generate_session_manifest(
    name: &str,
    version: streamlib_idents::SemVer,
    type_name: &str,
    runtime_key: &str,
    entrypoint: &str,
) -> String {
    format!(
        "package:\n  \
           org: {org}\n  \
           name: {name}\n  \
           version: \"{version}\"\n\
         processors:\n  \
           - name: {type_name}\n    \
             description: live-submitted session processor\n    \
             runtime: {runtime_key}\n    \
             execution: manual\n    \
             entrypoint: \"{entrypoint}\"\n    \
             inputs: []\n    \
             outputs: []\n",
        org = streamlib_idents::SESSION_ORG,
    )
}

/// Project a package-name segment (`my-processor`) or a loose identifier into a
/// PascalCase type name (`MyProcessor`) — the inverse of
/// [`kebab_case`](super::session::kebab_case). Interior `-` / `_` boundaries
/// start a new capitalized run.
fn pascal_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut capitalize_next = true;
    for ch in name.chars() {
        if ch == '-' || ch == '_' || ch == ' ' {
            capitalize_next = true;
            continue;
        }
        if capitalize_next {
            out.extend(ch.to_uppercase());
            capitalize_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python_request(name: Option<&str>) -> SubmittedProcessorSource {
        SubmittedProcessorSource {
            source_text: "class Widget:\n    def process(self, ctx):\n        pass\n".to_string(),
            language: ProcessorLanguage::Python,
            requested_name: name.map(str::to_string),
            processor_type_name: Some("Widget".to_string()),
        }
    }

    #[test]
    fn pascal_case_projects_kebab_and_snake() {
        assert_eq!(pascal_case("my-processor"), "MyProcessor");
        assert_eq!(pascal_case("already_snake"), "AlreadySnake");
        assert_eq!(pascal_case("camera"), "Camera");
    }

    #[test]
    fn rust_source_is_refused_for_live_submit() {
        // Rust-from-source is a full cargo build (the `streamlib pkg build`
        // flow), never a live graph mutation. Revert the Rust arm and this
        // would attempt to stage a cargo project the live path can't build.
        let request = SubmittedProcessorSource {
            source_text: "struct S;".to_string(),
            language: ProcessorLanguage::Rust,
            requested_name: Some("widget".to_string()),
            processor_type_name: Some("Widget".to_string()),
        };
        let err = stage_submitted_source(&request).expect_err("rust must be refused");
        assert!(matches!(
            err,
            AddModuleError::SourceLanguageUnsupportedForLiveSubmit { .. }
        ));
    }

    #[test]
    fn missing_name_is_refused_before_staging() {
        // Neither a package name nor a type name: nothing to mint an identity
        // from. Refused before any filesystem side effect.
        let request = SubmittedProcessorSource {
            source_text: "class Widget: pass".to_string(),
            language: ProcessorLanguage::Python,
            requested_name: None,
            processor_type_name: None,
        };
        let err = stage_submitted_source(&request).expect_err("missing name must be refused");
        assert!(matches!(err, AddModuleError::SubmittedSourceMissingName));
    }

    #[test]
    fn stages_python_source_into_a_loadable_session_package() {
        // The staged dir must carry a parseable `streamlib.yaml` declaring the
        // `@session/<name>` package plus the source file at the entrypoint's
        // module path — the exact on-disk shape `Strategy::Path` loads.
        let request = python_request(Some("widget"));
        let staged = stage_submitted_source(&request).expect("python stages");
        assert_eq!(staged.module.org.as_str(), streamlib_idents::SESSION_ORG);

        let manifest_path = staged.dir.join(streamlib_idents::Manifest::FILE_NAME);
        assert!(manifest_path.is_file(), "manifest must be staged");
        let manifest = streamlib_idents::Manifest::load(&staged.dir).expect("manifest parses");
        let package = manifest.package.expect("package block present");
        assert_eq!(package.org.as_str(), streamlib_idents::SESSION_ORG);
        assert_eq!(package.name.as_str(), "widget");

        assert!(
            staged.dir.join("python").join("widget.py").is_file(),
            "source must be staged at the entrypoint module path"
        );

        // The Python dependency-resolution artifact must be staged beside the
        // source — without it the orchestrator's venv tail makes an empty venv
        // and the `import streamlib` probe hard-fails. Mentally-revert the
        // `dep_artifacts` staging and this pyproject.toml is absent.
        let pyproject = staged.dir.join("pyproject.toml");
        assert!(
            pyproject.is_file(),
            "a live-submitted Python session package must stage a pyproject.toml"
        );
        let pyproject_body = std::fs::read_to_string(&pyproject).unwrap();
        assert!(
            pyproject_body.contains("streamlib"),
            "the staged pyproject must declare the streamlib SDK dep so the venv \
             tail can resolve it, got: {pyproject_body}"
        );

        // Reclaim the scratch dir this hermetic test wrote.
        let _ = std::fs::remove_dir_all(&staged.dir);
    }

    #[test]
    fn stages_typescript_source_with_a_deno_json_import_map() {
        // The TypeScript live-submit path must stage the source under `deno/`
        // plus a `deno.json` import map so the Deno subprocess resolves the
        // `streamlib` SDK specifier. Mentally-revert the `dep_artifacts`
        // staging and the deno.json is absent.
        let request = SubmittedProcessorSource {
            source_text: "export class Widget {}\n".to_string(),
            language: ProcessorLanguage::TypeScript,
            requested_name: Some("ts-widget".to_string()),
            processor_type_name: Some("Widget".to_string()),
        };
        let staged = stage_submitted_source(&request).expect("typescript stages");
        assert!(
            staged.dir.join("deno").join("ts_widget.ts").is_file(),
            "source must be staged at the entrypoint module path"
        );
        let deno_json = staged.dir.join("deno.json");
        assert!(
            deno_json.is_file(),
            "a live-submitted TypeScript session package must stage a deno.json"
        );
        let deno_body = std::fs::read_to_string(&deno_json).unwrap();
        assert!(
            deno_body.contains("streamlib"),
            "the staged deno.json must map the streamlib specifier, got: {deno_body}"
        );
        let _ = std::fs::remove_dir_all(&staged.dir);
    }

    #[test]
    fn each_stage_mints_a_distinct_monotonic_version() {
        // Two submits of the same name mint distinct `0.0.N` versions — the
        // property `replace` relies on so a bumped re-registration never
        // collides with the prior one.
        let first = stage_submitted_source(&python_request(Some("widget"))).expect("first stages");
        let second =
            stage_submitted_source(&python_request(Some("widget"))).expect("second stages");
        assert_ne!(
            second.module.to_string(),
            first.module.to_string(),
            "each mint must advance the session version counter"
        );
        let _ = std::fs::remove_dir_all(&first.dir);
        let _ = std::fs::remove_dir_all(&second.dir);
    }

    // =========================================================================
    // Public-method orchestration locks. These drive `register_processor_from_
    // source` / `replace_processor_from_source` end-to-end against a fake
    // in-process build orchestrator (no Python/GPU/venv): subprocess processors
    // register their ports straight from the staged manifest, so the whole load
    // is hermetic. Each uses a distinct `@session/<name>` so the process-global
    // registry / ledger never collide across tests, and `#[serial]` guards the
    // process-global STREAMLIB_HOME + registry.
    // =========================================================================

    use std::sync::atomic::{AtomicBool, Ordering};

    use serial_test::serial;

    use super::super::build_orchestrator::{
        BuildError, BuildEvent, BuildEventSink, BuildOrchestrator, BuildRequest, BuildSource,
        StagedArtifact,
    };
    use crate::core::descriptors::{SchemaIdent, TypeName};
    use crate::core::processors::PROCESSOR_REGISTRY;

    /// An `add_local` fixture: a host-vtable session processor registered from a
    /// compiled type with NO staged source on disk — the not-source-backed case
    /// the transactional replace refuses.
    #[crate::processor("@app/local/FromSourceReplaceTarget", execution = manual)]
    struct FromSourceReplaceTarget;
    impl crate::core::ManualProcessor for FromSourceReplaceTarget::Processor {
        fn setup(
            &mut self,
            _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
        ) -> crate::core::error::Result<()> {
            Ok(())
        }
        fn teardown(
            &mut self,
            _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
        ) -> crate::core::error::Result<()> {
            Ok(())
        }
        fn start(
            &mut self,
            _ctx: &crate::core::context::RuntimeContextFullAccess<'_>,
        ) -> crate::core::error::Result<()> {
            Ok(())
        }
    }

    /// Resolve the registered structured processor ident for a session module's
    /// short type name.
    fn session_ident_for(
        module: &streamlib_idents::ModuleIdent,
        type_name: &str,
    ) -> Option<SchemaIdent> {
        PROCESSOR_REGISTRY.resolve_installed_processor_type(
            &module.org,
            &module.name,
            &TypeName::new(type_name).expect("valid type name"),
        )
    }

    /// Point STREAMLIB_HOME at a fresh tempdir so session-source staging lands
    /// in an isolated tree, and STREAMLIB_PYTHON_NATIVE_LIB at a dummy existing
    /// file so the Python host resolves at registration (the constructor only
    /// stores the path — it is dlopen'd at instantiate, which these
    /// registration-only tests never reach). Restores prior values on drop;
    /// serial tests only.
    struct HomeGuard {
        _tmp: tempfile::TempDir,
        prev_home: Option<String>,
        prev_native: Option<String>,
    }
    impl HomeGuard {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let native = tmp.path().join("libstreamlib_python_native.so");
            std::fs::write(&native, b"dummy").unwrap();
            let prev_home = std::env::var("STREAMLIB_HOME").ok();
            let prev_native = std::env::var("STREAMLIB_PYTHON_NATIVE_LIB").ok();
            // SAFETY: serial tests — no other thread races the process env.
            unsafe {
                std::env::set_var("STREAMLIB_HOME", tmp.path());
                std::env::set_var("STREAMLIB_PYTHON_NATIVE_LIB", &native);
            }
            Self {
                _tmp: tmp,
                prev_home,
                prev_native,
            }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: see `HomeGuard::new` — serial tests, no env race.
            unsafe {
                match &self.prev_home {
                    Some(v) => std::env::set_var("STREAMLIB_HOME", v),
                    None => std::env::remove_var("STREAMLIB_HOME"),
                }
                match &self.prev_native {
                    Some(v) => std::env::set_var("STREAMLIB_PYTHON_NATIVE_LIB", v),
                    None => std::env::remove_var("STREAMLIB_PYTHON_NATIVE_LIB"),
                }
            }
        }
    }

    /// Block on a `Runner` async op using the runner's own owned tokio runtime.
    fn drive<T>(runtime: &Runner, fut: impl std::future::Future<Output = T>) -> T {
        runtime.tokio_runtime_variant.handle().block_on(fut)
    }

    /// No-op build event sink.
    struct NoopSink;
    impl BuildEventSink for NoopSink {
        fn emit(&self, _event: BuildEvent) {}
    }

    /// "Materializes" a `PackageDir` by loading it in place — no toolchain. The
    /// subprocess registration path reads ports from the staged manifest, so
    /// this is enough to register a session source with real ports.
    struct LoadAsIsOrchestrator;
    impl BuildOrchestrator for LoadAsIsOrchestrator {
        fn materialize(
            &self,
            request: &BuildRequest,
            _sink: &dyn BuildEventSink,
        ) -> std::result::Result<StagedArtifact, BuildError> {
            match &request.source {
                BuildSource::PackageDir(dir) => Ok(StagedArtifact {
                    staged_dir: dir.clone(),
                    rebuilt: false,
                }),
                other => Err(BuildError::UnsupportedSource(format!("{other:?}"))),
            }
        }
    }

    /// [`LoadAsIsOrchestrator`] that fails the FIRST materialize after it is
    /// armed, then succeeds — the fault injection for the transactional-replace
    /// lock: it fails the replacement load but lets the compensating restore of
    /// the prior registration through.
    struct FailNextMaterializeOrchestrator {
        armed: AtomicBool,
    }
    impl BuildOrchestrator for FailNextMaterializeOrchestrator {
        fn materialize(
            &self,
            request: &BuildRequest,
            _sink: &dyn BuildEventSink,
        ) -> std::result::Result<StagedArtifact, BuildError> {
            match &request.source {
                BuildSource::PackageDir(dir) => {
                    if self.armed.swap(false, Ordering::SeqCst) {
                        return Err(BuildError::BuildFailed {
                            tool: "test".to_string(),
                            package: request.package.to_string(),
                            detail: "forced replacement materialize failure".to_string(),
                        });
                    }
                    Ok(StagedArtifact {
                        staged_dir: dir.clone(),
                        rebuilt: false,
                    })
                }
                other => Err(BuildError::UnsupportedSource(format!("{other:?}"))),
            }
        }
    }

    /// [`LoadAsIsOrchestrator`] that rewrites the staged manifest's empty
    /// `inputs: []` / `outputs: []` placeholders into real ports before the
    /// load reads them — the fake stand-in for the (out-of-engine) subprocess
    /// port-extraction pass, proving the registration path mints CONNECTABLE
    /// ports from whatever the staged manifest declares.
    struct PortSplicingOrchestrator;
    impl BuildOrchestrator for PortSplicingOrchestrator {
        fn materialize(
            &self,
            request: &BuildRequest,
            _sink: &dyn BuildEventSink,
        ) -> std::result::Result<StagedArtifact, BuildError> {
            let BuildSource::PackageDir(dir) = &request.source else {
                return Err(BuildError::UnsupportedSource(format!("{:?}", request.source)));
            };
            let manifest_path = dir.join(streamlib_idents::Manifest::FILE_NAME);
            let body = std::fs::read_to_string(&manifest_path).map_err(|e| BuildError::Other {
                package: request.package.to_string(),
                detail: format!("read staged manifest: {e}"),
            })?;
            let spliced = body
                .replace(
                    "inputs: []",
                    "inputs:\n      - name: in0\n        schema: any",
                )
                .replace(
                    "outputs: []",
                    "outputs:\n      - name: out0\n        schema: any",
                );
            std::fs::write(&manifest_path, spliced).map_err(|e| BuildError::Other {
                package: request.package.to_string(),
                detail: format!("write spliced manifest: {e}"),
            })?;
            Ok(StagedArtifact {
                staged_dir: dir.clone(),
                rebuilt: false,
            })
        }
    }

    fn python_named(name: &str) -> SubmittedProcessorSource {
        SubmittedProcessorSource {
            source_text: "class Widget:\n    def process(self, ctx):\n        pass\n".to_string(),
            language: ProcessorLanguage::Python,
            requested_name: Some(name.to_string()),
            processor_type_name: Some("Widget".to_string()),
        }
    }

    fn session_package_in_ledger(name: &str) -> bool {
        let pkg = streamlib_idents::PackageRef::new(
            streamlib_idents::Org::new(streamlib_idents::SESSION_ORG).unwrap(),
            streamlib_idents::Package::new(name).unwrap(),
        );
        ledger::loaded_module_registration_ledger_packages().contains(&pkg)
    }

    #[test]
    #[serial]
    fn register_from_source_without_orchestrator_is_a_typed_error_with_no_residue() {
        // (7a) A build-requiring load with NO orchestrator wired fails typed,
        // the staged scratch dir (and its empty parent) is reclaimed, and the
        // ledger carries zero residue. Mentally-revert the on-failure cleanup in
        // `load_staged_session_source` and the staged parent survives.
        let _home = HomeGuard::new();
        let runtime = Runner::new().unwrap();
        let name = "fromsrc-no-orch";

        let err = drive(&runtime, runtime.register_processor_from_source(python_named(name)))
            .expect_err("no orchestrator must fail the build-requiring load");
        // Wrapped through `Error::from(AddModuleError)`.
        assert!(
            err.to_string().contains("BuildOrchestrator")
                || err.to_string().contains("no BuildOrchestrator"),
            "expected a no-orchestrator error, got: {err}"
        );
        assert!(
            !session_source_staging_root().join(name).exists(),
            "the staged @session/<name> dir must be reclaimed after a failed load"
        );
        assert!(
            !session_package_in_ledger(name),
            "a failed load must leave zero ledger residue"
        );
    }

    #[test]
    #[serial]
    fn replace_of_an_unregistered_target_surfaces_the_remove_error() {
        // (7b) Replacing a target that was never loaded surfaces the typed
        // not-loaded removal error before any registration runs.
        let _home = HomeGuard::new();
        let runtime = Runner::new().unwrap();
        runtime.set_build_orchestrator(LoadAsIsOrchestrator);

        let target = streamlib_idents::ModuleIdent::any(
            streamlib_idents::Org::new(streamlib_idents::SESSION_ORG).unwrap(),
            streamlib_idents::Package::new("fromsrc-never-loaded").unwrap(),
        );
        let request = ReplaceProcessorFromSource {
            target_session_module: target,
            replacement: python_named("fromsrc-never-loaded"),
        };
        let err = drive(&runtime, runtime.replace_processor_from_source(request))
            .expect_err("replacing an unloaded target must error");
        assert!(
            err.to_string().contains("remove_module"),
            "expected the remove_module not-loaded error, got: {err}"
        );
        assert!(
            !session_package_in_ledger("fromsrc-never-loaded"),
            "no registration must have occurred"
        );
    }

    #[test]
    #[serial]
    fn replace_failure_restores_the_exact_prior_registration() {
        // (7c) TRANSACTIONAL LOCK: a replacement whose load fails must leave the
        // OLD exact registration intact (restored from disk) and surface the
        // typed "prior registration restored" error. Mentally-revert the
        // compensation branch and the old registration is gone.
        let _home = HomeGuard::new();
        let runtime = Runner::new().unwrap();
        runtime.set_build_orchestrator(FailNextMaterializeOrchestrator {
            armed: AtomicBool::new(false),
        });
        let name = "fromsrc-transactional";

        // Register the original (orchestrator not yet armed → succeeds).
        let original = drive(
            &runtime,
            runtime.register_processor_from_source(python_named(name)),
        )
        .expect("original registration succeeds");
        let original_ident = session_ident_for(&original, "Widget").expect("original resolves");
        assert!(PROCESSOR_REGISTRY.is_registered(&original_ident));

        // Arm the fault so the replacement's load fails; the compensating
        // restore (a second materialize) then succeeds.
        runtime.set_build_orchestrator(FailNextMaterializeOrchestrator {
            armed: AtomicBool::new(true),
        });
        let request = ReplaceProcessorFromSource {
            target_session_module: original.clone(),
            replacement: python_named(name),
        };
        let err = drive(&runtime, runtime.replace_processor_from_source(request))
            .expect_err("a failing replacement must error");
        assert!(
            matches!(
                err,
                Error::Configuration(ref m)
                    if m.contains("prior registration was restored")
            ),
            "expected the restored-prior error, got: {err}"
        );

        // The OLD exact ident is still registered — the working processor
        // survived the failed replacement.
        assert!(
            PROCESSOR_REGISTRY.is_registered(&original_ident),
            "the prior registration must be restored intact"
        );
        assert!(
            session_package_in_ledger(name),
            "the target's ledger record must survive the failed replace"
        );

        // Cleanup.
        runtime
            .remove_module(original)
            .expect("cleanup remove of the restored target");
    }

    #[test]
    #[serial]
    fn register_from_source_mints_connectable_ports_from_the_manifest() {
        // (7d) PORTS LOCK: when the staged manifest declares ports, the
        // registered descriptor carries them (connectable). The regression twin
        // below asserts the placeholder (portless) manifest yields empty ports —
        // proving the ports come from the manifest the orchestrator produced,
        // not from anything hardcoded.
        let _home = HomeGuard::new();

        // With ports spliced into the staged manifest → connectable ports.
        let runtime = Runner::new().unwrap();
        runtime.set_build_orchestrator(PortSplicingOrchestrator);
        let ported = drive(
            &runtime,
            runtime.register_processor_from_source(python_named("fromsrc-ported")),
        )
        .expect("registration with ports succeeds");
        let ported_ident = session_ident_for(&ported, "Widget").expect("ported resolves");
        let (inputs, outputs) = PROCESSOR_REGISTRY
            .port_info(&ported_ident)
            .expect("a registered session processor exposes port info");
        assert_eq!(inputs.len(), 1, "spliced input port must be registered");
        assert_eq!(outputs.len(), 1, "spliced output port must be registered");
        runtime.remove_module(ported).expect("cleanup ported");

        // Regression twin: the placeholder manifest (no splice) → portless.
        let plain_runtime = Runner::new().unwrap();
        plain_runtime.set_build_orchestrator(LoadAsIsOrchestrator);
        let plain = drive(
            &plain_runtime,
            plain_runtime.register_processor_from_source(python_named("fromsrc-portless")),
        )
        .expect("registration without ports succeeds");
        let plain_ident = session_ident_for(&plain, "Widget").expect("plain resolves");
        let (plain_in, plain_out) = PROCESSOR_REGISTRY
            .port_info(&plain_ident)
            .expect("port info present");
        assert!(
            plain_in.is_empty() && plain_out.is_empty(),
            "the placeholder manifest must yield a portless descriptor — got \
             {plain_in:?} / {plain_out:?}"
        );
        plain_runtime.remove_module(plain).expect("cleanup portless");
    }

    #[test]
    #[serial]
    fn replace_of_an_add_local_target_is_refused_and_leaves_it_intact() {
        // (7e) An `add_local` host-vtable registration is not source-backed on
        // disk, so a transactional replace refuses it (ReplaceTargetNotSource-
        // Backed) and leaves the target intact — the operator must use the
        // explicit remove + register escape hatch.
        let _home = HomeGuard::new();
        let runtime = Runner::new().unwrap();
        let loaded = runtime
            .add_local_blocking::<FromSourceReplaceTarget::Processor>(serde_json::Value::Null)
            .expect("add_local registers a host-vtable session processor");

        let request = ReplaceProcessorFromSource {
            target_session_module: loaded.ident.clone(),
            replacement: python_named(loaded.ident.name.as_str()),
        };
        let err = drive(&runtime, runtime.replace_processor_from_source(request))
            .expect_err("replacing a non-source-backed add_local target must be refused");
        assert!(
            matches!(
                err,
                Error::Configuration(ref m) if m.contains("not source-backed")
            ),
            "expected ReplaceTargetNotSourceBacked, got: {err}"
        );
        // The add_local target is untouched.
        let ident = session_ident_for(&loaded.ident, "FromSourceReplaceTarget")
            .expect("the add_local target must remain registered");
        assert!(PROCESSOR_REGISTRY.is_registered(&ident));
        runtime.remove_module(loaded.ident).expect("cleanup remove");
    }
}
