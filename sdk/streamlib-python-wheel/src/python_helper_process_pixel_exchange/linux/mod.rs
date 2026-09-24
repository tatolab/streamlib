// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::os::fd::{FromRawFd as _, IntoRawFd as _, OwnedFd, RawFd};
use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib_consumer_rhi::ConsumerVulkanBuffer;

use super::{
    HelperCheckedOutPixelSurface, HelperCheckedOutSurface, HelperProcessGpuExchangeClient,
    HelperSurfaceCheckOutLeaseDebt, SURFACE_SHARE_RESPONSE_TIMEOUT,
    refuse_check_out_the_service_declined, required_positive_u32_check_out_metadata_field,
};
use texture::an_acquired_device_texture_carries_no_exportable_fd_error;

mod export_staging;
mod foreign_dma_buf;
mod gpu_kernels;
mod processor_owned_window;
mod texture;

pub(crate) use export_staging::{
    CpuReadbackCopyDirection, HelperCpuReadbackExport, HelperDeviceExport,
};
pub(crate) use foreign_dma_buf::HelperForeignSurfaceUnregisterDebt;
pub(crate) use gpu_kernels::{
    HelperProcessGraphicsDraw, HelperProcessGraphicsKernelRegistration,
    HelperProcessRayTracingKernelRegistration, compute_dispatch_wire_entry,
};
pub(crate) use texture::{
    HelperAcquiredTexture, HelperCheckedOutTextureSurface, OpaqueFdTextureExportDescription,
};

/// One field of an escalate response, named in the failure so a parent
/// that answered a shape this child does not understand says which part.
fn response_field<'py>(response: &Bound<'py, PyAny>, field: &str) -> PyResult<Bound<'py, PyAny>> {
    response.get_item(field).map_err(|_| {
        crate::python_processor_context::gpu_operation_error(format!(
            "the parent's response carried no {field}"
        ))
    })
}

/// A u64 the wire carries as a decimal string, because JSON has no 64-bit
/// integer. Host-side counterpart: `EscalateResponseOk::staging_byte_size`.
fn decimal_string_field(response: &Bound<'_, PyAny>, field: &str) -> PyResult<u64> {
    let as_written: String = response_field(response, field)?.extract()?;
    as_written.parse().map_err(|_| {
        crate::python_processor_context::gpu_operation_error(format!(
            "the parent's {field} was {as_written:?}, which is not a decimal u64"
        ))
    })
}

/// The exporting device's UUID, as 32 hex characters.
fn parse_device_uuid(as_hex: &str) -> PyResult<[u8; 16]> {
    let mut uuid = [0u8; 16];
    if as_hex.len() != 32 {
        return Err(crate::python_processor_context::gpu_operation_error(
            format!(
                "the parent reported the exporting device UUID as {as_hex:?}, which is not 32 hex \
             characters; importing onto the wrong GPU reads the wrong memory rather than failing"
            ),
        ));
    }
    for (byte, hex_pair) in uuid.iter_mut().zip(as_hex.as_bytes().chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(hex_pair).unwrap_or("zz"), 16).map_err(
            |_| {
                crate::python_processor_context::gpu_operation_error(format!(
                    "the parent reported the exporting device UUID as {as_hex:?}, which is not hex"
                ))
            },
        )?;
    }
    Ok(uuid)
}

/// A `u64` array field of a checkout's registration metadata; absent or
/// malformed entries collapse to an empty array the caller length-checks.
fn plane_u64_array_check_out_metadata_field(response: &serde_json::Value, field: &str) -> Vec<u64> {
    response
        .get(field)
        .and_then(|value| value.as_array())
        .map(|entries| entries.iter().filter_map(|entry| entry.as_u64()).collect())
        .unwrap_or_default()
}

/// Dup every plane fd for a Vulkan import, leaving the originals with the
/// surface so later exports answer locally. vkAllocateMemory takes fd
/// ownership only on success, so importing the originals would leave their
/// ownership ambiguous on a partial multi-plane failure.
///
/// Returned raw: ownership hands to the very next Vulkan import call. The
/// dups are collected as `OwnedFd` first so a partial duplication failure
/// closes the successes by scope.
fn duplicate_plane_fds_for_import(plane_fds: &[OwnedFd]) -> PyResult<Vec<RawFd>> {
    let duplicated: Vec<OwnedFd> = plane_fds
        .iter()
        .map(|plane_fd| {
            plane_fd.try_clone().map_err(|duplicate_failure| {
                PyRuntimeError::new_err(format!(
                    "could not duplicate a plane fd for import: {duplicate_failure}"
                ))
            })
        })
        .collect::<PyResult<_>>()?;
    Ok(duplicated.into_iter().map(OwnedFd::into_raw_fd).collect())
}

