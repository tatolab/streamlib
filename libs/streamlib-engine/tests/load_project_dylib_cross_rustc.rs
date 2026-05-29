// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-rustc-version / cross-dep-graph dlopen integration test for
//! issue #927.
//!
//! Companion to PR #918's β-shape Phase D work. The fixture under
//! `libs/streamlib-cross-rustc-fixture/` is a standalone Cargo
//! workspace (own `[workspace]` table, own `Cargo.lock`) that pins
//! older transitive `serde` / `tracing` than the host workspace
//! resolves, so building it produces a `.so` against a deliberately
//! divergent crate graph.
//!
//! What the test actually surfaces:
//!
//! - The fixture's `Cargo.lock` resolves to materially different
//!   transitive versions than the host's (asserted at runtime —
//!   see [`assert_dep_graph_divergence`]).
//! - `cargo build` against that fixture produces a `.so`.
//! - `Runner::add_module_with(...)` accepts the `STREAMLIB_PLUGIN`
//!   symbol from the divergently-compiled cdylib.
//! - Each #918 β-shape return type (`TextureRing`,
//!   `RhiColorConverter`, `RhiCommandRecorder`, `VulkanComputeKernel`,
//!   `VulkanGraphicsKernel`, plus `VulkanAccelerationStructure` and
//!   `VulkanRayTracingKernel` when the host supports ray tracing) is
//!   constructed, cloned (where applicable — `RhiCommandRecorder` is
//!   the Box-handle `!Clone` shape), and dropped from cdylib code
//!   without panic — running through the FFI vtable, not the
//!   in-process Boxed path, by wrapping the sweep in
//!   `gpu_limited_access().escalate(...)`.
//! - The sweep runs once in `start()`. `setup()` is intentionally
//!   empty: both lifecycles wrap the same `GpuContextFullAccess`
//!   β-shape with the same host-vtable instance, so doubling the
//!   sweep adds ~15s of BLAS + RT-kernel construction without
//!   exercising a distinct vtable surface.
//!
//! What this test does NOT lock on its own — these are guarded
//! elsewhere:
//!
//! - Per-`extern "C"` vtable slot byte offset →
//!   `streamlib-plugin-abi`'s `offset_of!` layout regression tests.
//! - Host-side callback bodies for each clone/drop slot → the
//!   engine's own per-type unit tests
//!   (`vulkan_compute_kernel::tests` etc.).
//! - True cross-rustc-version compatibility → structural by
//!   `#[repr(C)]` design; upgrading Option 1 → Option 2 of the
//!   issue's "Approach" (rustc-minor matrix in CI) requires no
//!   source change to this fixture.
//!
//! Requires a working Vulkan device on the test host. CI has no GPU
//! runner planned (`project_ci_strategy_no_gpu`); the test runs
//! locally and fails clean on GPU-less hosts with a Vulkan-init
//! error. Ray-tracing coverage is conditional on
//! `supports_ray_tracing_pipeline()` — non-RT hosts record SKIP
//! lines and the test accepts them as soft-pass.

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use serial_test::serial;
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::{BuildPolicy, Strategy, Runner};
use streamlib::sdk::RunnerAutoBuild;
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

/// Returns the resolved version string for `crate_name` in
/// `lockfile_path`, scanning the first `[[package]]` entry whose
/// `name =` line matches.
fn lockfile_version(lockfile_path: &Path, crate_name: &str) -> Option<String> {
    let body = std::fs::read_to_string(lockfile_path).ok()?;
    let needle = format!("name = \"{crate_name}\"");
    let mut iter = body.lines();
    while let Some(line) = iter.next() {
        if line.trim() == needle {
            for next in iter.by_ref() {
                if let Some(rest) = next.trim().strip_prefix("version = \"") {
                    if let Some(v) = rest.strip_suffix('"') {
                        return Some(v.to_string());
                    }
                }
                if next.starts_with("[[package]]") {
                    break;
                }
            }
            return None;
        }
    }
    None
}

