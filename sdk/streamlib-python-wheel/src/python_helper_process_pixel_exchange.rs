// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The helper child's half of the pixel exchange.
//!
//! A Python processor's pixels live in the engine's pools, one process away.
//! Acquiring goes through two channels that already exist: the escalate
//! socket carries the request (the parent allocates, checks the buffer into
//! its surface-share service, and answers with the minted surface id), and
//! the surface-share socket carries the memory itself — the checkout's
//! `recvmsg` lands the DMA-BUF plane fds in this process's fd table via
//! SCM_RIGHTS, and the consumer-side Vulkan import maps them. The escalate
//! socket never carries fds.
//!
//! Pool allocations are DMA-BUF-flavoured; an `opaque_fd` checkout is
//! refused here by name because it belongs to the device-export staging
//! path, which imports through CUDA rather than a host mapping.

use std::path::PathBuf;

use pyo3::prelude::*;
use pyo3::types::PyDict;

#[cfg(target_os = "linux")]
use pyo3::exceptions::PyRuntimeError;
#[cfg(target_os = "linux")]
use std::os::fd::RawFd;
#[cfg(target_os = "linux")]
use std::os::unix::net::UnixStream;
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use parking_lot::Mutex;
#[cfg(target_os = "linux")]
use streamlib_consumer_rhi::{ConsumerVulkanBuffer, ConsumerVulkanDevice};

use streamlib::sdk::rhi::PixelFormat;

/// One escalate round trip to the parent, called with the GIL attached.
///
/// The callable is the bridge's `request_from_parent`, whose wait on the
/// response releases the GIL — a slow parent parks this thread, never the
/// interpreter's others.
fn escalate_round_trip<'py>(
    python: Python<'py>,
    escalate_request_to_parent: &Py<PyAny>,
    op: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    escalate_request_to_parent
        .bind(python)
        .call1((op,))
        .map_err(|request_failure| {
            crate::python_processor_context::gpu_operation_error(format!(
                "the parent refused or failed the GPU request: {request_failure}"
            ))
        })
}

/// What a checkout turned into once the fds were imported: mapped memory
/// plus the layout facts every view derives from.
#[cfg(target_os = "linux")]
pub(crate) struct HelperCheckedOutPixelSurface {
    /// The id this surface travels under — what a downstream processor
    /// resolves, and what keys the parent's registry entry.
    pub(crate) surface_id: String,
    pub(crate) consumer_buffer: ConsumerVulkanBuffer,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: PixelFormat,
    pub(crate) bytes_per_row: u64,
    /// Present only on an acquired surface — a resolved one belongs to its
    /// acquirer, and releasing it here would evict somebody else's frame.
    pub(crate) release_to_parent: Option<HelperSurfaceReleaseDebt>,
}

/// The release an acquired surface owes its parent: one `release_handle`
/// escalate op, which drops the parent registry's strong reference and the
/// surface-share service entry together.
#[cfg(target_os = "linux")]
pub(crate) struct HelperSurfaceReleaseDebt {
    escalate_request_to_parent: Py<PyAny>,
    handle_id: String,
}

#[cfg(target_os = "linux")]
impl Drop for HelperSurfaceReleaseDebt {
    /// Best-effort: a parent that is already gone has released everything
    /// with the connection, so a failure here is logged, never raised.
    fn drop(&mut self) {
        let outcome = Python::attach(|python| -> PyResult<()> {
            let op = PyDict::new(python);
            op.set_item("op", "release_handle")?;
            op.set_item("handle_id", self.handle_id.as_str())?;
            escalate_round_trip(python, &self.escalate_request_to_parent, &op)?;
            Ok(())
        });
        if let Err(release_failure) = outcome {
            tracing::warn!(
                handle_id = %self.handle_id,
                %release_failure,
                "releasing a surface to the parent failed; its pool slot returns at teardown"
            );
        }
    }
}

/// The child-side client that fulfills `ctx.gpu_limited_access` calls by
/// crossing to the parent: escalate for allocation, surface-share for the
/// memory, one consumer Vulkan device per child for the import.
pub(crate) struct HelperProcessGpuExchangeClient {
    escalate_request_to_parent: Py<PyAny>,
    #[cfg_attr(not(target_os = "linux"), expect(dead_code))]
    surface_socket_path: PathBuf,
    /// One connection per child, opened at first checkout. Cleared on any
    /// IO failure so the next call reconnects instead of reusing a stream
    /// with half a frame in it.
    #[cfg(target_os = "linux")]
    surface_share_connection: Mutex<Option<UnixStream>>,
    /// One Vulkan device per child, created at first import.
    #[cfg(target_os = "linux")]
    consumer_vulkan_device: Mutex<Option<Arc<ConsumerVulkanDevice>>>,
}

