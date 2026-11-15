// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// ---- Pipeline cache tests ------------------------------------------------
//
// Every test that mutates `STREAMLIB_PIPELINE_CACHE_DIR` is marked
// `#[serial(streamlib_pipeline_cache_env)]` so they serialize against
// each other regardless of whether they skip the GPU device queue
// mutex. The `pipeline_cache_dir()` resolution path doesn't need a
// Vulkan device, so the queue-mutex serialization isn't sufficient
// by itself. Required for soundness under Rust 2024's `unsafe
// std::env::set_var` — concurrent reads from sibling test threads
// would otherwise be UB.

use serial_test::serial;
use streamlib_runtime_client_contract::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};

use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
use crate::vulkan::rhi::kernels::vulkan_pipeline_cache_on_disk::{
    PIPELINE_CACHE_DIR_ENV, pipeline_cache_dir,
};

fn unique_cache_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "streamlib-pipeline-cache-{label}-{}",
        mint_machine_global_unique_name_suffix()
    ))
}

/// Run `f` with `STREAMLIB_PIPELINE_CACHE_DIR` set to `dir`, restoring
/// the previous value (or unsetting) afterward. Callers MUST mark
/// the test `#[serial(streamlib_pipeline_cache_env)]` — see the
/// section comment above for why.
fn with_pipeline_cache_dir<R>(dir: &Path, f: impl FnOnce() -> R) -> R {
    let prev = std::env::var(PIPELINE_CACHE_DIR_ENV).ok();
    // SAFETY: callers serialize via `#[serial(streamlib_pipeline_cache_env)]`,
    // so concurrent env-var reads/writes from sibling tests are not
    // possible.
    unsafe { std::env::set_var(PIPELINE_CACHE_DIR_ENV, dir) };
    let r = f();
    // SAFETY: same as above.
    unsafe {
        match prev {
            Some(v) => std::env::set_var(PIPELINE_CACHE_DIR_ENV, v),
            None => std::env::remove_var(PIPELINE_CACHE_DIR_ENV),
        }
    }
    r
}

#[test]
#[serial(streamlib_pipeline_cache_env)]
fn cache_file_path_is_stable_for_same_spirv_and_distinct_for_different() {
    let dir = unique_cache_dir("path-stability");
    with_pipeline_cache_dir(&dir, || {
        let main = c"main";
        let p1 = pipeline_cache_file_path(blend_spv(1), main, None);
        let p2 = pipeline_cache_file_path(blend_spv(1), main, None);
        let p4 = pipeline_cache_file_path(blend_spv(4), main, None);
        assert_eq!(p1, p2, "same SPIR-V must hash to same path");
        assert_ne!(p1, p4, "different SPIR-V must hash to different paths");
        // One module backs one pipeline per entry point, and they are not
        // interchangeable — sharing a file would have them overwrite each
        // other's blob.
        let sharpen = pipeline_cache_file_path(blend_spv(1), c"sharpen", None);
        assert_ne!(
            p1, sharpen,
            "two entry points over one module must not share a cache file"
        );
        assert!(p1.starts_with(&dir));
        assert!(
            p1.file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.ends_with(".bin"))
                .unwrap_or(false),
            "cache file should end in .bin, got {p1:?}"
        );
    });
}

#[test]
#[serial(streamlib_pipeline_cache_env)]
fn env_override_takes_precedence_over_xdg_default() {
    let dir = unique_cache_dir("env-override");
    with_pipeline_cache_dir(&dir, || {
        let resolved = pipeline_cache_dir(None);
        assert_eq!(resolved, dir);
    });
}