/// Lock that the fixture's resolved Cargo.lock materially diverges
/// from the host workspace's resolved Cargo.lock. If a future
/// refactor accidentally aligns the two (e.g. someone unpins the
/// fixture's deps), the "cross-dep-graph" load-bearing claim of the
/// test silently weakens — this assertion catches that drift.
fn assert_dep_graph_divergence(workspace_root: &Path, fixture_lock: &Path) {
    let host_lock = workspace_root.join("Cargo.lock");
    assert!(
        host_lock.exists(),
        "host Cargo.lock missing at {} — workspace not built?",
        host_lock.display()
    );
    assert!(
        fixture_lock.exists(),
        "fixture Cargo.lock missing at {} — cargo build did not produce one?",
        fixture_lock.display()
    );

    // Crates pinned in the fixture's Cargo.toml to specific older
    // versions vs the host workspace's open `workspace = true` /
    // unpinned resolution. At least these two must differ; the goal
    // is "deliberate dep-graph divergence is real," not "every dep
    // differs."
    let probe_crates = ["serde", "tracing"];
    let mut diverged = Vec::new();
    for crate_name in probe_crates {
        let host_v = lockfile_version(&host_lock, crate_name);
        let fixture_v = lockfile_version(fixture_lock, crate_name);
        if host_v.is_some() && fixture_v.is_some() && host_v != fixture_v {
            diverged.push(format!(
                "{crate_name}: host={} vs fixture={}",
                host_v.unwrap(),
                fixture_v.unwrap()
            ));
        }
    }
    assert!(
        !diverged.is_empty(),
        "fixture Cargo.lock does NOT diverge from host Cargo.lock on any of {probe_crates:?} \
         — the cross-dep-graph claim of this test is weakened. Did someone unpin the fixture's deps?"
    );
    eprintln!(
        "cross-rustc fixture vs host dep-graph divergence confirmed: {}",
        diverged.join(", ")
    );
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

    // Build the fixture in its own sibling workspace via a separate
    // `--target-dir` so its artifacts don't collide with the host's
    // `target/`. `--manifest-path` keeps cargo inside the fixture's
    // own `[workspace]` scope, so resolution uses the fixture's own
    // (gitignored, generated-fresh) `Cargo.lock`.
    let fixture_target_dir = workspace_root.join("target").join("cross-rustc-fixture");
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

    // With the fixture now built, cargo has written its `Cargo.lock`
    // alongside the manifest. Assert the resolved versions diverge
    // from the host workspace's resolved versions on at least one
    // probe crate so a future refactor that accidentally aligns the
    // two trips here instead of silently weakening the test.
    assert_dep_graph_divergence(workspace_root, &fixture_src.join("Cargo.lock"));

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

    // Stage as a streamlib project the runtime can load path-based.
    // Mirror the host workspace's directory hierarchy
    // (`libs/streamlib-cross-rustc-fixture/` + `packages/core/`) inside
    // the tempdir so the fixture's `streamlib.yaml` patch entry
    // `path: ../../packages/core` resolves.
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

    let runtime = Runner::with_auto_build().unwrap();
    runtime
        .add_module_with_blocking(
            module_ident_any_version!("tatolab", "cross-rustc-fixture"),
            Strategy::Path { path: fixture_dst.clone(), build: BuildPolicy::NeverBuild },
        )
        .expect("add_module_with must succeed against the cross-rustc cdylib");

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

    // Manual-processor `setup()` runs the first β-shape sweep; the
    // result is stashed on the processor instance and combined with
    // `start()`'s sweep into the output file. Poll briefly for the
    // file to absorb scheduling jitter.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !output_path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }

    runtime.stop().ok();

    assert!(
        output_path.exists(),
        "BetaShapeRoundTripProcessor did not write {} within 15s — \
         either the cdylib's setup/start lifecycle didn't fire, or the \
         β-shape Create/Clone/Drop path panicked at the FFI boundary",
        output_path.display()
    );

    let contents = std::fs::read_to_string(&output_path).unwrap();
    assert!(
        !contents.starts_with("ERR:"),
        "BetaShapeRoundTripProcessor reported an error from the β-shape round-trip:\n{contents}"
    );
    assert!(
        contents.starts_with("OK\n"),
        "first line must be 'OK', got: {contents}"
    );

    // The five unconditional β-shapes run on every test host.
    for ty in [
        "TextureRing",
        "RhiColorConverter",
        "RhiCommandRecorder",
        "VulkanComputeKernel",
        "VulkanGraphicsKernel",
    ] {
        let needle = format!("{ty}:OK");
        assert!(
            contents.contains(&needle),
            "missing β-shape round-trip line {needle:?} — full body:\n{contents}"
        );
    }

    // Consumer-rhi POD round-trip (#1039): the divergently-compiled
    // fixture asserts every `streamlib-consumer-rhi` POD type
    // (TextureFormat / TextureUsages / PixelFormat / VulkanLayout)
    // produces the same scalar discriminants the host expects.
    // A failure here means a consumer-rhi POD got re-numbered under
    // the fixture's compilation but not the host's (or vice versa) —
    // the exact kind of silent drift `#[repr(...)]` + discriminant
    // pinning exists to catch. The fixture suppresses the OK line
    // when any per-variant check fails, so the OK assert is
    // sufficient to gate both shapes of failure (missing line / FAIL
    // lines accumulated); the full body is included in the panic
    // message so individual `ConsumerRhi:<name>:FAIL` lines surface
    // in the failure report.
    assert!(
        contents.contains("ConsumerRhiPodRoundTrip:OK"),
        "missing consumer-rhi POD round-trip OK line — either the \
         sweep never ran, or one or more per-variant checks failed \
         (look for `ConsumerRhi:<name>:FAIL` lines below). Full \
         body:\n{contents}"
    );

    // `Texture::native_handle` (#957, Phase F) is gated on EGL exposing
    // a render-target-capable DRM modifier
    // (`docs/learnings/nvidia-egl-dmabuf-render-target.md`). Accept
    // either OK or SKIPPED_NO_DMA_BUF; anything else (no line at all,
    // ERR, etc.) fails.
    {
        let ok = "Texture::native_handle:OK";
        let skipped = "Texture::native_handle:SKIPPED_NO_DMA_BUF";
        assert!(
            contents.contains(ok) || contents.contains(skipped),
            "missing β-shape round-trip line for Texture::native_handle: \
             expected one of {ok:?} or {skipped:?} — full body:\n{contents}"
        );
    }

    // VulkanAccelerationStructure + VulkanRayTracingKernel are
    // RT-feature-gated. Accept either OK or SKIPPED_NO_RT_SUPPORT;
    // anything else (no line at all, ERR, etc.) fails.
    for ty in ["VulkanAccelerationStructure", "VulkanRayTracingKernel"] {
        let ok = format!("{ty}:OK");
        let skipped = format!("{ty}:SKIPPED_NO_RT_SUPPORT");
        assert!(
            contents.contains(&ok) || contents.contains(&skipped),
            "missing β-shape round-trip line for {ty}: expected one of \
             {ok:?} or {skipped:?} — full body:\n{contents}"
        );
    }
}
