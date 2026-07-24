// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python venv provisioning tail for the build orchestrator.
//!
//! When a package's staged artifact carries a Python runtime, the
//! orchestrator provisions a self-contained virtual environment INSIDE the
//! staged directory at build time — `{staged_dir}/.venv` — so the runtime
//! spawn site never has to create or mutate a venv. Building the venv into
//! the orchestrator's build-to-temp directory means the existing atomic
//! rename ([`crate::atomic_swap`]) carries the venv into place atomically;
//! no second rename is added here.
//!
//! This relocates the venv-creation logic that previously lived in the
//! engine's `spawn_python_subprocess_op` (per-spawn, cache-keyed) to a
//! single build-time provision. The shape of the work is identical:
//! `uv venv` → `uv pip install` (pre-built wheel when present, editable
//! source install otherwise) → populate the SDK's generated wire
//! vocabulary via in-process JTD codegen → pre-warm `.pyc` via
//! `python -m compileall`.

use std::path::{Path, PathBuf};
use std::process::Command;

use streamlib_engine::core::runtime::BuildError;

/// Provision a Python virtual environment inside `temp_dir` (the
/// orchestrator's build-to-temp staging directory) when the staged package
/// carries a Python runtime.
///
/// On success the venv lives at `{temp_dir}/.venv` with its interpreter at
/// `{temp_dir}/.venv/bin/python` (Unix) / `{temp_dir}/.venv/Scripts/python.exe`
/// (Windows) — so that after the orchestrator's atomic rename it lands at
/// `{staged_package_dir}/.venv/bin/python`.
///
/// `link_python_sdk`, when `Some`, is the linked checkout's Python SDK path
/// (resolved once by the orchestrator from the active `streamlib link`); the
/// staged pyproject's `streamlib` dependency is redirected at it so the venv
/// resolves the SDK from the checkout instead of the registry — the cargo
/// `[patch]` mirror on the Python side. `None` ⇒ registry resolution.
///
/// `link_checkout` is that same active-link checkout root, threaded to the
/// in-venv `_generated_` codegen so the SDK's schema deps resolve from the
/// checkout under a link. It is the orchestrator's authoritative link state,
/// NOT re-derived from the marker — a relocated venv codegen must not walk up
/// out of the staged cache into a stray marker (see
/// [`streamlib_jtd_codegen::generate`]). `None` ⇒ registry resolution.
///
/// No-op (returns `Ok(())`) when the staged package has no Python runtime.
#[tracing::instrument(skip(temp_dir, link_python_sdk, link_checkout), fields(temp_dir = %temp_dir.display(), package = %package_label))]
pub fn provision_python_venv(
    temp_dir: &Path,
    link_python_sdk: Option<&Path>,
    link_checkout: Option<&Path>,
    package_label: &str,
) -> Result<(), BuildError> {
    if !staged_package_has_python(temp_dir) {
        tracing::debug!("no Python runtime in staged package — skipping venv provisioning");
        return Ok(());
    }

    tracing::info!("provisioning Python venv inside staged package");

    let uv_cache_dir = streamlib_engine::core::get_uv_cache_dir();

    let venv_dir = temp_dir.join(".venv");

    #[cfg(unix)]
    let venv_python = venv_dir.join("bin").join("python");
    #[cfg(windows)]
    let venv_python = venv_dir.join("Scripts").join("python.exe");

    // The pre-built wheel (binary install, no build backend at install time)
    // is the load-bearing path for container deploys; editable source install
    // against the package's pyproject.toml is the dev-tree fallback. Both are
    // staged into `temp_dir` by `assemble_artifact` (full Python source tree,
    // wheels included).
    let prebuilt_wheel = find_first_wheel(&temp_dir.join("python").join("wheels"), package_label)?;
    let pyproject_path = {
        let p = temp_dir.join("pyproject.toml");
        if p.exists() { Some(p) } else { None }
    };

    // ---- link-mode override ----
    // When a `streamlib link` is active (the orchestrator resolved the
    // checkout's Python SDK), redirect the staged pyproject's `streamlib`
    // dependency at it before installing, so the venv resolves the SDK from the
    // checkout (mirrors the cargo `[patch]` injected into the cdylib build).
    if let (Some(pyproject), Some(sdk_path)) = (&pyproject_path, link_python_sdk) {
        apply_link_override(pyproject, sdk_path, package_label)?;
    }

    // ---- uv venv ----
    let venv_dir_str = path_to_str(&venv_dir, package_label)?;
    let output = run_uv(&["venv", &venv_dir_str, "--python", "3.12"], &uv_cache_dir)
        .map_err(|detail| build_failed(package_label, detail))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(build_failed(
            package_label,
            format!("uv venv failed: {stderr}"),
        ));
    }
    tracing::info!(venv = %venv_dir.display(), "venv created");

    // ---- uv pip install ----
    let venv_python_str = path_to_str(&venv_python, package_label)?;
    if let Some(ref wheel) = prebuilt_wheel {
        tracing::info!(wheel = %wheel.display(), "installing pre-built wheel");
        let wheel_str = path_to_str(wheel, package_label)?;
        let output = run_uv(
            &["pip", "install", &wheel_str, "--python", &venv_python_str],
            &uv_cache_dir,
        )
        .map_err(|detail| build_failed(package_label, detail))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(build_failed(
                package_label,
                format!("uv pip install (wheel) failed: {stderr}"),
            ));
        }
    } else if let Some(ref pyproject) = pyproject_path {
        tracing::info!(project = %temp_dir.display(), "installing project deps from source");
        let _ = pyproject; // presence-gated; install targets the project dir
        let project_str = path_to_str(temp_dir, package_label)?;
        let output = run_uv(
            &[
                "pip",
                "install",
                "-e",
                &project_str,
                "--python",
                &venv_python_str,
            ],
            &uv_cache_dir,
        )
        .map_err(|detail| build_failed(package_label, detail))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(build_failed(
                package_label,
                format!("uv pip install (source) failed: {stderr}"),
            ));
        }
    }

    // ---- codegen: populate streamlib/_generated_ in the venv ----
    ensure_streamlib_generated_in_venv(&venv_python, link_checkout, package_label)?;

    // ---- compileall: pre-warm .pyc ----
    precompile_venv(&venv_python, &venv_dir, package_label)?;

    tracing::info!("Python venv provisioned");
    Ok(())
}

