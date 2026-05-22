// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-rustc-version / cross-dep-graph dlopen integration test for
//! issue #927 — the empirical gate for PR #918's β-shape Phase D work
//! and the structural cross-repo plugin distribution claim from
//! CLAUDE.md → "Plugin Distribution Model — the cross-repo dream".
//!
//! What this test locks: a cdylib built in a **standalone Cargo
//! workspace** (`libs/streamlib-cross-rustc-fixture/`, with its own
//! `[workspace]` table and its own `Cargo.lock` resolving distinctly
//! divergent transitive crate versions vs the host workspace) loads
//! cleanly into a `Runner` through `load_project(...)` and exercises
//! Create + Clone + Drop of every #917 β-shape return type without
//! panic.
//!
//! Per the issue body's "Approach" section, the fixture rides
//! Option 1 (same-rustc, deliberately mismatched dep graph).
//! Cross-rustc-version independence is **structural by
//! construction**: every type that crosses the cdylib boundary in
//! PR #918 is `#[repr(C)]` with a byte-pinned layout regression test
//! in `streamlib-plugin-abi`. When the remaining β-shape gaps are
//! closed (Phase E #907 + Phase F #908) a follow-up CI matrix can
//! rebuild the fixture under a different rustc minor to upgrade
//! Option 1 → Option 2 with no source changes here.
//!
//! Like `load_project_dylib_gpu_acquire`, this test requires a
//! working Vulkan device on the test host (the fixture's `start()`
//! creates a real `VulkanComputeKernel` from SPIR-V). CI has no GPU
//! runner planned (see `project_ci_strategy_no_gpu`); the test runs
//! locally and fails clean on GPU-less hosts with a Vulkan device-
//! init error.
//!
//! Mentally-revert lock: change any β-shape in #918 back to an
//! `Arc<HostInternalType>` raw-pointer transit (the pre-#917 shape)
//! and either the host vtable's `clone_<type>` / `drop_<type>` slot
//! goes back to operating on a host-internal layout, or the
//! cdylib-side β-shape struct loses its `#[repr(C)]` pin — either way,
//! the deliberately-divergent transitive dep graph between the
//! fixture and the host workspace means the host and cdylib see
//! different `Arc<Inner>` allocation header layouts, and the
//! clone/drop arithmetic corrupts the refcount. The "OK" sentinel
//! the fixture writes is the gate; corruption surfaces as a panic
//! at the FFI boundary (reported as `ERR:` in the result file) or
//! as an outright crash from `Arc::decrement_strong_count` on a
//! mismatched header.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use serial_test::serial;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;
use streamlib_engine::core::runtime::host_target_triple;

fn copy_dir_contents(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dst_entry = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_contents(&entry.path(), &dst_entry);
        } else {
            std::fs::copy(entry.path(), &dst_entry).unwrap();
        }
    }
}