/// Close a dup Vulkan refused ownership of — and this path can run per
/// frame. Single-plane only: a multi-plane failure leaves the tail's
/// ownership ambiguous (already-imported planes were freed by the callee's
/// teardown), and those dups leak, bounded by plane count.
fn close_single_plane_dup_vulkan_refused(dup_raw_fds: &[RawFd]) {
    if let [only_plane_fd] = dup_raw_fds {
        // SAFETY: an fd Vulkan refused ownership of; ours alone.
        unsafe { libc::close(*only_plane_fd) };
    }
}

/// Dup the first plane fd so the caller can hand it to native code without
/// disturbing the surface's own mapping.
fn duplicate_first_plane_fd_for_export(
    exported_plane_fds: &[OwnedFd],
    exported_subject: &str,
) -> PyResult<OwnedFd> {
    let first_plane_fd = exported_plane_fds.first().ok_or_else(|| {
        PyRuntimeError::new_err(format!(
            "this {exported_subject} was checked out with no plane fd to export; nothing to \
             hand to native code"
        ))
    })?;
    first_plane_fd.try_clone().map_err(|duplicate_failure| {
        PyRuntimeError::new_err(format!(
            "could not duplicate this {exported_subject}'s memory fd: {duplicate_failure}"
        ))
    })
}

impl HelperCheckedOutPixelSurface {
    /// A DMA-BUF fd for the first plane, and the plane's byte size.
    ///
    /// The fd is a `dup` of the one this process was handed at check-out,
    /// so the caller owns it and closing it does not disturb this
    /// surface's own mapping. Always a genuine DMA-BUF: the pixel
    /// checkout refuses every other flavour before this surface can
    /// exist, so this name never mislabels an fd.
    pub(crate) fn export_dma_buf(&self) -> PyResult<(RawFd, u64)> {
        let exported = duplicate_first_plane_fd_for_export(&self.exported_plane_fds, "surface")?;
        Ok((
            exported.into_raw_fd(),
            self.bytes_per_row * u64::from(self.height),
        ))
    }
}

impl HelperCheckedOutSurface {
    pub(crate) fn exchange_client(&self) -> &Arc<HelperProcessGpuExchangeClient> {
        match self {
            Self::PixelBuffer(pixel_surface) => &pixel_surface.exchange_client,
            Self::Texture(texture_surface) => &texture_surface.exchange_client,
            Self::AcquiredDeviceTexture(acquired_texture) => &acquired_texture.exchange_client,
        }
    }

    /// A DMA-BUF fd for the surface's first plane plus its byte size,
    /// whichever backing answers.
    pub(crate) fn export_dma_buf(&self) -> PyResult<(RawFd, u64)> {
        match self {
            Self::PixelBuffer(pixel_surface) => pixel_surface.export_dma_buf(),
            Self::Texture(texture_surface) => texture_surface.export_dma_buf(),
            Self::AcquiredDeviceTexture(_) => {
                Err(an_acquired_device_texture_carries_no_exportable_fd_error())
            }
        }
    }

    /// The OPAQUE_FD texture handle plus its allocation-stable shape,
    /// or the refusal naming the right door.
    pub(crate) fn export_opaque_fd(&self) -> PyResult<OpaqueFdTextureExportDescription> {
        match self {
            Self::PixelBuffer(_) => Err(PyRuntimeError::new_err(
                "this surface is a pixel buffer, not a texture; its memory fd exports \
                 through `export_dma_buf`",
            )),
            Self::Texture(texture_surface) => texture_surface.export_opaque_fd(),
            Self::AcquiredDeviceTexture(_) => {
                Err(an_acquired_device_texture_carries_no_exportable_fd_error())
            }
        }
    }
}

/// How many connections a helper sets aside after a timeout before it stops
/// asking the service anything: each one stays open, and a service that has
/// outwaited the timeout this often has stopped answering.
const SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST: usize = 4;

