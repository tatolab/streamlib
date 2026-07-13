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
//! Prerequisite: each package's `python/pyproject.toml` declares `streamlib`
//! like any dependency (it is not injected). The SDK resolves by version from
//! PyPI once published; for local development `streamlib link --engine`
//! (run by `./setup.sh`) points `uv` at the in-repo SDK instead, so no registry
//! configuration is needed. numpy resolves from public PyPI normally (a
//! truly-external dep).
//!
//! Run:
//!   cargo run -p polyglot-venv-isolation-scenario

#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::{ProcessorSpec, ProcessorTypeReference};
use streamlib::sdk::runtime::Runner;

const RUN_DURATION: Duration = Duration::from_secs(2);

/// One package under test: the processor schema and the numpy version its
/// pyproject pins. `processor_ref` is a fn because the `processor_type_ref!`
/// macro takes string literals (it parses the org / name at compile time), so
/// each package gets its own literal-arg constructor.
struct PackageUnderTest {
    label: &'static str,
    processor_ref: fn() -> ProcessorTypeReference,
    expected_numpy: &'static str,
}

fn pkg_a_ref() -> ProcessorTypeReference {
    processor_type_ref!(
        "tatolab",
        "polyglot-venv-isolation-pkg-a",
        "NumpyVersionReporter"
    )
}

fn pkg_b_ref() -> ProcessorTypeReference {
    processor_type_ref!(
        "tatolab",
        "polyglot-venv-isolation-pkg-b",
        "NumpyVersionReporter"
    )
}

const PACKAGES: &[PackageUnderTest] = &[
    PackageUnderTest {
        label: "pkg-a",
        processor_ref: pkg_a_ref,
        expected_numpy: "1.26.4",
    },
    PackageUnderTest {
        label: "pkg-b",
        processor_ref: pkg_b_ref,
        expected_numpy: "2.1.3",
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
    let pid = std::process::id();

    println!("=== Polyglot per-package venv isolation scenario ===");
    println!("Run length: {RUN_DURATION:?}");

    let runtime = Runner::with_auto_build()?;

    // No module-loading call: this example's two example-local Python packages
    // (`./pkg-a/python` + `./pkg-b/python`, each pinning its own conflicting
    // numpy) live in this app's `streamlib_modules/` folder (populated by
    // `./setup.sh`). The runtime lazily discovers + loads each on the first
    // `processor_type_ref!` reference, provisioning each package's own venv.

    // Per-package output files the processors write their observed numpy
    // version into.
    let mut output_files: Vec<PathBuf> = Vec::with_capacity(PACKAGES.len());

    for pkg in PACKAGES {
        let output_file = std::env::temp_dir()
            .join(format!("polyglot-venv-isolation-{}-{pid}.json", pkg.label));
        let _ = std::fs::remove_file(&output_file);
        println!("{} output file: {}", pkg.label, output_file.display());

        let processor = runtime.add_processor(ProcessorSpec::new(
            (pkg.processor_ref)(),
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
