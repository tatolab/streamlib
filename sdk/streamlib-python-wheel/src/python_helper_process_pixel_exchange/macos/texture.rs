// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use streamlib_consumer_rhi::{
    ConsumerVulkanBuffer, ConsumerVulkanDevice, ConsumerVulkanTexture,
    ConsumerVulkanTimelineSemaphore, TextureFormat, VulkanImageUsage, VulkanLayout,
};

use super::super::{
    HelperCheckedOutSurface, HelperProcessGpuExchangeClient, HelperSurfaceCheckOutLeaseDebt,
    HelperSurfaceReleaseDebt, SurfaceShareTransferredHandle, escalate_round_trip_to_parent,
    required_positive_u32_check_out_metadata_field,
};
use super::{HelperIOSurfaceCpuLock, IOSurfaceLockRefused};
use streamlib::sdk::engine::apple_surface_share::RetainedIOSurfaceSharedAcrossThreads;

/// A texture-backed surface this helper imported: the engine's image rebuilt
/// over its IOSurface on this process's own device, plus the timeline pair
/// and the layout cell that keep the crossing coordinated.
///
/// The surface's storage is its own linear rows, so the CPU reaches the
/// pixels through the surface itself under its lock — the same memory the
/// engine's image and this side's image both address, with no staging.
pub(crate) struct HelperCheckedOutTextureSurface {
    pub(crate) surface_id: String,
    pub(crate) consumer_texture: ConsumerVulkanTexture,
    /// The surface the image is created over — the CPU door's memory.
    iosurface: RetainedIOSurfaceSharedAcrossThreads,
    /// The surface's pages imported as a buffer, on the first device-tensor
    /// reach: an image's own memory maps a private copy, so the no-copy
    /// `MTLBuffer` a Metal capsule points at comes from here.
    iosurface_pages_import: std::sync::OnceLock<ConsumerVulkanBuffer>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) format: TextureFormat,
    iosurface_cpu_lock: HelperIOSurfaceCpuLock,
    /// The producer's edge of the pair. Nothing signals it today — the
    /// escalate dispatch retires its GPU work before any consumer can learn
    /// the id — so there is no wire value to wait on; the import holds the
    /// edge so a producer that starts signalling finds its consumers on it.
    #[expect(
        dead_code,
        reason = "no wire value exists to wait on yet; held for the pair's producer side"
    )]
    produce_done_timeline: ConsumerVulkanTimelineSemaphore,
    /// This side's edge, signalled once at release.
    consume_done_timeline: ConsumerVulkanTimelineSemaphore,
    /// The layout this side's image sits in — seeded from the checkout's
    /// published cell, republished at release.
    current_image_layout: VulkanLayout,
    /// Present only on an acquired texture — a resolved one belongs to its
    /// acquirer.
    pub(crate) release_to_parent: Option<HelperSurfaceReleaseDebt>,
    #[expect(
        dead_code,
        reason = "settled by its own Drop; nothing reads it, and that is the point"
    )]
    release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt,
    exchange_client: Arc<HelperProcessGpuExchangeClient>,
}

impl HelperCheckedOutTextureSurface {
    /// The host view of the texture's pixels: its IOSurface's own rows, at
    /// the surface's stride.
    pub(crate) fn host_visible_pixel_plane_view(
        &self,
    ) -> PyResult<crate::python_gpu_surface_pixel_exchange::HostVisiblePixelPlaneView> {
        let format = self.format.host_view_pixel_format().ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "texture {:?} is {:?}, which is planar; one host view cannot span it",
                self.surface_id, self.format
            ))
        })?;
        let iosurface = &self.iosurface;
        Ok(
            crate::python_gpu_surface_pixel_exchange::HostVisiblePixelPlaneView {
                base_address: iosurface.base_address().as_ptr().cast(),
                bytes_per_row: iosurface.bytes_per_row() as u64,
                width: self.width,
                height: self.height,
                format,
            },
        )
    }

    /// Take the IOSurface lock for CPU access, read-only or read-write.
    pub(crate) fn lock_the_iosurface_for_cpu_access(&self, read_only: bool) -> PyResult<()> {
        self.iosurface_cpu_lock
            .lock(&self.iosurface, read_only)
            .map_err(|refused| self.iosurface_lock_error(refused))
    }

    /// Release the IOSurface lock this texture's CPU access holds, if any.
    pub(crate) fn unlock_the_iosurface_after_cpu_access(&self) -> PyResult<()> {
        self.iosurface_cpu_lock
            .release(&self.iosurface)
            .map_err(|refused| self.iosurface_lock_error(refused))
    }

    fn iosurface_lock_error(&self, refused: IOSurfaceLockRefused) -> PyErr {
        PyRuntimeError::new_err(format!("{refused} on texture {:?}", self.surface_id))
    }

    /// A no-copy `MTLBuffer` over the texture's IOSurface rows, importing the
    /// pages on first ask.
    pub(crate) fn metal_buffer_over_the_iosurface_pages(
        &self,
    ) -> PyResult<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLBuffer>>>
    {
        let iosurface_pages_import = match self.iosurface_pages_import.get() {
            Some(already_imported) => already_imported,
            None => {
                let vulkan_device = self.exchange_client.consumer_vulkan_device()?;
                let imported =
                    ConsumerVulkanBuffer::from_iosurface_pages(&vulkan_device, &self.iosurface)
                        .map_err(|import_failure| {
                            PyRuntimeError::new_err(format!(
                                "texture {:?}'s IOSurface pages would not import as a buffer, \
                                 so no Metal buffer can alias them: {import_failure}",
                                self.surface_id
                            ))
                        })?;
                self.iosurface_pages_import.get_or_init(|| imported)
            }
        };
        iosurface_pages_import
            .exported_metal_buffer()
            .map_err(|export_failure| {
                PyRuntimeError::new_err(format!(
                    "texture {:?} has no Metal buffer over its IOSurface: {export_failure}",
                    self.surface_id
                ))
            })
    }
}