/// Whether the staged package directory carries a Python runtime. Detected
/// from the staged tree itself: a `python/` source directory (the layout
/// `streamlib pack` / `assemble_artifact` stages Python source into) or a
/// `pyproject.toml` at the package root.
pub(crate) fn staged_package_has_python(temp_dir: &Path) -> bool {
    temp_dir.join("python").is_dir() || temp_dir.join("pyproject.toml").is_file()
}

/// Populate the SDK's wire-vocabulary package (`streamlib/_generated_`) inside
/// the venv via the in-process JTD codegen.
///
/// The published `streamlib` SDK ships source only — its `_generated_/` is a
/// build artifact excluded from the distribution. Resolve the installed
/// `streamlib` package directory through the venv interpreter, then run
/// codegen against the SDK's shipped `streamlib.yaml`. Skips when
/// `_generated_` is already populated. Fails loud when `streamlib` isn't
/// importable in the venv: the SDK is resolved from a registry by version
/// (not injected), so a Python package MUST declare `streamlib` as a
/// dependency.
fn ensure_streamlib_generated_in_venv(
    venv_python: &Path,
    link_checkout: Option<&Path>,
    package_label: &str,
) -> Result<(), BuildError> {
    let probe = Command::new(venv_python)
        .args([
            "-c",
            "import streamlib, os; print(os.path.dirname(streamlib.__file__))",
        ])
        .output()
        .map_err(|e| {
            build_failed(
                package_label,
                format!("failed to probe streamlib in the processor venv: {e}"),
            )
        })?;
    if !probe.status.success() {
        return Err(build_failed(
            package_label,
            format!(
                "streamlib is not installed in the processor venv. A Python package \
                 must declare `streamlib` as a dependency — it is resolved from the \
                 registry by version, not injected. Add `streamlib` to the package's \
                 pyproject.toml. ({})",
                String::from_utf8_lossy(&probe.stderr).trim()
            ),
        ));
    }
    let streamlib_dir = PathBuf::from(String::from_utf8_lossy(&probe.stdout).trim().to_string());

    let generated = streamlib_dir.join("_generated_");
    let already = generated
        .read_dir()
        .map(|mut d| {
            // An empty `_generated_/` (or one carrying only `__init__.py`)
            // is treated as unpopulated — codegen must still run.
            d.any(|entry| {
                entry
                    .ok()
                    .map(|e| e.file_name() != "__init__.py")
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if already {
        tracing::debug!(generated = %generated.display(), "streamlib wire vocabulary already populated — skipping codegen");
        return Ok(());
    }

    let manifest = streamlib_dir.join("streamlib.yaml");
    if !manifest.exists() {
        return Err(build_failed(
            package_label,
            "the installed streamlib is missing streamlib.yaml, so its wire vocabulary \
             can't be generated. The streamlib version is too old for this engine, or \
             the distribution is malformed."
                .to_string(),
        ));
    }

    tracing::info!(generated = %generated.display(), "generating streamlib wire vocabulary into venv");
    streamlib_jtd_codegen::generate(streamlib_jtd_codegen::GenerateOptions {
        runtime: streamlib_jtd_codegen::RuntimeTarget::Python,
        output: generated,
        project_dir: Some(streamlib_dir.clone()),
        schema_file: None,
        schema_dir: None,
        workspace_root: streamlib_dir,
        write_lockfile: false,
        // Authoritative link state from the orchestrator (checkout when linked,
        // `None` on distribution) — never marker-re-derived from this relocated
        // venv dir.
        link_checkout: link_checkout.map(|p| p.to_path_buf()),
    })
    .map_err(|e| {
        build_failed(
            package_label,
            format!("failed to generate streamlib wire vocabulary in venv: {e}"),
        )
    })
}

/// Pre-warm `.pyc` files across the venv via `python -m compileall` so the
/// first runtime import doesn't pay the bytecode-compile cost. Uses the
/// venv's own interpreter. A non-success exit is treated as fatal — a venv
/// that can't compile its own modules is a broken provision.
fn precompile_venv(
    venv_python: &Path,
    venv_dir: &Path,
    package_label: &str,
) -> Result<(), BuildError> {
    let venv_dir_str = path_to_str(venv_dir, package_label)?;
    tracing::info!(venv = %venv_dir.display(), "pre-warming venv bytecode (compileall)");
    let output = Command::new(venv_python)
        .args(["-m", "compileall", "-q", &venv_dir_str])
        .output()
        .map_err(|e| build_failed(package_label, format!("failed to run compileall: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(build_failed(
            package_label,
            format!("compileall failed: {stderr}"),
        ));
    }
    Ok(())
}

/// Enumerate `*.whl` files in `wheels_dir` and return the first one
/// (sorted), or `None` when the dir is missing or empty.
fn find_first_wheel(wheels_dir: &Path, package_label: &str) -> Result<Option<PathBuf>, BuildError> {
    if !wheels_dir.is_dir() {
        return Ok(None);
    }
    let mut wheels: Vec<PathBuf> = std::fs::read_dir(wheels_dir)
        .map_err(|e| BuildError::Other {
            package: package_label.to_string(),
            detail: format!(
                "failed to read wheels directory {}: {e}",
                wheels_dir.display()
            ),
        })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "whl"))
        .collect();
    wheels.sort();
    Ok(wheels.into_iter().next())
}

/// Run a `uv` command with the given args and cache directory. Returns the
/// raw process output (caller inspects `status`); the `Err` arm covers only
/// the failure to spawn `uv` at all.
fn run_uv(args: &[&str], uv_cache_dir: &Path) -> Result<std::process::Output, String> {
    let mut cmd = Command::new("uv");
    cmd.args(args)
        .env("UV_CACHE_DIR", uv_cache_dir.to_str().unwrap_or(""));
    // Resolve `streamlib` (and any other registry deps) from the SAME single
    // registry location cargo / `.slpkg` use, deriving `UV_INDEX` from it
    // rather than depending on an ambiently-set one. A link-mode build resolves
    // `streamlib` from its injected `[tool.uv.sources]` path override, which
    // wins over the index regardless of what is set here.
    if let Some(index) = derive_uv_index() {
        cmd.env("UV_INDEX", index);
    }
    cmd.output().map_err(|e| {
        format!(
            "failed to run uv (is uv installed?): {e}. Install with: \
             curl -LsSf https://astral.sh/uv/install.sh | sh"
        )
    })
}

/// The `UV_INDEX` pypi simple-index URL derived from the single configured
/// registry location (`STREAMLIB_PACKAGE_SOURCE`), or `None` when no registry is
/// configured — a link-mode / path-only build resolves `streamlib` from a
/// `[tool.uv.sources]` override instead of an index.
fn derive_uv_index() -> Option<String> {
    streamlib_idents::PackageSource::from_env().map(|c| c.pypi_simple_index_url())
}

fn path_to_str(path: &Path, package_label: &str) -> Result<String, BuildError> {
    path.to_str().map(|s| s.to_string()).ok_or_else(|| {
        build_failed(
            package_label,
            format!("path is not valid UTF-8: {}", path.display()),
        )
    })
}

fn build_failed(package: &str, detail: String) -> BuildError {
    BuildError::BuildFailed {
        tool: "venv".to_string(),
        package: package.to_string(),
        detail,
    }
}

/// Rewrite the staged `pyproject`'s `streamlib` dependency to resolve from the
/// linked checkout's Python SDK at `sdk_path` via `[tool.uv.sources]`. Called
/// only when the orchestrator resolved an active `streamlib link` — the
/// discovery + corrupt-marker handling live there, so this is a pure
/// manifest rewrite.
fn apply_link_override(
    pyproject: &Path,
    sdk_path: &Path,
    package_label: &str,
) -> Result<(), BuildError> {
    tracing::info!(
        sdk = %sdk_path.display(),
        "streamlib link active — pointing staged pyproject's streamlib dep at the linked checkout"
    );

    let body = std::fs::read_to_string(pyproject)
        .map_err(|e| build_failed(package_label, format!("read staged pyproject.toml: {e}")))?;
    let mut doc: toml_edit::DocumentMut = body.parse().map_err(|e: toml_edit::TomlError| {
        build_failed(package_label, format!("parse staged pyproject.toml: {e}"))
    })?;

    let mut source = toml_edit::InlineTable::new();
    source.insert(
        "path",
        toml_edit::Value::from(sdk_path.to_string_lossy().into_owned()),
    );
    source.insert("editable", toml_edit::Value::from(true));

    let tool = ensure_pyproject_table(doc.as_table_mut(), "tool", package_label)?;
    let uv = ensure_pyproject_table(tool, "uv", package_label)?;
    let sources = ensure_pyproject_table(uv, "sources", package_label)?;
    sources.insert(
        "streamlib",
        toml_edit::Item::Value(toml_edit::Value::InlineTable(source)),
    );

    std::fs::write(pyproject, doc.to_string())
        .map_err(|e| build_failed(package_label, format!("write staged pyproject.toml: {e}")))
}

/// Get-or-create `key` as a table, with a typed error when an existing
/// non-table value occupies the key (a malformed pyproject must never panic).
fn ensure_pyproject_table<'a>(
    table: &'a mut toml_edit::Table,
    key: &str,
    package_label: &str,
) -> Result<&'a mut toml_edit::Table, BuildError> {
    if !table.contains_key(key) {
        table.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    table[key].as_table_mut().ok_or_else(|| {
        build_failed(
            package_label,
            format!("staged pyproject.toml: `{key}` exists but is not a table"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{which_uv, write_fixture_streamlib_sdk};

    /// Stage a temp package dir that carries a Python runtime and
    /// path-depends on the fixture SDK (so the `import streamlib` probe +
    /// codegen run fully offline). Mirrors what `assemble_artifact` stages:
    /// a `python/` source dir + a `pyproject.toml`.
    fn write_staged_python_package(temp_dir: &Path, sdk: &Path) {
        std::fs::create_dir_all(temp_dir.join("python")).unwrap();
        std::fs::write(temp_dir.join("python").join("__init__.py"), "").unwrap();
        std::fs::write(
            temp_dir.join("pyproject.toml"),
            format!(
                r#"[project]
name = "mypkg"
version = "0.1.0"
dependencies = ["streamlib"]
[tool.uv.sources]
streamlib = {{ path = "{sdk}", editable = true }}
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"
[tool.hatch.build.targets.wheel]
packages = ["python"]
"#,
                sdk = sdk.display()
            ),
        )
        .unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn uv_index_derived_from_configured_registry_else_none() {
        // The orchestrator derives `UV_INDEX` from the single configured
        // registry location so the venv build no longer depends on an
        // ambiently-set one. SAFETY: `#[serial]` — no other thread races these
        // process-global env writes.
        let prev = std::env::var("STREAMLIB_PACKAGE_SOURCE").ok();

        // A local `file://` tree derives its pypi simple-index sub-URL.
        unsafe {
            std::env::set_var("STREAMLIB_PACKAGE_SOURCE", "file:///tmp/streamlib-tree");
        }
        assert_eq!(
            derive_uv_index().as_deref(),
            Some("file:///tmp/streamlib-tree/pypi/simple")
        );

        // An HTTP mount derives the same simple-index shape.
        unsafe {
            std::env::set_var("STREAMLIB_PACKAGE_SOURCE", "http://127.0.0.1:8799");
        }
        assert_eq!(
            derive_uv_index().as_deref(),
            Some("http://127.0.0.1:8799/pypi/simple")
        );

        // Unset → no derived index (dev / link / path-only build). Mentally
        // reverting the `from_env` gate to a hardcoded fallback would return
        // Some here and reintroduce the ambient dependence this removes.
        unsafe {
            std::env::remove_var("STREAMLIB_PACKAGE_SOURCE");
        }
        assert_eq!(derive_uv_index(), None);

        unsafe {
            match prev {
                Some(v) => std::env::set_var("STREAMLIB_PACKAGE_SOURCE", v),
                None => std::env::remove_var("STREAMLIB_PACKAGE_SOURCE"),
            }
        }
    }

    #[test]
    fn find_first_wheel_returns_none_for_missing_or_empty_dir() {
        // No wheels dir at all -> None (the common case: source-only package).
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("nope");
        assert_eq!(find_first_wheel(&missing, "pkg").unwrap(), None);

        // Present but empty -> None.
        let empty = temp.path().join("wheels");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(find_first_wheel(&empty, "pkg").unwrap(), None);
    }

    #[test]
    fn find_first_wheel_ignores_non_whl_files() {
        // Only non-wheel files present -> None. Reverting the `.whl`
        // extension filter would pick up the sdist/readme and return Some.
        let temp = tempfile::tempdir().unwrap();
        let wheels = temp.path().join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        std::fs::write(wheels.join("pkg-0.1.0.tar.gz"), b"").unwrap();
        std::fs::write(wheels.join("README.md"), b"").unwrap();
        assert_eq!(find_first_wheel(&wheels, "pkg").unwrap(), None);
    }

    #[test]
    fn find_first_wheel_picks_sorted_first_deterministically() {
        // Multiple wheels -> the lexicographically-first, regardless of
        // creation order. Reverting the `wheels.sort()` would make the
        // result depend on readdir order (non-deterministic).
        let temp = tempfile::tempdir().unwrap();
        let wheels = temp.path().join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        std::fs::write(wheels.join("zeta-9.0.0-py3-none-any.whl"), b"").unwrap();
        std::fs::write(wheels.join("alpha-1.0.0-py3-none-any.whl"), b"").unwrap();
        let picked = find_first_wheel(&wheels, "pkg").unwrap().unwrap();
        assert_eq!(picked.file_name().unwrap(), "alpha-1.0.0-py3-none-any.whl");
    }

    #[test]
    fn no_python_runtime_is_a_noop() {
        // A staged package with neither a python/ dir nor a pyproject.toml
        // gets no venv — the tail must not create one. Mentally reverting
        // the detection guard would create a venv (and fail / no-op) for a
        // schemas-only or Rust-only package.
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("streamlib.yaml"), "package:\n  name: x\n").unwrap();
        provision_python_venv(temp.path(), None, None, "tatolab/x")
            .expect("no-python must be a no-op");
        assert!(
            !temp.path().join(".venv").exists(),
            "no venv must be created for a non-Python staged package"
        );
    }

    #[test]
    fn provisions_venv_codegen_and_pyc_offline() {
        // End-to-end provision against an OFFLINE fixture SDK: proves the
        // tail yields {temp_dir}/.venv/bin/python, a populated
        // streamlib/_generated_ (codegen ran), and at least one .pyc
        // (compileall ran). Requires `uv` + a system Python 3.12 on PATH;
        // skips (does not fail) when `uv` is absent so the suite stays
        // green on a box without it.
        if which_uv().is_none() {
            eprintln!("skipping: `uv` not on PATH");
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let sdk = write_fixture_streamlib_sdk(root.path());

        let temp = tempfile::tempdir().unwrap();
        write_staged_python_package(temp.path(), &sdk);

        provision_python_venv(temp.path(), None, None, "tatolab/mypkg")
            .expect("provision must succeed offline against the fixture SDK");

        // Contract: interpreter at exactly {temp_dir}/.venv/bin/python.
        let venv_python = temp.path().join(".venv").join("bin").join("python");
        assert!(
            venv_python.exists(),
            "venv interpreter must exist at {}",
            venv_python.display()
        );

        // Codegen: the installed (editable) streamlib's _generated_ must be
        // populated beyond its __init__.py.
        let generated = sdk.join("src").join("streamlib").join("_generated_");
        let populated = generated
            .read_dir()
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name() != "__init__.py");
        assert!(
            populated,
            "streamlib/_generated_ must be populated by codegen at {}",
            generated.display()
        );

        // compileall: at least one .pyc somewhere under the venv.
        let has_pyc = find_any_pyc(&temp.path().join(".venv"));
        assert!(
            has_pyc,
            "compileall must have produced at least one .pyc in the venv"
        );
    }

    #[test]
    fn link_override_redirects_staged_pyproject_at_the_sdk_path() {
        // Given the linked checkout's Python SDK path (resolved by the
        // orchestrator), the staged pyproject's streamlib dep is redirected at
        // it via `[tool.uv.sources]`. Reverting the rewrite would leave the dep
        // resolving from the registry.
        let sdk = Path::new("/opt/streamlib-checkout/sdk/streamlib-python");

        let staged = tempfile::tempdir().unwrap();
        let pyproject = staged.path().join("pyproject.toml");
        std::fs::write(
            &pyproject,
            "[project]\nname = \"p\"\nversion = \"0.1.0\"\ndependencies = [\"streamlib\"]\n",
        )
        .unwrap();

        apply_link_override(&pyproject, sdk, "tatolab/p").unwrap();

        let body = std::fs::read_to_string(&pyproject).unwrap();
        assert!(
            body.contains("[tool.uv.sources]"),
            "sources table missing:\n{body}"
        );
        assert!(
            body.contains("/opt/streamlib-checkout/sdk/streamlib-python"),
            "override path missing:\n{body}"
        );
        assert!(
            body.contains("editable = true"),
            "editable flag missing:\n{body}"
        );
    }

    #[test]
    fn non_table_tool_key_in_staged_pyproject_is_a_typed_error_not_a_panic() {
        let staged = tempfile::tempdir().unwrap();
        let pyproject = staged.path().join("pyproject.toml");
        std::fs::write(&pyproject, "tool = 3\n").unwrap();

        let err = apply_link_override(&pyproject, Path::new("/opt/sdk"), "tatolab/p")
            .expect_err("non-table `tool` must be a typed error");
        assert!(
            format!("{err}").contains("not a table"),
            "error must name the malformed key, got: {err}"
        );
    }

    #[test]
    fn provision_applies_link_override_end_to_end_against_linked_sdk() {
        // Drive the override through `provision_python_venv` itself: passing the
        // offline fixture SDK as the link SDK path, the STAGED pyproject (no uv
        // source of its own) must be rewritten and the venv must install the
        // linked SDK. Requires `uv`.
        if which_uv().is_none() {
            eprintln!("skipping: `uv` not on PATH");
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let sdk = write_fixture_streamlib_sdk(root.path());

        // Staged package WITHOUT any uv source — resolution must come from
        // the injected link override.
        let staged = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(staged.path().join("python")).unwrap();
        std::fs::write(staged.path().join("python").join("__init__.py"), "").unwrap();
        std::fs::write(
            staged.path().join("pyproject.toml"),
            r#"[project]
name = "mypkg"
version = "0.1.0"
dependencies = ["streamlib"]
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"
[tool.hatch.build.targets.wheel]
packages = ["python"]
"#,
        )
        .unwrap();

        provision_python_venv(staged.path(), Some(&sdk), None, "tatolab/mypkg")
            .expect("provision must succeed against the linked fixture SDK");

        // The staged pyproject got the override injected…
        let body = std::fs::read_to_string(staged.path().join("pyproject.toml")).unwrap();
        assert!(
            body.contains("[tool.uv.sources]"),
            "override missing:\n{body}"
        );
        assert!(
            body.contains(&sdk.display().to_string()),
            "sdk path missing:\n{body}"
        );
        // …and the venv exists (streamlib resolved from the linked SDK).
        assert!(
            staged
                .path()
                .join(".venv")
                .join("bin")
                .join("python")
                .exists()
        );
    }

    fn find_any_pyc(dir: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_dir() {
                if find_any_pyc(&path) {
                    return true;
                }
            } else if path.extension().is_some_and(|e| e == "pyc") {
                return true;
            }
        }
        false
    }
}