#[test]
#[serial(streamlib_pipeline_cache_env)]
fn a_streams_kernel_caches_under_its_stream_unless_the_env_override_is_set() {
    let stream_directory = unique_cache_dir("stream-directory");
    let prev = std::env::var(PIPELINE_CACHE_DIR_ENV).ok();
    // SAFETY: serialized via `#[serial(streamlib_pipeline_cache_env)]`.
    unsafe { std::env::remove_var(PIPELINE_CACHE_DIR_ENV) };
    let resolved_for_the_stream = pipeline_cache_dir(Some(&stream_directory));
    let resolved_for_no_stream = pipeline_cache_dir(None);
    // SAFETY: as above.
    unsafe {
        if let Some(v) = &prev {
            std::env::set_var(PIPELINE_CACHE_DIR_ENV, v);
        }
    }
    assert_eq!(resolved_for_the_stream, stream_directory);
    assert_ne!(
        resolved_for_no_stream, stream_directory,
        "a kernel built for no stream cached under a stream's directory"
    );

    let override_directory = unique_cache_dir("override-over-stream");
    with_pipeline_cache_dir(&override_directory, || {
        assert_eq!(
            pipeline_cache_dir(Some(&stream_directory)),
            override_directory,
            "the env override must win over the stream's directory"
        );
        let in_the_stream =
            pipeline_cache_file_path(blend_spv(1), c"main", Some(&stream_directory));
        assert!(in_the_stream.starts_with(&override_directory));
    });
}