impl HelperProcessGpuExchangeClient {
    pub(crate) fn new(escalate_request_to_parent: Py<PyAny>, surface_socket_path: PathBuf) -> Self {
        Self {
            escalate_request_to_parent,
            surface_socket_path,
            #[cfg(target_os = "linux")]
            surface_share_connection: Mutex::new(None),
            #[cfg(target_os = "linux")]
            consumer_vulkan_device: Mutex::new(None),
        }
    }

    /// Ask the parent to allocate, then check the result out and import it.
    ///
    /// Called attached; the escalate wait releases the GIL, and the checkout
    /// and Vulkan import run detached.
    #[cfg(target_os = "linux")]
    pub(crate) fn acquire_pixel_buffer(
        &self,
        python: Python<'_>,
        width: u32,
        height: u32,
        wire_format_name: &str,
    ) -> PyResult<HelperCheckedOutPixelSurface> {
        let op = PyDict::new(python);
        op.set_item("op", "acquire_pixel_buffer")?;
        op.set_item("width", width)?;
        op.set_item("height", height)?;
        op.set_item("format", wire_format_name)?;
        let response = escalate_round_trip(python, &self.escalate_request_to_parent, &op)?;
        let handle_id: String = response
            .get_item("handle_id")
            .map_err(|_| {
                PyRuntimeError::new_err(
                    "the parent's acquire_pixel_buffer response carried no handle_id",
                )
            })?
            .extract()?;
        let mut checked_out = python.detach(|| self.check_out_and_import(&handle_id))?;
        checked_out.release_to_parent = Some(HelperSurfaceReleaseDebt {
            escalate_request_to_parent: self.escalate_request_to_parent.clone_ref(python),
            handle_id,
        });
        Ok(checked_out)
    }

    /// Check out a surface another processor published. No release debt:
    /// the surface belongs to its acquirer.
    #[cfg(target_os = "linux")]
    pub(crate) fn resolve_surface(
        &self,
        python: Python<'_>,
        surface_id: &str,
    ) -> PyResult<HelperCheckedOutPixelSurface> {
        python.detach(|| self.check_out_and_import(surface_id))
    }

    /// `check_out` over the surface-share socket, then the DMA-BUF import.
    #[cfg(target_os = "linux")]
    fn check_out_and_import(&self, surface_id: &str) -> PyResult<HelperCheckedOutPixelSurface> {
        let request = serde_json::json!({"op": "check_out", "surface_id": surface_id});
        let (response, received_fds) = self.surface_share_request(&request)?;
        self.import_checked_out_surface(surface_id, &response, received_fds)
    }

    /// One request/response over the cached surface-share connection,
    /// reconnecting lazily and dropping the connection on any IO failure.
    #[cfg(target_os = "linux")]
    fn surface_share_request(
        &self,
        request: &serde_json::Value,
    ) -> PyResult<(serde_json::Value, Vec<RawFd>)> {
        let mut connection = self.surface_share_connection.lock();
        if connection.is_none() {
            *connection = Some(
                streamlib_surface_client::connect_to_surface_share_socket(
                    &self.surface_socket_path,
                )
                .map_err(|connect_failure| {
                    PyRuntimeError::new_err(format!(
                        "could not reach the surface-share socket at {}: {connect_failure}. The \
                         parent runtime owns that socket; if it is gone, this helper is orphaned",
                        self.surface_socket_path.display(),
                    ))
                })?,
            );
        }
        let stream = connection.as_ref().expect("connection just populated");
        match streamlib_surface_client::send_request_with_fds(
            stream,
            request,
            &[],
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        ) {
            Ok(response_and_fds) => Ok(response_and_fds),
            Err(io_failure) => {
                *connection = None;
                Err(PyRuntimeError::new_err(format!(
                    "the surface-share request failed mid-stream: {io_failure}"
                )))
            }
        }
    }

    /// Validate the checkout metadata and turn the plane fds into mapped
    /// memory. Owns the fds from here: every early return closes them.
    #[cfg(target_os = "linux")]
    fn import_checked_out_surface(
        &self,
        surface_id: &str,
        response: &serde_json::Value,
        received_fds: Vec<RawFd>,
    ) -> PyResult<HelperCheckedOutPixelSurface> {
        let close_all = |fds: &[RawFd]| {
            for fd in fds {
                // SAFETY: kernel-delivered fds this process owns and has not
                // imported anywhere.
                unsafe { libc::close(*fd) };
            }
        };
        if let Some(checkout_error) = response.get("error").and_then(|value| value.as_str()) {
            close_all(&received_fds);
            return Err(PyRuntimeError::new_err(format!(
                "the surface-share service refused check_out of {surface_id:?}: {checkout_error}"
            )));
        }

        // Trailing timeline-semaphore fds arrive after the plane fds when the
        // registration carried them. A pixel-buffer checkout carries none
        // today; peeled and closed rather than assumed absent, so a
        // registration that gains them cannot corrupt the plane list.
        let trailing_timeline_fd_count = ["has_produce_done_fd", "has_consume_done_fd"]
            .iter()
            .filter(|flag| {
                response
                    .get(**flag)
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false)
            })
            .count();
        if received_fds.len() < trailing_timeline_fd_count + 1 {
            close_all(&received_fds);
            return Err(PyRuntimeError::new_err(format!(
                "check_out of {surface_id:?} returned {} fds, fewer than the {} its metadata \
                 promises",
                received_fds.len(),
                trailing_timeline_fd_count + 1,
            )));
        }
        let mut plane_fds = received_fds;
        let timeline_fds = plane_fds.split_off(plane_fds.len() - trailing_timeline_fd_count);
        close_all(&timeline_fds);

