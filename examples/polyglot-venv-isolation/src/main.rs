// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-package Python venv isolation proof.
//!
//! Two distinct Python streamlib packages pin *conflicting* numpy
//! versions — `pkg-a` pins `numpy==1.26.4` (NumPy 1.x ABI) and `pkg-b`
//! pins `numpy==2.1.3` (NumPy 2.x ABI). Those two pins can never
//! co-resolve in a single shared environment; the only way both
//! processors can run in one pipeline is if each package gets its own
//! per-package virtual environment.
//!
//! This is exactly what the build orchestrator's venv provisioning tail
//! does: each Python package is materialized into the streamlib package
//! cache with a self-contained `{staged}/.venv` holding that package's
//! own resolved dependencies. The engine spawn path then launches each
//! processor against its own venv interpreter — it no longer creates or
//! mutates venvs at spawn time.
//!
//! Each processor imports numpy at setup and writes the observed
//! `numpy.__version__` to a host-visible output file. After the run this
//! binary reads both files and asserts:
//!
//!   * pkg-a's processor saw exactly `1.26.4`
//!   * pkg-b's processor saw exactly `2.1.3`
//!
//! Both holding simultaneously proves each package resolved its own venv
//! independently — and, transitively, that the orchestrator-provisioned
//! venv + the engine↔SDK protocol handshake all work end to end (the
//! processors could not have reached `setup()` otherwise).
//!
//! The pipeline is deliberately GPU-free and hardware-free: two
//! continuous processors, each self-paced by the runner's MonotonicTimer,
//! no camera / display / Vulkan. The point is venv isolation, not GPU.
//!
//! Prerequisite: the per-package venv installs `streamlib` from the static registry
//! pypi registry by version (it is declared like any dependency, not
//! injected). The host environment must therefore expose the static registry pypi
//! index to `uv` via a container-level `UV_INDEX` / `pip.conf`, e.g.
//!   UV_INDEX="tatolab=file:///path/to/registry-tree/pypi/simple"
//! numpy resolves from public PyPI normally (a truly-external dep).
//!
//! Run:
//!   cargo run -p polyglot-venv-isolation-scenario

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use streamlib::sdk::descriptors::{ModuleIdent, SchemaIdent};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Runner, Strategy};
use streamlib::sdk::RunnerAutoBuild;

const RUN_DURATION: Duration = Duration::from_secs(2);

/// One package under test: its module ident, the processor schema, the
/// numpy version its pyproject pins, and the example-local source dir.
/// `module_ident` / `processor_ident` are fns because the
/// `module_ident_any_version!` / `schema_ident_any_version!` macros take
/// string literals (they parse the org / name at compile time), so each
/// package gets its own literal-arg constructor.
struct PackageUnderTest {
    label: &'static str,
    module_ident: fn() -> ModuleIdent,
    processor_ident: fn() -> Result<SchemaIdent>,
    expected_numpy: &'static str,
    source_subdir: &'static str,
}

fn pkg_a_module() -> ModuleIdent {
    module_ident_any_version!("tatolab", "polyglot-venv-isolation-pkg-a")
}

fn pkg_a_ident() -> Result<SchemaIdent> {
    streamlib::sdk::schema_ident_any_version!(
        "tatolab",
        "polyglot-venv-isolation-pkg-a",
        "NumpyVersionReporter"
    )
}

fn pkg_b_module() -> ModuleIdent {
    module_ident_any_version!("tatolab", "polyglot-venv-isolation-pkg-b")
}

fn pkg_b_ident() -> Result<SchemaIdent> {
    streamlib::sdk::schema_ident_any_version!(
        "tatolab",
        "polyglot-venv-isolation-pkg-b",
        "NumpyVersionReporter"
    )
}

const PACKAGES: &[PackageUnderTest] = &[
    PackageUnderTest {
        label: "pkg-a",
        module_ident: pkg_a_module,
        processor_ident: pkg_a_ident,
        expected_numpy: "1.26.4",
        source_subdir: "pkg-a/python",
    },
    PackageUnderTest {
        label: "pkg-b",
        module_ident: pkg_b_module,
        processor_ident: pkg_b_ident,
        expected_numpy: "2.1.3",
        source_subdir: "pkg-b/python",
    },
];

fn main() -> ExitCode {
    match run() {
        Ok(()) => {
            println!("✓ per-package venv isolation verified — each package resolved its own pinned numpy");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("✗ scenario failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let pid = std::process::id();

    println!("=== Polyglot per-package venv isolation scenario ===");
    println!("Run length: {RUN_DURATION:?}");

    let runtime = Runner::with_auto_build()?;

    // Per-package output files the processors write their observed numpy
    // version into.
    let mut output_files: Vec<PathBuf> = Vec::with_capacity(PACKAGES.len());

    for pkg in PACKAGES {
        // Each Python sub-package is example-local (a sibling of this
        // example crate) and not workspace-staged, so it is resolved by
        // its manifest directory. `IfStale` triggers the build
        // orchestrator's materialize tail, which provisions the
        // package's own `{staged}/.venv` with its own pinned numpy.
        runtime.add_module_with_blocking(
            (pkg.module_ident)(),
            Strategy::Path {
                path: manifest_dir.join(pkg.source_subdir),
                build: BuildPolicy::IfStale,
            },
        )?;

        let output_file = std::env::temp_dir()
            .join(format!("polyglot-venv-isolation-{}-{pid}.json", pkg.label));
        let _ = std::fs::remove_file(&output_file);
        println!("{} output file: {}", pkg.label, output_file.display());

        let processor = runtime.add_processor(ProcessorSpec::new(
            (pkg.processor_ident)()?,
            serde_json::json!({
                "output_file": output_file.to_string_lossy(),
            }),
        ))?;
        println!("+ {} NumpyVersionReporter: {processor}", pkg.label);

        output_files.push(output_file);
    }

    runtime.start()?;
    std::thread::sleep(RUN_DURATION);
    runtime.stop()?;

    // After the run, assert each package observed exactly its own pinned
    // numpy version. Both holding simultaneously is the isolation proof:
    // 1.26.4 and 2.1.3 cannot coexist in one environment.
    let mut all_ok = true;
    for (pkg, output_file) in PACKAGES.iter().zip(output_files.iter()) {
        let observed = read_numpy_version(output_file)?;
        if observed == pkg.expected_numpy {
            println!(
                "✓ {}: observed numpy {observed} (expected {})",
                pkg.label, pkg.expected_numpy
            );
        } else {
            all_ok = false;
            eprintln!(
                "✗ {}: observed numpy {observed}, expected {} — venv isolation broken",
                pkg.label, pkg.expected_numpy
            );
        }
    }

    if !all_ok {
        return Err(Error::Runtime(
            "one or more packages did not resolve its own pinned numpy version".into(),
        ));
    }
    Ok(())
}

fn read_numpy_version(path: &std::path::Path) -> Result<String> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        Error::Runtime(format!(
            "processor did not write {} — setup/teardown may have failed: {e}",
            path.display()
        ))
    })?;
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| Error::Runtime(format!("output file is not valid JSON: {e}")))?;
    v.get("numpy_version")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Runtime("missing numpy_version in output file".into()))
}
