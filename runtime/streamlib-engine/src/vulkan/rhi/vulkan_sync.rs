// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan synchronization primitives.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use vulkanalia::vk::KhrExternalSemaphoreFdExtensionDeviceCommands;

use crate::core::{Error, Result};

/// Vulkan semaphore wrapper for synchronization.
#[allow(dead_code)]
pub struct VulkanSemaphore {
    device: vulkanalia::Device,
    semaphore: vk::Semaphore,
}

#[allow(dead_code)]
impl VulkanSemaphore {
    /// Create a new Vulkan semaphore.
    pub fn new(device: &vulkanalia::Device) -> Result<Self> {
        let semaphore_info = vk::SemaphoreCreateInfo::builder().build();

        let semaphore = unsafe { device.create_semaphore(&semaphore_info, None) }
            .map_err(|e| Error::GpuError(format!("Failed to create semaphore: {e}")))?;

        Ok(Self {
            device: device.clone(),
            semaphore,
        })
    }

    /// Get the underlying Vulkan semaphore handle.
    pub fn semaphore(&self) -> vk::Semaphore {
        self.semaphore
    }
}

impl Drop for VulkanSemaphore {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_semaphore(self.semaphore, None);
        }
    }
}

// VulkanSemaphore is Send + Sync because Vulkan handles are thread-safe
unsafe impl Send for VulkanSemaphore {}
unsafe impl Sync for VulkanSemaphore {}

/// Vulkan fence wrapper for CPU-GPU synchronization.
#[allow(dead_code)]
pub struct VulkanFence {
    device: vulkanalia::Device,
    fence: vk::Fence,
}

#[allow(dead_code)]
impl VulkanFence {
    /// Create a new Vulkan fence.
    ///
    /// # Arguments
    /// * `device` - The Vulkan device
    /// * `signaled` - Whether to create the fence in signaled state
    pub fn new(device: &vulkanalia::Device, signaled: bool) -> Result<Self> {
        let flags = if signaled {
            vk::FenceCreateFlags::SIGNALED
        } else {
            vk::FenceCreateFlags::empty()
        };

        let fence_info = vk::FenceCreateInfo::builder().flags(flags).build();

        let fence = unsafe { device.create_fence(&fence_info, None) }
            .map_err(|e| Error::GpuError(format!("Failed to create fence: {e}")))?;

        Ok(Self {
            device: device.clone(),
            fence,
        })
    }

    /// Wait for the fence to be signaled.
    ///
    /// # Arguments
    /// * `timeout_ns` - Timeout in nanoseconds (u64::MAX for no timeout)
    pub fn wait(&self, timeout_ns: u64) -> Result<()> {
        unsafe { self.device.wait_for_fences(&[self.fence], true, timeout_ns) }
            .map(|_| ())
            .map_err(|e| Error::GpuError(format!("Failed to wait for fence: {e}")))
    }

    /// Reset the fence to unsignaled state.
    pub fn reset(&self) -> Result<()> {
        unsafe { self.device.reset_fences(&[self.fence]) }
            .map(|_| ())
            .map_err(|e| Error::GpuError(format!("Failed to reset fence: {e}")))
    }

    /// Get the underlying Vulkan fence handle.
    pub fn fence(&self) -> vk::Fence {
        self.fence
    }
}

impl Drop for VulkanFence {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_fence(self.fence, None);
        }
    }
}

// VulkanFence is Send + Sync because Vulkan handles are thread-safe
unsafe impl Send for VulkanFence {}
unsafe impl Sync for VulkanFence {}

