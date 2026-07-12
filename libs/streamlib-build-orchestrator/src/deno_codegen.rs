// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Deno TypeScript wire-vocabulary codegen tail for the build orchestrator.
//!
//! A Deno package's processors import their wire types from a package-local
//! `_generated_/` (e.g. `./_generated_/tatolab__core/video_frame.ts`),
//! materialized by `streamlib generate --runtime typescript`. That directory
//! is a per-consumer build artifact — `streamlib pack` excludes it from the
//! shipped source (exactly as it excludes Python's `_generated_`) — so the
//! orchestrator must regenerate it into the staged package after assembly.
//!
//! This is the Deno mirror of [`crate::python_venv`]'s
//! `ensure_streamlib_generated_in_venv`: both run in-process JTD codegen so a
//! staged polyglot package is runnable without a separate `deno task setup`.
//! Schema deps (e.g. `@tatolab/core`) resolve from the static registry via the
//! codegen resolver's env-aware config (`STREAMLIB_REGISTRY_URL` / `STREAMLIB_REGISTRY_URL`)
//! — the same path the Rust build-script codegen uses.
//!
//! Generating into the orchestrator's build-to-temp directory means the
//! existing atomic rename ([`crate::atomic_swap`]) carries `_generated_/` into
//! place — no second rename.

use std::path::Path;

use streamlib_cargo_build as build;
use streamlib_engine::core::runtime::BuildError;

use crate::build_failed;

/// Generate the staged Deno package's `_generated_/` wire vocabulary.
///
/// No-op (returns `Ok(())`) when the staged package declares no TypeScript
/// runtime processors.
#[tracing::instrument(skip(temp_dir), fields(temp_dir = %temp_dir.display(), package = %package_label))]
pub fn provision_deno_typescript(temp_dir: &Path, package_label: &str) -> Result<(), BuildError> {
    let config = match build::read_minimal_project_config(temp_dir) {
        Ok(Some(config)) => config,
        Ok(None) => return Ok(()),
        Err(e) => {
            return Err(build_failed(
                package_label,
                format!("reading streamlib.yaml for Deno codegen: {e}"),
            ))
        }
    };
    if !build::has_typescript_runtime_processors(&config) {
        tracing::debug!("no Deno runtime in staged package — skipping TypeScript codegen");
        return Ok(());
    }

    // Always materialize `_generated_/` for a Deno package, even when codegen
    // emits nothing (no external schema deps). Its presence is the reliable
    // "the codegen tail ran" marker the cache-reuse guard
    // ([`staged_package_has_deno`] + `_generated_` existence) checks — the
    // Deno parallel to Python's always-present `.venv`. Without an
    // unconditional marker, a no-schema Deno package would never satisfy the
    // guard and would re-stage on every IfStale hit.
    let generated = temp_dir.join("_generated_");
    std::fs::create_dir_all(&generated).map_err(|e| {
        build_failed(
            package_label,
            format!("creating _generated_ dir for Deno codegen: {e}"),
        )
    })?;
    tracing::info!(
        generated = %generated.display(),
        "generating Deno wire vocabulary into staged package"
    );
    streamlib_jtd_codegen::generate(streamlib_jtd_codegen::GenerateOptions {
        runtime: streamlib_jtd_codegen::RuntimeTarget::Typescript,
        output: generated,
        project_dir: Some(temp_dir.to_path_buf()),
        schema_file: None,
        schema_dir: None,
        workspace_root: temp_dir.to_path_buf(),
        // The staged package is a transient cache slot; a lockfile written
        // here is byproduct (the per-package fingerprint is the staleness
        // oracle, not a lockfile).
        write_lockfile: false,
    })
    .map_err(|e| {
        build_failed(
            package_label,
            format!(
                "failed to generate Deno wire vocabulary (schema deps resolve from the \
                 registry — is STREAMLIB_REGISTRY_URL / STREAMLIB_REGISTRY_URL set?): {e}"
            ),
        )
    })
}

/// Whether a staged package carries a Deno (TypeScript) runtime — read from
/// its manifest, matching the gate [`provision_deno_typescript`] uses. The
/// cache-reuse guard pairs this with `_generated_` existence so a Deno slot
/// missing its regenerated wire vocabulary re-stages instead of being reused
/// broken (the Deno parallel to [`crate::python_venv::staged_package_has_python`]
/// + `.venv` existence).
pub(crate) fn staged_package_has_deno(dir: &Path) -> bool {
    matches!(
        build::read_minimal_project_config(dir),
        Ok(Some(config)) if build::has_typescript_runtime_processors(&config)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn jtd_codegen_available() -> bool {
        std::process::Command::new("jtd-codegen")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn staged_package_has_deno_reads_manifest() {
        let deno = tempdir().unwrap();
        std::fs::write(
            deno.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: ts\n  version: 0.1.0\nprocessors:\n  - name: T\n    version: 1.0.0\n    description: d\n    runtime: deno\n    execution: manual\n    entrypoint: \"t.ts:default\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        assert!(staged_package_has_deno(deno.path()));

        let schemas_only = tempdir().unwrap();
        std::fs::write(
            schemas_only.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: s\n  version: 0.1.0\nschemas:\n  T:\n    file: schemas/t.yaml\n",
        )
        .unwrap();
        assert!(!staged_package_has_deno(schemas_only.path()));
        // Missing manifest → not a Deno package.
        assert!(!staged_package_has_deno(tempdir().unwrap().path()));
    }

    #[test]
    fn no_typescript_processors_is_noop() {
        // A schema-only (or pure-Python/Rust) staged package must not get a
        // `_generated_/` from the Deno tail.
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: schemas-only\n  version: 0.1.0\nschemas:\n  T:\n    file: schemas/t.yaml\n",
        )
        .unwrap();
        provision_deno_typescript(dir.path(), "schemas-only").unwrap();
        assert!(
            !dir.path().join("_generated_").exists(),
            "non-Deno package must not get a _generated_ directory"
        );
    }

    #[test]
    fn generates_typescript_wire_vocabulary_for_local_schema() {
        // End-to-end against a self-contained package (a Local schema, no
        // registry dep): proves the tail runs codegen and emits the package's
        // wire vocabulary as `.ts` under `_generated_/`. Skips (does not fail)
        // when `jtd-codegen` is absent so the suite stays green without it.
        if !jtd_codegen_available() {
            eprintln!("skipping: `jtd-codegen` not on PATH");
            return;
        }
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: ts\n  version: 0.1.0\nschemas:\n  Foo:\n    file: schemas/foo.yaml\nprocessors:\n  - name: T\n    version: 1.0.0\n    description: d\n    runtime: deno\n    execution: manual\n    entrypoint: \"t.ts:default\"\n    inputs: []\n    outputs: []\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("schemas")).unwrap();
        std::fs::write(
            dir.path().join("schemas/foo.yaml"),
            "metadata:\n  type: Foo\n  max_payload_bytes: 16\nproperties:\n  x:\n    type: uint32\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("t.ts"), b"export default class {}").unwrap();

        provision_deno_typescript(dir.path(), "ts").unwrap();

        let generated = dir.path().join("_generated_");
        assert!(generated.is_dir(), "_generated_ must be created");
        // At least one emitted `.ts` somewhere under _generated_.
        let mut found_ts = false;
        for entry in walkdir(&generated) {
            if entry.extension().and_then(|e| e.to_str()) == Some("ts") {
                found_ts = true;
                break;
            }
        }
        assert!(found_ts, "codegen must emit a .ts wire type under _generated_");
    }

    fn walkdir(root: &Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else {
                        out.push(p);
                    }
                }
            }
        }
        out
    }
}
