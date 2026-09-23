// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Consumer-side timeline semaphore — imports a host-allocated
//! exportable timeline semaphore (an OPAQUE_FD on Linux, a Metal shared
//! event's Mach send right on macOS) and exposes the wait /
//! signal-from-host / counter-read operations.
//!
//! Mirrors [`crate::ConsumerVulkanTexture`] for sync primitives.
//! There is no `new` / `new_exportable` constructor: the consumer
//! never originates a timeline semaphore — it only imports one the
//! host already created.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrExternalSemaphoreFdExtensionDeviceCommands;

use crate::{ConsumerRhiError, ConsumerVulkanDevice, Result, VulkanTimelineSemaphoreLike};

/// Consumer-side timeline semaphore. See module docs.
pub struct ConsumerVulkanTimelineSemaphore {
    vulkan_device: Arc<ConsumerVulkanDevice>,
    semaphore: vk::Semaphore,
}

impl ConsumerVulkanTimelineSemaphore {
    /// Import a host-side exportable timeline semaphore via OPAQUE_FD.
    ///
    /// The consumer creates a fresh `VkSemaphore` against its own
    /// device, then `vkImportSemaphoreFdKHR` replaces the payload with
    /// the host's timeline state. After import, `wait` /
    /// `signal_host` / `current_value` operate against the same
    /// timeline as the host.
    ///
    /// fd ownership transfers to the Vulkan driver on success — caller
    /// must NOT close `fd` afterwards. On error the caller still owns
    /// it.
    pub fn from_imported_opaque_fd(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        fd: std::os::unix::io::RawFd,
    ) -> Result<Self> {
        let device = vulkan_device.device();
        let mut type_info = vk::SemaphoreTypeCreateInfo::builder()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0)
            .build();
        let info = vk::SemaphoreCreateInfo::builder()
            .push_next(&mut type_info)
            .build();
        let semaphore = unsafe { device.create_semaphore(&info, None) }.map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanTimelineSemaphore: create_semaphore failed: {e}"
            ))
        })?;

        let import_info = vk::ImportSemaphoreFdInfoKHR::builder()
            .semaphore(semaphore)
            .flags(vk::SemaphoreImportFlags::empty())
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
            .fd(fd)
            .build();

        if let Err(e) = unsafe { device.import_semaphore_fd_khr(&import_info) } {
            unsafe { device.destroy_semaphore(semaphore, None) };
            return Err(ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanTimelineSemaphore: import_semaphore_fd_khr failed: {e}"
            )));
        }

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            semaphore,
        })
    }

    /// Import a host-side exportable timeline semaphore from the Mach send
    /// right to its Metal shared event.
    ///
    /// The handle is rebuilt from the right, the event from the handle, and
    /// the event imported through `VkImportMetalSharedEventInfoEXT`. The
    /// caller keeps `send_right`; the event holds its own reference. Errors,
    /// never crashes, when the right names no shared event or the device
    /// lacks `VK_EXT_metal_objects` — the caller then orders host-side.
    #[cfg(target_os = "macos")]
    pub fn from_imported_metal_shared_event_mach_send_right(
        vulkan_device: &Arc<ConsumerVulkanDevice>,
        send_right: &streamlib_surface_client::OwnedMachSendRight,
    ) -> Result<Self> {
        use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};

        let device = vulkan_device.device();
        if !device
            .extensions()
            .contains(&vk::EXT_METAL_OBJECTS_EXTENSION.name)
        {
            return Err(ConsumerRhiError::Gpu(
                "ConsumerVulkanTimelineSemaphore: the consumer device has no \
                 VK_EXT_metal_objects, so a Metal shared event cannot be imported"
                    .into(),
            ));
        }
        let handle =
            streamlib_surface_client::metal_shared_event_handle_of_mach_send_right(send_right)
                .map_err(|e| {
                    ConsumerRhiError::Gpu(format!(
                        "ConsumerVulkanTimelineSemaphore: the send right rebuilt no shared-event \
                 handle: {e}"
                    ))
                })?;
        let shared_event = MTLCreateSystemDefaultDevice()
            .and_then(|metal_device| metal_device.newSharedEventWithHandle(&handle))
            .ok_or_else(|| {
                ConsumerRhiError::Gpu(
                    "ConsumerVulkanTimelineSemaphore: the send right names no live Metal \
                     shared event"
                        .into(),
                )
            })?;

        let mut import_info = vk::ImportMetalSharedEventInfoEXT::builder().build();
        import_info.mtl_shared_event = objc2::rc::Retained::as_ptr(&shared_event).cast_mut().cast();
        // MoltenVK writes the initial value to the shared counter. The event
        // ignores a decrease, so 0 keeps the producer's value; anything larger
        // would silently raise it.
        let mut type_info = vk::SemaphoreTypeCreateInfo::builder()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0)
            .build();
        type_info.next = (&mut import_info as *mut _) as *const std::ffi::c_void;
        let info = vk::SemaphoreCreateInfo::builder()
            .push_next(&mut type_info)
            .build();
        let semaphore = unsafe { device.create_semaphore(&info, None) }.map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanTimelineSemaphore: create_semaphore importing the shared \
                 event failed: {e}"
            ))
        })?;

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            semaphore,
        })
    }

    /// Block until the timeline counter has reached or surpassed
    /// `value`. `timeout_ns` of `u64::MAX` means "no timeout".
    pub fn wait(&self, value: u64, timeout_ns: u64) -> Result<()> {
        let semaphores = [self.semaphore];
        let values = [value];
        let info = vk::SemaphoreWaitInfo::builder()
            .flags(vk::SemaphoreWaitFlags::empty())
            .semaphores(&semaphores)
            .values(&values)
            .build();
        unsafe {
            self.vulkan_device
                .device()
                .wait_semaphores(&info, timeout_ns)
        }
        .map(|_| ())
        .map_err(|e| {
            ConsumerRhiError::Gpu(format!(
                "wait_semaphores(value={value}, timeout_ns={timeout_ns}): {e}"
            ))
        })
    }

    /// Host-side signal: advance the counter to `value` from the CPU.
    ///
    /// Single-writer-per-edge per
    /// `docs/architecture/adapter-timeline-single-writer.md`: only
    /// one process ever signals a given timeline, so `value` is
    /// strictly greater than the current value by construction and
    /// VUID-VkSemaphoreSignalInfo-value-03258 holds without runtime
    /// clamping.
    pub fn signal_host(&self, value: u64) -> Result<()> {
        let info = vk::SemaphoreSignalInfo::builder()
            .semaphore(self.semaphore)
            .value(value)
            .build();
        unsafe { self.vulkan_device.device().signal_semaphore(&info) }
            .map_err(|e| ConsumerRhiError::Gpu(format!("signal_semaphore(value={value}): {e}")))
    }

    /// Read the timeline counter via `vkGetSemaphoreCounterValue`.
    pub fn current_value(&self) -> Result<u64> {
        unsafe {
            self.vulkan_device
                .device()
                .get_semaphore_counter_value(self.semaphore)
        }
        .map_err(|e| ConsumerRhiError::Gpu(format!("get_semaphore_counter_value: {e}")))
    }

    /// Raw `vk::Semaphore` handle for inclusion in queue submit infos.
    pub fn semaphore(&self) -> vk::Semaphore {
        self.semaphore
    }
}

