// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Where the compute and graphics kernels keep their on-disk pipeline caches,
//! and how a cache file is read back, created as a `VkPipelineCache` and
//! written out again.

use std::path::{Path, PathBuf};

use streamlib_runtime_client_contract::directory_at_an_explicit_mode::{
    OWNER_ONLY_DIRECTORY_MODE, create_directory_and_its_missing_parents_at_mode,
};
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;

/// Env var that overrides the pipeline-cache directory of every kernel kind.
/// Used by tests and headless / CI scenarios that need a writable, isolated
/// cache root.
pub(crate) const PIPELINE_CACHE_DIR_ENV: &str = "STREAMLIB_PIPELINE_CACHE_DIR";

/// The kind of kernel a pipeline cache belongs to: what its log lines name it
/// and the extension its cache files carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PipelineCacheKernelKind {
    Compute,
    Graphics,
}

impl PipelineCacheKernelKind {
    /// What a log line about this kind's pipeline cache calls the kernel.
    fn log_label(self) -> &'static str {
        match self {
            Self::Compute => "Compute kernel",
            Self::Graphics => "Graphics kernel",
        }
    }

    /// The extension of this kind's cache files; the two kinds never share a
    /// file even under one directory.
    fn cache_file_extension(self) -> &'static str {
        match self {
            Self::Compute => "bin",
            Self::Graphics => "gfx.bin",
        }
    }
}

/// Resolve the cache directory.
///
/// Order: `STREAMLIB_PIPELINE_CACHE_DIR` env override → the building stream's
/// own directory under its project → the streamlib data directory's
/// `cache/pipeline-cache` for a kernel the engine builds for no stream.
pub(crate) fn pipeline_cache_dir(pipeline_cache_directory_of_its_stream: Option<&Path>) -> PathBuf {
    if let Ok(dir) = std::env::var(PIPELINE_CACHE_DIR_ENV)
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    if let Some(stream_directory) = pipeline_cache_directory_of_its_stream {
        return stream_directory.to_path_buf();
    }
    // Co-located under the streamlib home (`<STREAMLIB_HOME>/.streamlib/cache/`),
    // NOT the XDG cache dir — every built/cached artifact lives under the
    // streamlib working tree per the home contract. See
    // `streamlib_runtime_client_contract::streamlib_home`.
    streamlib_runtime_client_contract::streamlib_home::get_streamlib_data_dir()
        .join("cache")
        .join("pipeline-cache")
}

/// The cache file a `kernel_kind` pipeline keyed by `cache_key_hex` reads and
/// writes, in the directory [`pipeline_cache_dir`] resolves.
pub(crate) fn pipeline_cache_file_path_keyed_by(
    kernel_kind: PipelineCacheKernelKind,
    cache_key_hex: &str,
    pipeline_cache_directory_of_its_stream: Option<&Path>,
) -> PathBuf {
    pipeline_cache_dir(pipeline_cache_directory_of_its_stream).join(format!(
        "{cache_key_hex}.{}",
        kernel_kind.cache_file_extension()
    ))
}

/// The cache blob at `path`, `None` when it is missing, empty or unreadable.
pub(crate) fn read_cache_blob(
    kernel_kind: PipelineCacheKernelKind,
    path: &Path,
) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) if !bytes.is_empty() => Some(bytes),
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                "{} pipeline cache: unreadable cache file at {}: {e}",
                kernel_kind.log_label(),
                path.display()
            );
            None
        }
    }
}

/// A `VkPipelineCache` seeded with `initial_data`, `None` when the driver
/// refuses one and the pipeline builds against the null cache.
pub(crate) fn create_pipeline_cache_handle(
    kernel_kind: PipelineCacheKernelKind,
    device: &vulkanalia::Device,
    initial_data: Option<&[u8]>,
    label: &str,
) -> Option<vk::PipelineCache> {
    let kernel = kernel_kind.log_label();
    let mut info = vk::PipelineCacheCreateInfo::builder();
    if let Some(data) = initial_data {
        info = info.initial_data(data);
        tracing::debug!(
            "{kernel} '{label}': loading pipeline cache (pInitialData {} bytes)",
            data.len()
        );
    } else {
        tracing::debug!("{kernel} '{label}': pipeline cache cold (no pInitialData)");
    }
    let info = info.build();
    match unsafe { device.create_pipeline_cache(&info, None) } {
        Ok(handle) => Some(handle),
        Err(e) => {
            tracing::warn!(
                "{kernel} '{label}': vkCreatePipelineCache failed: {e} — falling back to null cache"
            );
            None
        }
    }
}

/// Write what the driver holds in `cache` to `path`; a failure is logged and
/// costs the next build a cold compile, nothing more.
pub(crate) fn persist_pipeline_cache(
    kernel_kind: PipelineCacheKernelKind,
    device: &vulkanalia::Device,
    cache: vk::PipelineCache,
    path: &Path,
    label: &str,
) {
    let kernel = kernel_kind.log_label();
    let data = match unsafe { device.get_pipeline_cache_data(cache) } {
        Ok(data) => data,
        Err(e) => {
            tracing::warn!("{kernel} '{label}': vkGetPipelineCacheData failed: {e}");
            return;
        }
    };
    if data.is_empty() {
        return;
    }
    if let Err(e) = atomic_write_pipeline_cache(kernel_kind, path, &data) {
        tracing::warn!(
            "{kernel} '{label}': failed to persist pipeline cache to {}: {e}",
            path.display()
        );
    } else {
        tracing::debug!(
            "{kernel} '{label}': persisted pipeline cache ({} bytes) to {}",
            data.len(),
            path.display()
        );
    }
}

fn atomic_write_pipeline_cache(
    kernel_kind: PipelineCacheKernelKind,
    path: &Path,
    data: &[u8],
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        create_directory_and_its_missing_parents_at_mode(parent, OWNER_ONLY_DIRECTORY_MODE)?;
    }
    // Same-directory temp file → POSIX rename is atomic on the same
    // filesystem. The loser of a race just overwrites the winner, which is
    // fine — both blobs are equally valid and the driver re-validates on
    // next load.
    let mut tmp = path.to_path_buf();
    tmp.set_extension(format!(
        "{}.tmp.{}",
        kernel_kind.cache_file_extension(),
        mint_machine_global_unique_name_suffix()
    ));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