        let handle_type = response
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("dma_buf");
        if handle_type != "dma_buf" {
            close_all(&plane_fds);
            return Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} is registered as {handle_type:?}, which is not a \
                 host-mappable pixel buffer: an opaque_fd surface belongs to the device-export \
                 path and imports through CUDA, not a CPU mapping"
            )));
        }

        let metadata_u32 = |field: &str| -> PyResult<u32> {
            response
                .get(field)
                .and_then(|value| value.as_u64())
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "check_out of {surface_id:?} carried no usable {field}"
                    ))
                })
        };
        let width = metadata_u32("width").inspect_err(|_| close_all(&plane_fds))?;
        let height = metadata_u32("height").inspect_err(|_| close_all(&plane_fds))?;
        let format_name = response
            .get("format")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let format = crate::python_processor_context::parse_pixel_format_name(format_name)
            .inspect_err(|_| close_all(&plane_fds))?;
        let plane_sizes: Vec<u64> = response
            .get("plane_sizes")
            .and_then(|value| value.as_array())
            .map(|sizes| sizes.iter().filter_map(|size| size.as_u64()).collect())
            .unwrap_or_default();
        if plane_sizes.len() != plane_fds.len() {
            close_all(&plane_fds);
            return Err(PyRuntimeError::new_err(format!(
                "check_out of {surface_id:?} returned {} plane fds but {} plane sizes",
                plane_fds.len(),
                plane_sizes.len(),
            )));
        }
        // The allocation's row pitch, padding included — the same derivation
        // the engine-side view uses, so the strides agree across processes.
        let plane0_size = plane_sizes[0];
        if plane0_size == 0 || !plane0_size.is_multiple_of(u64::from(height)) {
            close_all(&plane_fds);
            return Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} reports plane size {plane0_size}, not a whole number of \
                 {height} rows"
            )));
        }
        let bytes_per_row = plane0_size / u64::from(height);

        let vulkan_device = self
            .consumer_vulkan_device()
            .inspect_err(|_| close_all(&plane_fds))?;
        // Import from dups: vkAllocateMemory takes ownership of a fd only on
        // success, so handing over the originals would leave their ownership
        // ambiguous on a partial multi-plane failure. The originals stay ours
        // and close exactly once; a dup not consumed by a failed import is
        // the leak we accept on that error path.
        let mut dup_fds: Vec<RawFd> = Vec::with_capacity(plane_fds.len());
        for fd in &plane_fds {
            // SAFETY: duplicating an fd this process owns.
            let dup_fd = unsafe { libc::dup(*fd) };
            if dup_fd < 0 {
                close_all(&dup_fds);
                close_all(&plane_fds);
                return Err(PyRuntimeError::new_err(format!(
                    "could not duplicate a plane fd for import: {}",
                    std::io::Error::last_os_error()
                )));
            }
            dup_fds.push(dup_fd);
        }
        let import_outcome =
            ConsumerVulkanBuffer::from_dma_buf_fds(&vulkan_device, &dup_fds, &plane_sizes);
        close_all(&plane_fds);
        let consumer_buffer = import_outcome.map_err(|import_failure| {
            PyRuntimeError::new_err(format!(
                "Vulkan could not import surface {surface_id:?}'s DMA-BUF planes: \
                 {import_failure}"
            ))
        })?;

        Ok(HelperCheckedOutPixelSurface {
            surface_id: surface_id.to_string(),
            consumer_buffer,
            width,
            height,
            format,
            bytes_per_row,
            release_to_parent: None,
        })
    }

    #[cfg(target_os = "linux")]
    fn consumer_vulkan_device(&self) -> PyResult<Arc<ConsumerVulkanDevice>> {
        let mut device = self.consumer_vulkan_device.lock();
        if let Some(existing_device) = device.as_ref() {
            return Ok(Arc::clone(existing_device));
        }
        let created = Arc::new(ConsumerVulkanDevice::new().map_err(|device_failure| {
            PyRuntimeError::new_err(format!(
                "this helper process could not create its Vulkan import device: {device_failure}"
            ))
        })?);
        *device = Some(Arc::clone(&created));
        Ok(created)
    }
}
