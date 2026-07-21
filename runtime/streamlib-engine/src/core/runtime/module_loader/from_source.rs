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

use std::path::PathBuf;

use streamlib_processor_schema::ProcessorLanguage;

use super::super::Runner;
use super::super::operations::{ReplaceProcessorFromSource, SubmittedProcessorSource};
use super::build_orchestrator::BuildPolicy;
use super::errors::AddModuleError;
use super::session::kebab_case;
use super::source::Strategy;
use crate::core::error::{Error, Result};

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
        let module = staged.module.clone();
        let dir = staged.dir.clone();
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
                // Best-effort staging cleanup: the load failed, so the staged
                // dir is dead. The module_loader already discarded its
                // registration staging (zero registry residue); this only
                // reclaims the scratch directory.
                if let Err(cleanup) = std::fs::remove_dir_all(&dir) {
                    tracing::debug!(
                        dir = %dir.display(),
                        "failed to remove staged session-source dir after a failed submit: {cleanup}",
                    );
                }
                Err(Error::from(load_error))
            }
        }
    }

    /// Remove a prior `@session/<name>` registration, then re-register the
    /// replacement source at a monotonically-bumped `0.0.N`. The removal must
    /// succeed (the target must be a live, in-use-free session registration)
    /// before the replacement registers, so a replace never leaves two live
    /// registrations of the same name. Returns the minted registration
    /// [`ModuleIdent`](streamlib_idents::ModuleIdent) for the new definition.
    #[tracing::instrument(skip(self, request), fields(target = %request.target_session_module))]
    pub async fn replace_processor_from_source(
        &self,
        request: ReplaceProcessorFromSource,
    ) -> Result<streamlib_idents::ModuleIdent> {
        self.remove_module(request.target_session_module.clone())
            .map_err(Error::from)?;
        self.register_processor_from_source(request.replacement).await
    }
}

/// Stage a submitted source into a `@session/<name>@0.0.N` package directory:
/// mint the identity, write a generated `streamlib.yaml` plus the source file
/// in the subprocess runtime's expected layout. Returns the staged dir + minted
/// module ident, or a typed refusal (unsupported language / missing name /
/// un-mintable name / I/O).
fn stage_submitted_source(
    request: &SubmittedProcessorSource,
) -> std::result::Result<StagedSessionSource, AddModuleError> {
    let runtime_key = match request.language {
        ProcessorLanguage::Python => "python",
        ProcessorLanguage::TypeScript => "deno",
        ProcessorLanguage::Rust => {
            return Err(AddModuleError::SourceLanguageUnsupportedForLiveSubmit {
                language: "rust".to_string(),
            });
        }
    };

    // Type name (PascalCase) → subprocess entrypoint class + registered short
    // name. Falls back to a PascalCase projection of the requested package
    // name; one of the two must be present.
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

    let (source_rel, entrypoint) = match request.language {
        ProcessorLanguage::Python => (
            format!("python/{module_stem}.py"),
            format!("{module_stem}:{type_name}"),
        ),
        ProcessorLanguage::TypeScript => (
            format!("deno/{module_stem}.ts"),
            format!("{module_stem}.ts:{type_name}"),
        ),
        ProcessorLanguage::Rust => unreachable!("rust rejected above"),
    };

    let source_path = dir.join(&source_rel);
    if let Some(parent) = source_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| stage_err(format!("creating source dir {}: {e}", parent.display())))?;
    }
    std::fs::write(&source_path, request.source_text.as_bytes())
        .map_err(|e| stage_err(format!("writing source {}: {e}", source_path.display())))?;

    let manifest = generate_session_manifest(
        minted.package.as_str(),
        minted.version,
        &type_name,
        runtime_key,
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

        // Reclaim the scratch dir this hermetic test wrote.
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
}