impl HelperProcessGpuExchangeClient {
    /// One request/response over the cached surface-share connection,
    /// reconnecting lazily. The connection is taken out of the slot for the
    /// exchange and put back only on success, so a stream with half a frame
    /// in it is structurally dropped rather than remembered to be.
    pub(super) fn surface_share_request(
        &self,
        request: &serde_json::Value,
    ) -> PyResult<(serde_json::Value, Vec<OwnedFd>)> {
        self.surface_share_request_with_fds(request, &[])
    }

    /// The same exchange with outbound fds riding the request via
    /// SCM_RIGHTS — the adoption path's crossing. The kernel dups each fd
    /// into the service's table; the caller keeps its own.
    fn surface_share_request_with_fds(
        &self,
        request: &serde_json::Value,
        outbound_fds: &[RawFd],
    ) -> PyResult<(serde_json::Value, Vec<OwnedFd>)> {
        let mut connection = self.surface_share_connection.lock();
        let stream = match connection.take() {
            Some(open_stream) => open_stream,
            None => {
                let connections_set_aside = self
                    .surface_share_connections_set_aside_after_a_timeout
                    .lock()
                    .len();
                if connections_set_aside >= SURFACE_SHARE_CONNECTIONS_SET_ASIDE_AT_MOST {
                    return Err(PyRuntimeError::new_err(format!(
                        "the surface-share service has outwaited the {} s response timeout \
                         {connections_set_aside} times, so this helper asks it nothing more; the \
                         connections it set aside stay open, keeping the frames claimed on them \
                         held, until this helper stops",
                        SURFACE_SHARE_RESPONSE_TIMEOUT.as_secs()
                    )));
                }
                let opened_stream = streamlib_surface_client::connect_to_surface_share_socket(
                    &self.surface_socket_path,
                )
                .map_err(|connect_failure| {
                    PyRuntimeError::new_err(format!(
                        "could not reach the surface-share socket at {}: {connect_failure}. The \
                         parent runtime owns that socket; if it is gone, this helper is orphaned",
                        self.surface_socket_path.display(),
                    ))
                })?;
                opened_stream
                    .set_read_timeout(Some(SURFACE_SHARE_RESPONSE_TIMEOUT))
                    .map_err(|timeout_failure| {
                        PyRuntimeError::new_err(format!(
                            "could not bound how long the surface-share socket at {} may take \
                             to answer: {timeout_failure}",
                            self.surface_socket_path.display(),
                        ))
                    })?;
                opened_stream
            }
        };
        let (response, received_raw_fds) = match streamlib_surface_client::send_request_with_fds(
            &stream,
            request,
            outbound_fds,
            streamlib_surface_client::MAX_SCM_RIGHTS_FDS,
        ) {
            Ok(answered) => answered,
            Err(io_failure)
                if matches!(
                    io_failure.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                self.surface_share_connections_set_aside_after_a_timeout
                    .lock()
                    .push(stream);
                return Err(PyRuntimeError::new_err(format!(
                    "the surface-share service did not answer within {} s; this helper set that \
                     connection aside, keeping the frames it claimed on it held until it stops, \
                     and opens a new one on its next request",
                    SURFACE_SHARE_RESPONSE_TIMEOUT.as_secs()
                )));
            }
            Err(io_failure) => {
                return Err(PyRuntimeError::new_err(format!(
                    "the surface-share request failed mid-stream: {io_failure}"
                )));
            }
        };
        *connection = Some(stream);
        // SAFETY: adopting kernel-delivered fds the recvmsg just placed in
        // this process's fd table; nothing else holds them.
        let received_fds = received_raw_fds
            .into_iter()
            .map(|raw_fd| unsafe { OwnedFd::from_raw_fd(raw_fd) })
            .collect();
        Ok((response, received_fds))
    }