/// Vulkan **timeline** semaphore wrapper.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[allow(dead_code)]
///
/// Timeline semaphores carry a monotonically-increasing 64-bit counter.
/// Submitters wait on a value and signal a higher value; the wait
/// completes when the counter has reached or surpassed the requested
/// value. This is the synchronization primitive used by surface
/// adapters: each per-surface acquire/release pair advances the counter.
///
/// Created with `VkSemaphoreTypeCreateInfo` chained into the standard
/// `VkSemaphoreCreateInfo`. Optionally created with
/// `VkExportSemaphoreCreateInfo` so [`Self::export_opaque_fd`] can hand a
/// file descriptor to a subprocess, which imports it via
/// [`Self::from_imported_opaque_fd`] into its own `VkDevice`. The two
/// processes then signal/wait the same timeline.
///
/// Host-internal. The public exportable-timeline surface is the
/// `#[repr(C)]` `HostTimelineSemaphore` handle, minted by the FullAccess
/// `create_exportable_timeline_semaphore` slot which builds one of these
/// via [`Self::new_exportable`]. That host-side backing is why the
/// `new` / `new_exportable` / `create` constructor bodies must stay free
/// of any `host_inner()` guard.
pub struct HostVulkanTimelineSemaphore {
    device: vulkanalia::Device,
    semaphore: vk::Semaphore,
    /// Whether the caller asked for cross-process export via
    /// [`Self::new_exportable`].
    ///
    /// What was asked for, not what the object can do: where the platform or
    /// driver has no export mechanism the request is honoured without the
    /// export declaration, and the export methods refuse.
    cross_process_export_was_requested: bool,
    /// Whether `VkExportMetalObjectCreateInfoEXT{METAL_SHARED_EVENT}` was
    /// chained at creation, so MoltenVK hands the backing `MTLSharedEvent`
    /// to [`Self::export_metal_shared_event_mach_send_right`].
    #[cfg(target_os = "macos")]
    metal_shared_event_export_was_declared: bool,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl HostVulkanTimelineSemaphore {
    /// Create an in-process timeline semaphore (no export).
    ///
    /// Pair with [`Self::wait`] / [`Self::signal_host`] for
    /// single-process work, or pass the raw [`Self::semaphore`] handle
    /// to a `vkQueueSubmit2` `signal_semaphore_infos` slot for
    /// GPU-side advance — the
    /// [`RhiCommandRecorder`](super::RhiCommandRecorder) `submit_signaling_timeline`
    /// path is the canonical entry point.
    /// Use [`Self::new_exportable`] when the timeline must be shared
    /// with a subprocess via sync-fd.
    pub fn new(device: &vulkanalia::Device, initial_value: u64) -> Result<Self> {
        Self::create(device, initial_value, false)
    }

    /// Create an exportable timeline semaphore.
    ///
    /// On Linux `vkGetSemaphoreFdKHR` hands a fresh OPAQUE_FD per
    /// [`Self::export_opaque_fd`] call; ownership transfers to the caller
    /// (close after use, or pass via SCM_RIGHTS). On macOS the backing
    /// `MTLSharedEvent` is declared exportable, and
    /// [`Self::export_metal_shared_event_mach_send_right`] mints a Mach send
    /// right to it.
    pub fn new_exportable(device: &vulkanalia::Device, initial_value: u64) -> Result<Self> {
        Self::create(device, initial_value, true)
    }

    fn create(
        device: &vulkanalia::Device,
        initial_value: u64,
        cross_process_export_was_requested: bool,
    ) -> Result<Self> {
        let mut type_info = vk::SemaphoreTypeCreateInfo::builder()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(initial_value)
            .build();
        let mut export_info = vk::ExportSemaphoreCreateInfo::builder()
            .handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
            .build();
        let mut metal_export_info = vk::ExportMetalObjectCreateInfoEXT::builder()
            .export_object_type(vk::ExportMetalObjectTypeFlagsEXT::METAL_SHARED_EVENT)
            .build();
        let metal_shared_event_export_is_declared = cfg!(target_os = "macos")
            && cross_process_export_was_requested
            && device
                .extensions()
                .contains(&vk::EXT_METAL_OBJECTS_EXTENSION.name);

        // Each export struct is chained ahead of `type_info` by hand: the
        // builder's pNext takes `&mut` and would borrow `type_info` for the
        // struct's life.
        let info = if cross_process_export_was_requested
            && super::CROSS_PROCESS_EXPORT_BY_FILE_DESCRIPTOR_EXISTS_ON_THIS_PLATFORM
        {
            export_info.next = (&mut type_info as *mut _) as *mut std::ffi::c_void;
            vk::SemaphoreCreateInfo::builder()
                .push_next(&mut export_info)
                .build()
        } else if metal_shared_event_export_is_declared {
            metal_export_info.next = (&mut type_info as *mut _) as *mut std::ffi::c_void;
            vk::SemaphoreCreateInfo::builder()
                .push_next(&mut metal_export_info)
                .build()
        } else {
            vk::SemaphoreCreateInfo::builder()
                .push_next(&mut type_info)
                .build()
        };

        let semaphore = unsafe { device.create_semaphore(&info, None) }.map_err(|e| {
            Error::GpuError(format!(
                "Failed to create timeline semaphore \
                 (exportable={cross_process_export_was_requested}): {e}"
            ))
        })?;

        Ok(Self {
            device: device.clone(),
            semaphore,
            cross_process_export_was_requested,
            #[cfg(target_os = "macos")]
            metal_shared_event_export_was_declared: metal_shared_event_export_is_declared,
        })
    }

    /// Import a timeline semaphore from an OPAQUE_FD handed in by the
    /// host. Subprocess side of [`Self::export_opaque_fd`].
    ///
    /// `VK_SEMAPHORE_IMPORT_TEMPORARY_BIT` is NOT used: the imported
    /// semaphore takes permanent payload ownership, matching how DMA-BUF
    /// memory imports are bound for surface lifetime.
    ///
    /// On success the kernel fd ownership transfers to the Vulkan driver;
    /// the caller MUST NOT close `fd` afterwards. On error the caller
    /// retains ownership and is responsible for closing it.
    pub fn from_imported_opaque_fd(
        device: &vulkanalia::Device,
        fd: std::os::unix::io::RawFd,
    ) -> Result<Self> {
        // The semaphore must already exist before import. Create it as a
        // timeline semaphore with initial value 0; the import then
        // replaces the payload with the host's timeline state.
        let mut type_info = vk::SemaphoreTypeCreateInfo::builder()
            .semaphore_type(vk::SemaphoreType::TIMELINE)
            .initial_value(0)
            .build();
        let info = vk::SemaphoreCreateInfo::builder()
            .push_next(&mut type_info)
            .build();
        let semaphore = unsafe { device.create_semaphore(&info, None) }.map_err(|e| {
            Error::GpuError(format!(
                "Failed to create receiving timeline semaphore for import: {e}"
            ))
        })?;

        let import_info = vk::ImportSemaphoreFdInfoKHR::builder()
            .semaphore(semaphore)
            .flags(vk::SemaphoreImportFlags::empty())
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
            .fd(fd)
            .build();

        let import_result = unsafe { device.import_semaphore_fd_khr(&import_info) };
        if let Err(e) = import_result {
            unsafe { device.destroy_semaphore(semaphore, None) };
            return Err(Error::GpuError(format!(
                "vkImportSemaphoreFdKHR failed: {e}"
            )));
        }

        Ok(Self {
            device: device.clone(),
            semaphore,
            cross_process_export_was_requested: false,
            #[cfg(target_os = "macos")]
            metal_shared_event_export_was_declared: false,
        })
    }

    /// Export the semaphore as a fresh OPAQUE_FD suitable for SCM_RIGHTS
    /// passing to a subprocess. Each call returns a NEW fd; callers own
    /// the returned fd and must close it after use (or after the
    /// subprocess has imported its own copy).
    pub fn export_opaque_fd(&self) -> Result<std::os::unix::io::RawFd> {
        if !self.cross_process_export_was_requested {
            return Err(Error::GpuError(
                "HostVulkanTimelineSemaphore::export_opaque_fd: semaphore was not created with `new_exportable`".into(),
            ));
        }
        if !super::CROSS_PROCESS_EXPORT_BY_FILE_DESCRIPTOR_EXISTS_ON_THIS_PLATFORM {
            return Err(Error::GpuError(
                "HostVulkanTimelineSemaphore::export_opaque_fd: OPAQUE_FD semaphore export is \
                 a Linux mechanism and this platform has no vkGetSemaphoreFdKHR — the Apple \
                 cross-process timeline is a Metal shared event \
                 (export_metal_shared_event_mach_send_right)"
                    .into(),
            ));
        }
        let info = vk::SemaphoreGetFdInfoKHR::builder()
            .semaphore(self.semaphore)
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD)
            .build();
        let fd = unsafe { self.device.get_semaphore_fd_khr(&info) }
            .map_err(|e| Error::GpuError(format!("vkGetSemaphoreFdKHR failed: {e}")))?;
        Ok(fd)
    }