#[test]
#[serial]
fn dlopen_cross_rustc_fixture_round_trips_every_beta_shape() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let fixture_src = workspace_root.join("libs/streamlib-cross-rustc-fixture");
    let fixture_manifest = fixture_src.join("Cargo.toml");
    assert!(
        fixture_manifest.exists(),
        "fixture manifest missing at {}",
        fixture_manifest.display()
    );

    // Build the fixture in its own sibling workspace. The
    // `--manifest-path` argument keeps cargo in this crate's own
    // `[workspace]` scope — resolution uses the fixture's
    // `Cargo.lock` (deliberately divergent transitive versions vs
    // the host), not the host's. Use a dedicated `--target-dir` so
    // the fixture's artifacts never collide with the host's
    // `target/`.
    let fixture_target_dir = workspace_root
        .join("target")
        .join("cross-rustc-fixture");
    let status = std::process::Command::new(env!("CARGO"))
        .args([
            "build",
            "--manifest-path",
            fixture_manifest.to_str().unwrap(),
            "--target-dir",
            fixture_target_dir.to_str().unwrap(),
        ])
        .status()
        .expect("invoking cargo build for streamlib-cross-rustc-fixture");
    assert!(
        status.success(),
        "cargo build of the cross-rustc fixture must succeed — see stderr above"
    );

    let dylib_ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    let dylib_name = format!("libstreamlib_cross_rustc_fixture.{dylib_ext}");
    let built_dylib = fixture_target_dir.join("debug").join(&dylib_name);
    assert!(
        built_dylib.exists(),
        "fixture cdylib not at expected path: {}",
        built_dylib.display()
    );

    // Stage the fixture as a streamlib project the runtime can load
    // path-based. Mirror the host workspace's directory hierarchy
    // (`libs/streamlib-cross-rustc-fixture/` + `packages/core/`) inside
    // the tempdir so the fixture's `streamlib.yaml` patch entry
    // `path: ../../packages/core` resolves to the staged `core` peer.
    let tmp = tempfile::tempdir().unwrap();
    let fixture_dst = tmp.path().join("libs/streamlib-cross-rustc-fixture");
    let core_dst = tmp.path().join("packages/core");

    std::fs::create_dir_all(&fixture_dst).unwrap();
    std::fs::copy(
        fixture_src.join("streamlib.yaml"),
        fixture_dst.join("streamlib.yaml"),
    )
    .unwrap();
    copy_dir_contents(&fixture_src.join("schemas"), &fixture_dst.join("schemas"));

    let core_src = workspace_root.join("packages/core");
    std::fs::create_dir_all(&core_dst).unwrap();
    std::fs::copy(
        core_src.join("streamlib.yaml"),
        core_dst.join("streamlib.yaml"),
    )
    .unwrap();
    copy_dir_contents(&core_src.join("schemas"), &core_dst.join("schemas"));

    let triple_dir = fixture_dst.join("lib").join(host_target_triple());
    std::fs::create_dir_all(&triple_dir).unwrap();
    std::fs::copy(&built_dylib, triple_dir.join(&dylib_name)).unwrap();

    let output_path = tmp.path().join("beta_shape_round_trip.txt");
    let output_path_str = output_path.to_string_lossy().to_string();

    // Load the cross-rustc fixture into a fresh runtime. `load_project`
    // dlopens the cdylib, invokes its `STREAMLIB_PLUGIN` symbol, and
    // registers `BetaShapeRoundTripProcessor` against the runtime's
    // schema/processor registries.
    let runtime = Runner::new().unwrap();
    runtime
        .load_project(&fixture_dst)
        .expect("load_project must succeed against the cross-rustc cdylib");

    let ident = schema_ident!(
        "tatolab",
        "cross-rustc-fixture",
        "BetaShapeRoundTripProcessor",
        "1.0.0"
    );

    runtime
        .add_processor(ProcessorSpec::new(
            ident,
            json!({ "output_path": output_path_str }),
        ))
        .expect("add_processor must succeed for the dlopened BetaShapeRoundTripProcessor");

    runtime
        .start()
        .expect("runtime.start() must succeed (requires Vulkan device on this host)");

    // The processor's `start()` runs synchronously inside the
    // runtime's manual-processor spawn path — by the time `start()`
    // returns, the β-shape Create + Clone + Drop sequence has either
    // completed and written the OK file or panicked at an FFI
    // boundary (caught by `run_host_extern_c` and surfaced as the
    // setup/start error). Poll briefly to absorb scheduling jitter.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !output_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    runtime.stop().ok();

    assert!(
        output_path.exists(),
        "BetaShapeRoundTripProcessor.start() did not write {} within 10s — \
         either the cdylib's `start` lifecycle didn't fire, or the β-shape \
         Create/Clone/Drop path panicked at the FFI boundary",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        !contents.starts_with("ERR:"),
        "BetaShapeRoundTripProcessor reported an error from the β-shape round-trip: {contents}"
    );

    // Body format: "OK\n<type>:<status>\n..." — one line per β-shape
    // exercised. Lock that every expected type round-tripped.
    assert!(
        contents.starts_with("OK\n"),
        "first line must be 'OK', got: {contents}"
    );
    for expected in [
        "TextureRing:OK",
        "RhiColorConverter:OK",
        "RhiCommandRecorder:OK",
        "VulkanComputeKernel:OK",
    ] {
        assert!(
            contents.contains(expected),
            "missing β-shape round-trip line {expected:?} — full body:\n{contents}"
        );
    }
}