impl Drop for HelperCheckedOutTextureSurface {
    /// The release half of the crossing: let the CPU lock go, signal this
    /// side's consume edge, republish the layout. Best-effort throughout — a
    /// parent that is gone reclaims everything with the connection — and the
    /// debt fields settle after this body.
    ///
    /// The publish is a Mach round trip, so it runs detached — this can be a
    /// capsule deleter running under the child's GIL.
    fn drop(&mut self) {
        Python::attach(|python| {
            let release_failures = python.detach(|| {
                let mut release_failures: Vec<String> = Vec::new();
                if let Err(refused) = self.iosurface_cpu_lock.release(&self.iosurface) {
                    release_failures.push(refused.to_string());
                }
                let signalled = self
                    .consume_done_timeline
                    .current_value()
                    .and_then(|value| self.consume_done_timeline.signal_host(value + 1));
                if let Err(signal_failure) = signalled {
                    release_failures.push(format!("consume_done signal failed: {signal_failure}"));
                }
                if self.current_image_layout != VulkanLayout::UNDEFINED {
                    if let Err(barrier_failure) = self.consumer_texture.release_to_foreign_layout(
                        self.current_image_layout,
                        self.current_image_layout,
                    ) {
                        release_failures.push(format!("QFOT release failed: {barrier_failure}"));
                    }
                    if let Err(publish_failure) =
                        self.exchange_client.publish_image_layout_to_surface_share(
                            &self.surface_id,
                            self.current_image_layout.0,
                        )
                    {
                        release_failures.push(format!("layout publish failed: {publish_failure}"));
                    }
                }
                release_failures
            });
            if !release_failures.is_empty() {
                tracing::warn!(
                    "releasing texture surface {} left the crossing uncoordinated ({}); the \
                     service reclaims the claim when this helper's connection closes",
                    self.surface_id,
                    release_failures.join("; ")
                );
            }
        });
    }
}

/// Import one timeline edge of a texture checkout from its shared event's
/// port, or refuse the checkout naming the edge — a consumer outside the
/// pair is an unsynchronised reader.
fn import_timeline_edge_for_texture_check_out(
    vulkan_device: &Arc<ConsumerVulkanDevice>,
    surface_id: &str,
    edge_name: &str,
    edge_port: &SurfaceShareTransferredHandle,
) -> PyResult<ConsumerVulkanTimelineSemaphore> {
    ConsumerVulkanTimelineSemaphore::from_imported_metal_shared_event_mach_send_right(
        vulkan_device,
        edge_port,
    )
    .map_err(|import_failure| {
        PyRuntimeError::new_err(format!(
            "texture {surface_id:?}'s {edge_name} timeline would not import \
             ({import_failure}); a consumer outside the timeline pair is an unsynchronised \
             reader"
        ))
    })
}

impl HelperProcessGpuExchangeClient {
    /// Acquire a pooled texture over an IOSurface, then check it out and
    /// import it, so the CPU and this process's device both reach it.
    ///
    /// Called attached; the escalate wait releases the GIL, and the checkout
    /// and imports run detached.
    pub(crate) fn acquire_texture(
        self: &Arc<Self>,
        python: Python<'_>,
        width: u32,
        height: u32,
        wire_format_name: &str,
        usage: &[String],
    ) -> PyResult<HelperCheckedOutTextureSurface> {
        let op = PyDict::new(python);
        op.set_item("op", "acquire_texture")?;
        op.set_item("width", width)?;
        op.set_item("height", height)?;
        op.set_item("format", wire_format_name)?;
        op.set_item("usage", usage)?;
        let response =
            escalate_round_trip_to_parent(python, &self.escalate_request_to_parent, &op)?;
        let handle_id: String = response
            .get_item("handle_id")
            .map_err(|_| {
                PyRuntimeError::new_err(
                    "the parent's acquire_texture response carried no handle_id",
                )
            })?
            .extract()?;
        // The debt exists from the moment the parent allocated, so a refused
        // checkout or import below still hands the pool slot back.
        let release_to_parent = HelperSurfaceReleaseDebt {
            release_to_parent_without_waiting: self
                .release_to_parent_without_waiting
                .clone_ref(python),
            handle_id: handle_id.clone(),
        };
        let checked_out = python
            .detach(|| self.check_out_and_import(&handle_id))
            .map_err(|check_out_failure| {
                PyRuntimeError::new_err(format!(
                    "texture {handle_id:?} did not cross to this helper: {check_out_failure}. A \
                     texture crosses on macOS only when the engine allocated it over an \
                     IOSurface, which it does for any single-plane format on a device with \
                     VK_EXT_metal_objects"
                ))
            })?;
        let HelperCheckedOutSurface::Texture(mut checked_out_texture) = checked_out else {
            return Err(PyRuntimeError::new_err(format!(
                "acquire_texture's allocation {handle_id:?} resolved to a pixel buffer \
                 registration; a pool cannot answer a texture acquire with a buffer"
            )));
        };
        checked_out_texture.release_to_parent = Some(release_to_parent);
        Ok(checked_out_texture)
    }