    /// Block until the timeline counter has reached or surpassed `value`.
    ///
    /// `timeout_ns` is the per-call timeout; pass `u64::MAX` for "no
    /// timeout". Returns `Ok(())` only when the value was reached:
    /// `vkWaitSemaphores` reports a timeout as `VK_TIMEOUT` — a *positive*
    /// success code — which maps to [`Error::GpuError`] here, alongside
    /// genuine driver failures.
    pub fn wait(&self, value: u64, timeout_ns: u64) -> Result<()> {
        let semaphores = [self.semaphore];
        let values = [value];
        let info = vk::SemaphoreWaitInfo::builder()
            .flags(vk::SemaphoreWaitFlags::empty())
            .semaphores(&semaphores)
            .values(&values)
            .build();
        let outcome = unsafe { self.device.wait_semaphores(&info, timeout_ns) }.map_err(|e| {
            Error::GpuError(format!(
                "vkWaitSemaphores(value={value}, timeout_ns={timeout_ns}): {e}"
            ))
        })?;
        if outcome == vk::SuccessCode::TIMEOUT {
            return Err(Error::GpuError(format!(
                "vkWaitSemaphores(value={value}) timed out after {timeout_ns} ns"
            )));
        }
        Ok(())
    }

    /// Host-side signal: advance the counter to `value` from the CPU.
    /// Used when the producer has finished writing on the host side
    /// and wants to release the surface to the next consumer.
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
        unsafe { self.device.signal_semaphore(&info) }
            .map_err(|e| Error::GpuError(format!("vkSignalSemaphore(value={value}): {e}")))
    }

    /// Read the current timeline counter value via
    /// `vkGetSemaphoreCounterValue`. Used by tests and progress reporting.
    pub fn current_value(&self) -> Result<u64> {
        unsafe { self.device.get_semaphore_counter_value(self.semaphore) }
            .map_err(|e| Error::GpuError(format!("vkGetSemaphoreCounterValue: {e}")))
    }

    /// Raw `vk::Semaphore` handle for inclusion in queue submit infos.
    pub fn semaphore(&self) -> vk::Semaphore {
        self.semaphore
    }

    /// Whether cross-process export was requested at creation.
    pub fn is_exportable(&self) -> bool {
        self.cross_process_export_was_requested
    }

    /// A Mach send right to the `MTLSharedEvent` MoltenVK backs this timeline
    /// with — the macOS peer of [`Self::export_opaque_fd`]. The Vulkan value
    /// is the event's `signaledValue`, one to one. Errors when the export was
    /// not declared at creation (no [`Self::new_exportable`], or no
    /// `VK_EXT_metal_objects`) or MoltenVK hands back no event; the caller
    /// then orders host-side.
    #[cfg(target_os = "macos")]
    pub fn export_metal_shared_event_mach_send_right(
        &self,
    ) -> Result<streamlib_surface_client::OwnedMachSendRight> {
        use objc2_metal::MTLSharedEvent;
        use vulkanalia::vk::ExtMetalObjectsExtensionDeviceCommands;

        if !self.metal_shared_event_export_was_declared {
            return Err(Error::GpuError(
                "HostVulkanTimelineSemaphore: no Metal shared-event export was declared at \
                 creation (not new_exportable, or the device lacks VK_EXT_metal_objects)"
                    .into(),
            ));
        }
        let mut shared_event_info = vk::ExportMetalSharedEventInfoEXT::builder()
            .semaphore(self.semaphore)
            .build();
        let mut objects_info = vk::ExportMetalObjectsInfoEXT::builder().build();
        objects_info.next = (&mut shared_event_info as *mut _) as *const std::ffi::c_void;
        // SAFETY: the extension is enabled (checked at creation) and the chain
        // names this semaphore; MoltenVK writes the event pointer back.
        unsafe { self.device.export_metal_objects_ext(&mut objects_info) };

        let shared_event_pointer = shared_event_info.mtl_shared_event
            as *mut objc2::runtime::ProtocolObject<dyn MTLSharedEvent>;
        // SAFETY: MoltenVK returns the semaphore's own `MTLSharedEvent`,
        // alive for the semaphore's lifetime and not retained for the caller.
        let shared_event = unsafe { shared_event_pointer.as_ref() }.ok_or_else(|| {
            Error::GpuError(
                "vkExportMetalObjectsEXT returned no MTLSharedEvent for the timeline".into(),
            )
        })?;
        streamlib_surface_client::mach_send_right_of_metal_shared_event_handle(
            &shared_event.newSharedEventHandle(),
        )
        .map_err(|e| {
            Error::GpuError(format!(
                "the timeline's MTLSharedEvent did not yield a Mach send right: {e}"
            ))
        })
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for HostVulkanTimelineSemaphore {
    fn drop(&mut self) {
        unsafe { self.device.destroy_semaphore(self.semaphore, None) };
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
unsafe impl Send for HostVulkanTimelineSemaphore {}
#[cfg(any(target_os = "linux", target_os = "macos"))]
unsafe impl Sync for HostVulkanTimelineSemaphore {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl super::VulkanTimelineSemaphoreLike for HostVulkanTimelineSemaphore {
    fn wait(&self, value: u64, timeout_ns: u64) -> streamlib_consumer_rhi::Result<()> {
        HostVulkanTimelineSemaphore::wait(self, value, timeout_ns)
            .map_err(|e| streamlib_consumer_rhi::ConsumerRhiError::Gpu(e.to_string()))
    }
    fn signal_host(&self, value: u64) -> streamlib_consumer_rhi::Result<()> {
        HostVulkanTimelineSemaphore::signal_host(self, value)
            .map_err(|e| streamlib_consumer_rhi::ConsumerRhiError::Gpu(e.to_string()))
    }
    fn current_value(&self) -> streamlib_consumer_rhi::Result<u64> {
        HostVulkanTimelineSemaphore::current_value(self)
            .map_err(|e| streamlib_consumer_rhi::ConsumerRhiError::Gpu(e.to_string()))
    }
    fn semaphore(&self) -> vulkanalia::vk::Semaphore {
        HostVulkanTimelineSemaphore::semaphore(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan::rhi::HostVulkanDevice;

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn test_semaphore_creation() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };

        let semaphore = VulkanSemaphore::new(device.device());
        assert!(semaphore.is_ok(), "Semaphore creation should succeed");
        println!("Vulkan semaphore created successfully");
    }

    /// `vkWaitSemaphores` reports timeout as `VK_TIMEOUT` — a *positive*
    /// success code — so a wrapper that only checks `Err` silently converts
    /// a timed-out wait into `Ok`. Mental-revert: with the `SuccessCode::
    /// TIMEOUT` mapping removed from [`HostVulkanTimelineSemaphore::wait`],
    /// this test fails at the `expect_err`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn timeline_semaphore_bounded_wait_reports_timeout_as_error() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };
        let sem = HostVulkanTimelineSemaphore::new(device.device(), 0)
            .expect("create timeline semaphore");
        let timeout_error = sem
            .wait(1, 1_000_000)
            .expect_err("a 1 ms wait on a never-signaled value must be an error, not Ok");
        assert!(
            format!("{timeout_error}").contains("timed out"),
            "error names the timeout: {timeout_error}"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn timeline_semaphore_host_signal_advances_counter() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };
        let sem = HostVulkanTimelineSemaphore::new(device.device(), 0)
            .expect("create timeline semaphore");
        assert_eq!(sem.current_value().unwrap(), 0);
        sem.signal_host(7).expect("host signal");
        assert_eq!(sem.current_value().unwrap(), 7);
        // wait on a value already reached returns immediately.
        sem.wait(7, 0).expect("wait on already-reached value");
    }

    /// `new_exportable` plus `export_opaque_fd` returns a valid kernel
    /// fd. Sufficient to confirm `VK_KHR_external_semaphore_fd` is wired.
    /// Cross-process import is exercised by the surface-adapter
    /// integration tests in `streamlib-adapter-vulkan`.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn timeline_semaphore_exports_valid_opaque_fd() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };
        let sem = match HostVulkanTimelineSemaphore::new_exportable(device.device(), 0) {
            Ok(s) => s,
            Err(_) => {
                println!("Skipping — VK_KHR_external_semaphore_fd unavailable on this driver");
                return;
            }
        };
        let fd = sem.export_opaque_fd().expect("export_opaque_fd");
        assert!(fd >= 0, "exported sync fd should be a valid kernel fd");
        unsafe { libc::close(fd) };
    }

    /// Where the fd handle type does not exist, export refuses by name. Without
    /// the guard this reaches `vkGetSemaphoreFdKHR`, which the loader never
    /// resolved — vulkanalia's unloaded-command stub panics, and a panic in this
    /// position aborts the whole test binary rather than failing one case.
    #[cfg(not(target_os = "linux"))]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn timeline_semaphore_export_refuses_where_there_is_no_fd_handle_type() {
        let device = HostVulkanDevice::new().expect("the rig must produce a Vulkan device");
        let semaphore = HostVulkanTimelineSemaphore::new_exportable(device.device(), 0)
            .expect("a timeline semaphore must still be creatable without fd export");

        let refusal = semaphore
            .export_opaque_fd()
            .expect_err("a platform without vkGetSemaphoreFdKHR must refuse the export");
        assert!(
            refusal.to_string().contains("Linux mechanism"),
            "the refusal must say why rather than blaming the caller: {refusal}"
        );
    }

    /// The exported send right names the timeline's own `MTLSharedEvent`:
    /// a value set on either side is the value the other reads.
    #[cfg(target_os = "macos")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn timeline_semaphore_exports_its_metal_shared_event_as_a_mach_send_right() {
        use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice, MTLSharedEvent};

        let device = HostVulkanDevice::new().expect("the rig must produce a Vulkan device");
        let semaphore = HostVulkanTimelineSemaphore::new_exportable(device.device(), 3)
            .expect("an exportable timeline");
        let send_right = semaphore
            .export_metal_shared_event_mach_send_right()
            .expect("MoltenVK exports the timeline's shared event");
        let handle =
            streamlib_surface_client::metal_shared_event_handle_of_mach_send_right(&send_right)
                .expect("the send right rebuilds a handle");
        let shared_event = MTLCreateSystemDefaultDevice()
            .expect("a Metal device")
            .newSharedEventWithHandle(&handle)
            .expect("the handle names a live shared event");

        assert_eq!(shared_event.signaledValue(), 3);
        semaphore.signal_host(5).expect("host signal");
        assert_eq!(shared_event.signaledValue(), 5);
        shared_event.setSignaledValue(9);
        assert_eq!(semaphore.current_value().expect("counter"), 9);
    }

    #[cfg(target_os = "macos")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn a_timeline_not_created_exportable_refuses_the_metal_shared_event_export() {
        let device = HostVulkanDevice::new().expect("the rig must produce a Vulkan device");
        let semaphore =
            HostVulkanTimelineSemaphore::new(device.device(), 0).expect("an in-process timeline");
        let refusal = semaphore
            .export_metal_shared_event_mach_send_right()
            .expect_err("no export was declared");
        assert!(
            refusal
                .to_string()
                .contains("no Metal shared-event export was declared"),
            "the refusal names the missing declaration: {refusal}"
        );
    }

    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn test_fence_creation() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping test - Vulkan not available");
                return;
            }
        };

        // Test unsignaled fence
        let fence = VulkanFence::new(device.device(), false);
        assert!(fence.is_ok(), "Fence creation should succeed");

        // Test signaled fence
        let signaled_fence = VulkanFence::new(device.device(), true);
        assert!(
            signaled_fence.is_ok(),
            "Signaled fence creation should succeed"
        );

        // Wait on signaled fence should return immediately
        let fence = signaled_fence.unwrap();
        let result = fence.wait(0);
        assert!(result.is_ok(), "Wait on signaled fence should succeed");

        println!("Vulkan fence tests passed");
    }
}