#[test]
#[serial(streamlib_pipeline_cache_env)]
fn empty_env_var_falls_through_to_xdg_default() {
    // Whatever the XDG default resolves to is platform-dependent — what we
    // assert is that an explicitly-empty override does NOT short-circuit
    // resolution to an empty path.
    let dir = unique_cache_dir("empty-env");
    let prev = std::env::var(PIPELINE_CACHE_DIR_ENV).ok();
    // SAFETY: serialized via `#[serial(streamlib_pipeline_cache_env)]`
    // above, so concurrent env-var reads/writes from sibling tests
    // are not possible.
    unsafe { std::env::set_var(PIPELINE_CACHE_DIR_ENV, "") };
    let resolved = pipeline_cache_dir(None);
    unsafe {
        match prev {
            Some(v) => std::env::set_var(PIPELINE_CACHE_DIR_ENV, v),
            None => std::env::remove_var(PIPELINE_CACHE_DIR_ENV),
        }
    }
    assert!(!resolved.as_os_str().is_empty());
    let _ = dir; // keep the helper's tempdir name in scope
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial(streamlib_pipeline_cache_env)]
fn cache_miss_writes_cache_file_after_kernel_construction() {
    let device = vulkan_device_for_dispatch_tests();
    let dir = unique_cache_dir("miss-writes");
    with_pipeline_cache_dir(&dir, || {
        assert!(
            !dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none(),
            "tempdir should be empty before kernel construction"
        );
        let bindings = blend_descriptor(1);
        let _kernel = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                entry_point: "main",
                label: "cache-miss",
                spv: blend_spv(1),
                bindings: &bindings,
                push_constant_size: 4,
            },
        )
        .expect("kernel creation");
        let expected = pipeline_cache_file_path(blend_spv(1), c"main", None);
        assert!(
            expected.exists(),
            "cache file {} should exist after construction",
            expected.display()
        );
        let written = std::fs::read(&expected).expect("read cache");
        assert!(!written.is_empty(), "cache file must not be empty");
        // Header sanity: VkPipelineCacheHeaderVersionOne is 32 bytes,
        // first u32 is header_size = 32, second u32 is header_version = 1.
        let len = u32::from_le_bytes([written[0], written[1], written[2], written[3]]);
        let ver = u32::from_le_bytes([written[4], written[5], written[6], written[7]]);
        assert!(
            len >= 16 && ver == 1,
            "expected pipeline cache header (len, ver=1), got len={len} ver={ver}"
        );
    });
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial(streamlib_pipeline_cache_env)]
fn cache_hit_does_not_panic_or_break_kernel_construction() {
    let device = vulkan_device_for_dispatch_tests();
    let dir = unique_cache_dir("hit-reuses");
    with_pipeline_cache_dir(&dir, || {
        let bindings = blend_descriptor(1);
        // First construction populates the cache.
        drop(
            VulkanComputeKernel::new(
                &device,
                &ComputeKernelDescriptor {
                    entry_point: "main",
                    label: "cache-hit/first",
                    spv: blend_spv(1),
                    bindings: &bindings,
                    push_constant_size: 4,
                },
            )
            .expect("first construction"),
        );
        let cache_path = pipeline_cache_file_path(blend_spv(1), c"main", None);
        let warm_blob = std::fs::read(&cache_path).expect("warm read");
        assert!(
            !warm_blob.is_empty(),
            "cache must be populated by first run"
        );

        // Second construction should hit the cache. We can't assert on
        // tracing output deterministically, but we can assert that the
        // file still exists and is non-empty — the warm path read it,
        // built the pipeline, and rewrote (atomic) on success.
        drop(
            VulkanComputeKernel::new(
                &device,
                &ComputeKernelDescriptor {
                    entry_point: "main",
                    label: "cache-hit/second",
                    spv: blend_spv(1),
                    bindings: &bindings,
                    push_constant_size: 4,
                },
            )
            .expect("second construction"),
        );
        let after = std::fs::read(&cache_path).expect("post-warm read");
        assert!(!after.is_empty());
    });
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial(streamlib_pipeline_cache_env)]
fn corrupt_cache_blob_falls_back_to_recompile_and_overwrites() {
    let device = vulkan_device_for_dispatch_tests();
    let dir = unique_cache_dir("corrupt-blob");
    with_pipeline_cache_dir(&dir, || {
        create_directory_and_its_missing_parents_at_mode(&dir, OWNER_ONLY_DIRECTORY_MODE)
            .expect("mkdir");
        let cache_path = pipeline_cache_file_path(blend_spv(1), c"main", None);
        // Plant a header-invalid blob that the driver will reject.
        // 32 bytes of zeros has header_version=0 ≠ 1 — driver ignores
        // the data and treats the cache as empty.
        std::fs::write(&cache_path, vec![0u8; 32]).expect("plant corrupt blob");

        let bindings = blend_descriptor(1);
        let kernel = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                entry_point: "main",
                label: "corrupt-blob",
                spv: blend_spv(1),
                bindings: &bindings,
                push_constant_size: 4,
            },
        )
        .expect("kernel must construct despite corrupt cache");
        drop(kernel);

        let after = std::fs::read(&cache_path).expect("post-recompile read");
        // Driver-rewritten blob has header_version=1 in bytes 4..8.
        let ver = u32::from_le_bytes([after[4], after[5], after[6], after[7]]);
        assert_eq!(
            ver, 1,
            "expected driver to rewrite cache with valid header, got version={ver}"
        );
    });
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial(streamlib_pipeline_cache_env)]
fn read_only_cache_dir_does_not_break_kernel_construction() {
    let device = vulkan_device_for_dispatch_tests();
    let dir = unique_cache_dir("readonly-dir");
    with_pipeline_cache_dir(&dir, || {
        create_directory_and_its_missing_parents_at_mode(&dir, OWNER_ONLY_DIRECTORY_MODE)
            .expect("mkdir");
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // r-x for owner only — write attempts fail with EACCES.
            perms.set_mode(0o555);
        }
        std::fs::set_permissions(&dir, perms).expect("set readonly");

        let bindings = blend_descriptor(1);
        let result = VulkanComputeKernel::new(
            &device,
            &ComputeKernelDescriptor {
                entry_point: "main",
                label: "readonly-cache",
                spv: blend_spv(1),
                bindings: &bindings,
                push_constant_size: 4,
            },
        );

        // Restore so the cleanup can rmdir.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&dir).unwrap().permissions();
            p.set_mode(0o755);
            let _ = std::fs::set_permissions(&dir, p);
        }

        assert!(
            result.is_ok(),
            "kernel construction must succeed even when cache_dir is read-only: {result:?}"
        );
    });
}