    /// The texture arm of a checkout: rebuild the engine's image over the
    /// IOSurface on this process's device, join the timeline pair, and take
    /// the published layout as this side's starting point.
    pub(super) fn import_checked_out_texture(
        self: &Arc<Self>,
        surface_id: &str,
        response: &serde_json::Value,
        received_ports: Vec<SurfaceShareTransferredHandle>,
        release_check_out_to_surface_share: HelperSurfaceCheckOutLeaseDebt,
    ) -> PyResult<HelperCheckedOutTextureSurface> {
        let width = required_positive_u32_check_out_metadata_field(response, surface_id, "width")?;
        let height =
            required_positive_u32_check_out_metadata_field(response, surface_id, "height")?;
        let format_name = response
            .get("format")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let format = TextureFormat::from_wire_name(format_name).ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "check_out of texture {surface_id:?} named an unknown format {format_name:?}"
            ))
        })?;
        let usage = response
            .get("vk_image_usage")
            .and_then(|value| value.as_u64())
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                PyRuntimeError::new_err(format!(
                    "check_out of texture {surface_id:?} carried no vk_image_usage"
                ))
            })
            .map(VulkanImageUsage)?;
        let current_image_layout = VulkanLayout(
            response
                .get("current_image_layout")
                .and_then(|value| value.as_i64())
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(VulkanLayout::UNDEFINED.0),
        );

        let [iosurface_port, produce_done_port, consume_done_port] =
            <[_; 3]>::try_from(received_ports).map_err(|received_ports: Vec<_>| {
                PyRuntimeError::new_err(format!(
                    "check_out of texture {surface_id:?} carried {} port(s), not its IOSurface \
                     and both timeline edges",
                    received_ports.len()
                ))
            })?;
        let iosurface =
            objc2_io_surface::IOSurfaceRef::lookup_from_mach_port(iosurface_port.as_raw_name())
                .ok_or_else(|| {
                    PyRuntimeError::new_err(format!(
                        "check_out of texture {surface_id:?} carried a port that names no \
                         IOSurface"
                    ))
                })?;
        drop(iosurface_port);
        let vulkan_device = self.consumer_vulkan_device()?;
        let consumer_texture = ConsumerVulkanTexture::from_iosurface(
            &vulkan_device,
            &iosurface,
            width,
            height,
            format,
            usage,
        )
        .map_err(|import_failure| {
            PyRuntimeError::new_err(format!(
                "Vulkan could not import texture {surface_id:?}'s IOSurface: {import_failure}"
            ))
        })?;
        let produce_done_timeline = import_timeline_edge_for_texture_check_out(
            &vulkan_device,
            surface_id,
            "produce_done",
            &produce_done_port,
        )?;
        let consume_done_timeline = import_timeline_edge_for_texture_check_out(
            &vulkan_device,
            surface_id,
            "consume_done",
            &consume_done_port,
        )?;
        consumer_texture
            .acquire_from_foreign_layout(current_image_layout)
            .map_err(|barrier_failure| {
                PyRuntimeError::new_err(format!(
                    "texture {surface_id:?}'s acquire barrier failed: {barrier_failure}"
                ))
            })?;

        Ok(HelperCheckedOutTextureSurface {
            surface_id: surface_id.to_string(),
            consumer_texture,
            iosurface: RetainedIOSurfaceSharedAcrossThreads::new(iosurface),
            iosurface_pages_import: std::sync::OnceLock::new(),
            width,
            height,
            format,
            iosurface_cpu_lock: HelperIOSurfaceCpuLock::default(),
            produce_done_timeline,
            consume_done_timeline,
            current_image_layout,
            release_to_parent: None,
            release_check_out_to_surface_share,
            exchange_client: Arc::clone(self),
        })
    }
}

/// The macOS texture arm against a real Mach service: the registration's
/// IOSurface and timeline pair imported, the CPU door on the surface's own
/// rows, and the release. Needs a Vulkan device and a Metal device, and says
/// so rather than failing where there is neither.
#[cfg(test)]
mod texture_check_out_tests;