impl Drop for ConsumerVulkanTimelineSemaphore {
    fn drop(&mut self) {
        unsafe {
            self.vulkan_device
                .device()
                .destroy_semaphore(self.semaphore, None)
        };
    }
}

unsafe impl Send for ConsumerVulkanTimelineSemaphore {}
unsafe impl Sync for ConsumerVulkanTimelineSemaphore {}

impl VulkanTimelineSemaphoreLike for ConsumerVulkanTimelineSemaphore {
    fn wait(&self, value: u64, timeout_ns: u64) -> Result<()> {
        ConsumerVulkanTimelineSemaphore::wait(self, value, timeout_ns)
    }
    fn signal_host(&self, value: u64) -> Result<()> {
        ConsumerVulkanTimelineSemaphore::signal_host(self, value)
    }
    fn current_value(&self) -> Result<u64> {
        ConsumerVulkanTimelineSemaphore::current_value(self)
    }
    fn semaphore(&self) -> vk::Semaphore {
        ConsumerVulkanTimelineSemaphore::semaphore(self)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice, MTLSharedEvent};

    use super::*;

    fn try_create_device() -> Option<Arc<ConsumerVulkanDevice>> {
        match ConsumerVulkanDevice::new() {
            Ok(device) => Some(Arc::new(device)),
            Err(unavailable) => {
                println!("Skipping test — ConsumerVulkanDevice unavailable: {unavailable}");
                None
            }
        }
    }

    /// The import joins the producer's timeline at its current value — a
    /// reset to the import's `initialValue` would read 0 — and a host signal
    /// on either side is the value the other reads.
    #[test]
    fn a_shared_event_imports_at_the_producers_value_and_both_sides_observe_each_other() {
        let Some(device) = try_create_device() else {
            return;
        };
        let shared_event = MTLCreateSystemDefaultDevice()
            .and_then(|metal_device| metal_device.newSharedEvent())
            .expect("a Metal shared event");
        shared_event.setSignaledValue(40);
        let send_right = streamlib_surface_client::mach_send_right_of_metal_shared_event_handle(
            &shared_event.newSharedEventHandle(),
        )
        .expect("the handle's send right");

        let imported =
            ConsumerVulkanTimelineSemaphore::from_imported_metal_shared_event_mach_send_right(
                &device,
                &send_right,
            )
            .expect("the shared event imports");

        assert_eq!(imported.current_value().expect("counter"), 40);
        imported.signal_host(41).expect("host signal");
        assert_eq!(shared_event.signaledValue(), 41);
        shared_event.setSignaledValue(45);
        imported
            .wait(45, 1_000_000_000)
            .expect("the consumer observes the producer's signal");
    }

    #[test]
    fn a_send_right_naming_no_shared_event_refuses_the_import() {
        let Some(device) = try_create_device() else {
            return;
        };
        let unrelated =
            streamlib_surface_client::OwnedMachReceiveRight::allocate().expect("a receive right");
        let send_right = unrelated.make_send_right().expect("a send right");

        let Err(refusal) =
            ConsumerVulkanTimelineSemaphore::from_imported_metal_shared_event_mach_send_right(
                &device,
                &send_right,
            )
        else {
            panic!("a port naming no shared event must not import");
        };
        assert!(
            refusal
                .to_string()
                .contains("names no live Metal shared event"),
            "the refusal names the dead handle: {refusal}"
        );
    }
}
