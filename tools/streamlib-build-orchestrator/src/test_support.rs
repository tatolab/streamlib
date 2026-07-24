// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared offline test fixtures for the orchestrator + venv-provisioning
//! tests. Every helper here keeps the test fully offline: the `streamlib`
//! SDK is a local path-dep fixture (hatchling, ships `streamlib.yaml` + a
//! trivial dependency-free schema + an empty `_generated_/`) so
//! `uv pip install` resolves with NO network / the package source.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Build a self-contained, OFFLINE-installable fixture `streamlib` SDK
/// package under `root/sdk`: ships `streamlib.yaml` + one trivial,
/// dependency-free schema + an empty `_generated_/`, installable via uv
/// from a local path with no network. Returns the SDK dir.
///
/// This stands in for the real package-source-resolved SDK. The real SDK install
/// pulls `streamlib` by version from the package source (network); a fixture SDK keeps
/// the test fully offline while still exercising the exact provision flow:
/// install → probe `import streamlib` → codegen against the installed
/// `streamlib.yaml` → compileall.
pub(crate) fn write_fixture_streamlib_sdk(root: &Path) -> PathBuf {
    let sdk = root.join("sdk");
    let pkg = sdk.join("src").join("streamlib");
    std::fs::create_dir_all(pkg.join("_generated_")).unwrap();
    std::fs::create_dir_all(pkg.join("schemas")).unwrap();
    std::fs::write(
        sdk.join("pyproject.toml"),
        r#"[project]
name = "streamlib"
version = "0.1.0"
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"
[tool.hatch.build.targets.wheel]
packages = ["src/streamlib"]
[tool.hatch.build.targets.wheel.force-include]
"src/streamlib/streamlib.yaml" = "streamlib/streamlib.yaml"
"src/streamlib/schemas" = "streamlib/schemas"
"#,
    )
    .unwrap();
    std::fs::write(pkg.join("__init__.py"), "").unwrap();
    std::fs::write(pkg.join("_generated_").join("__init__.py"), "").unwrap();
    std::fs::write(
        pkg.join("streamlib.yaml"),
        "package:\n  org: tatolab\n  name: streamlib\n  version: 0.1.0\nschemas:\n  TestSchema:\n    file: schemas/test_schema.yaml\n",
    )
    .unwrap();
    std::fs::write(
        pkg.join("schemas").join("test_schema.yaml"),
        "metadata:\n  type: TestSchema\n  expected_payload_bytes: 1024\nproperties:\n  value:\n    type: uint32\n",
    )
    .unwrap();
    sdk
}

/// Write a pure-Python SOURCE package at `pkg_dir`: a `streamlib.yaml`
/// declaring one Python runtime processor, the processor's `.py`
/// entrypoint, and a `pyproject.toml` that path-depends on the fixture
/// `streamlib` SDK (so the `import streamlib` probe + codegen run fully
/// offline). This is what a developer hands `materialize` as a
/// `BuildSource::PackageDir`; `assemble_artifact` stages the full source
/// tree and the venv tail provisions `.venv` against it. No Rust runtime,
/// so the `IfStale` staleness skip applies.
pub(crate) fn write_python_source_package(pkg_dir: &Path, sdk: &Path) {
    std::fs::write(
        pkg_dir.join("streamlib.yaml"),
        r#"package:
  org: tatolab
  name: py-source
  version: 0.1.0
processors:
  - name: PyProc
    description: "offline python processor fixture"
    runtime: python
    execution: manual
    entrypoint: "pyproc:PyProc"
    inputs:
      - name: in0
        schema: any
    outputs:
      - name: out0
        schema: any
"#,
    )
    .unwrap();
    // The processor entrypoint `pyproc:PyProc` → `pyproc.py` at the root.
    std::fs::write(pkg_dir.join("pyproc.py"), "class PyProc:\n    pass\n").unwrap();
    std::fs::write(
        pkg_dir.join("pyproject.toml"),
        format!(
            r#"[project]
name = "py-source"
version = "0.1.0"
dependencies = ["streamlib"]
[tool.uv.sources]
streamlib = {{ path = "{sdk}", editable = true }}
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"
[tool.hatch.build.targets.wheel]
packages = ["pyproc.py"]
"#,
            sdk = sdk.display()
        ),
    )
    .unwrap();
}

/// `Some(())` when `uv` is on PATH and runnable; `None` otherwise. Tests
/// that need a real venv skip (don't fail) when `uv` is absent.
pub(crate) fn which_uv() -> Option<()> {
    Command::new("uv")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| ())
}