    /// Validate the checkout metadata and turn the plane fds into this
    /// process's view of the surface — mapped memory for a pixel buffer, an
    /// imported `VkImage` for a texture. The fds are `OwnedFd`s, so every
    /// early return closes them by scope rather than by remembering to.
    pub(super) fn import_checked_out_surface(
        self: &Arc<Self>,
        surface_id: &str,
        response: &serde_json::Value,
        received_fds: Vec<OwnedFd>,
    ) -> PyResult<HelperCheckedOutSurface> {
        refuse_check_out_the_service_declined(format_args!("{surface_id:?}"), response)?;

        // Trailing timeline-semaphore fds arrive after the plane fds when
        // the registration carried them — texture registrations do; a
        // pixel-buffer checkout carries none today. Peeled rather than
        // assumed absent, so a registration that gains them cannot corrupt
        // the plane list.
        let has_timeline_fd = |flag: &str| {
            response
                .get(flag)
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        };
        let has_produce_done_fd = has_timeline_fd("has_produce_done_fd");
        let has_consume_done_fd = has_timeline_fd("has_consume_done_fd");
        let trailing_timeline_fd_count =
            usize::from(has_produce_done_fd) + usize::from(has_consume_done_fd);
        if received_fds.len() < trailing_timeline_fd_count + 1 {
            return Err(PyRuntimeError::new_err(format!(
                "check_out of {surface_id:?} returned {} fds, fewer than the {} its metadata \
                 promises",
                received_fds.len(),
                trailing_timeline_fd_count + 1,
            )));
        }
        let mut plane_fds = received_fds;
        let mut trailing_timeline_fds = plane_fds
            .split_off(plane_fds.len() - trailing_timeline_fd_count)
            .into_iter();
        let produce_done_fd = has_produce_done_fd
            .then(|| trailing_timeline_fds.next())
            .flatten();
        let consume_done_fd = has_consume_done_fd
            .then(|| trailing_timeline_fds.next())
            .flatten();

        match response
            .get("resource_type")
            .and_then(|value| value.as_str())
            .unwrap_or("pixel_buffer")
        {
            "texture" => {
                return self
                    .import_checked_out_texture(
                        surface_id,
                        response,
                        plane_fds,
                        produce_done_fd,
                        consume_done_fd,
                    )
                    .map(HelperCheckedOutSurface::Texture);
            }
            "pixel_buffer" => {}
            other => {
                return Err(PyRuntimeError::new_err(format!(
                    "surface {surface_id:?} is registered as resource type {other:?}, which \
                     this consumer does not know"
                )));
            }
        }

        let handle_type = response
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("dma_buf");
        if handle_type != "dma_buf" {
            return Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} is registered as {handle_type:?}, which is not a \
                 host-mappable pixel buffer: an opaque_fd surface belongs to the device-export \
                 path and imports through CUDA, not a CPU mapping"
            )));
        }

        let width = required_positive_u32_check_out_metadata_field(response, surface_id, "width")?;
        let height =
            required_positive_u32_check_out_metadata_field(response, surface_id, "height")?;
        let format_name = response
            .get("format")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let format = crate::python_processor_context::parse_pixel_format_name(format_name)?;
        let plane_sizes = plane_u64_array_check_out_metadata_field(response, "plane_sizes");
        if plane_sizes.len() != plane_fds.len() {
            return Err(PyRuntimeError::new_err(format!(
                "check_out of {surface_id:?} returned {} plane fds but {} plane sizes",
                plane_fds.len(),
                plane_sizes.len(),
            )));
        }
        // The allocation's row pitch, padding included — the same derivation
        // the engine-side view uses, so the strides agree across processes.
        let plane0_size = plane_sizes.first().copied().unwrap_or(0);
        if plane0_size == 0 || !plane0_size.is_multiple_of(u64::from(height)) {
            return Err(PyRuntimeError::new_err(format!(
                "surface {surface_id:?} reports plane size {plane0_size}, not a whole number of \
                 {height} rows"
            )));
        }
        let bytes_per_row = plane0_size / u64::from(height);

        let vulkan_device = self.consumer_vulkan_device()?;
        let dup_raw_fds = duplicate_plane_fds_for_import(&plane_fds)?;
        let consumer_buffer =
            ConsumerVulkanBuffer::from_dma_buf_fds(&vulkan_device, &dup_raw_fds, &plane_sizes)
                .map_err(|import_failure| {
                    close_single_plane_dup_vulkan_refused(&dup_raw_fds);
                    PyRuntimeError::new_err(format!(
                        "Vulkan could not import surface {surface_id:?}'s DMA-BUF planes: \
                         {import_failure}"
                    ))
                })?;

        Ok(HelperCheckedOutSurface::PixelBuffer(
            HelperCheckedOutPixelSurface {
                surface_id: surface_id.to_string(),
                consumer_buffer,
                width,
                height,
                format,
                bytes_per_row,
                release_to_parent: None,
                unregister_foreign_from_surface_share: None,
                release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt {
                    exchange_client: Arc::clone(self),
                    surface_id: surface_id.to_string(),
                },
                exported_plane_fds: plane_fds,
                exchange_client: Arc::clone(self),
            },
        ))
    }
}

#[cfg(test)]
mod surface_share_response_timeout_tests;
