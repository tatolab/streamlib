// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Building a consumer device and its queues on the physical device the runtime named.

use super::*;

use super::consumer_vulkan_instance_and_device_selection::index_of_the_device_the_runtime_named;

impl ConsumerVulkanDevice {
    /// Construct a fresh consumer-only Vulkan device on the physical device
    /// whose `deviceUUID` is `vulkan_device_uuid_the_runtime_named`, refusing
    /// by name when no enumerated device carries it.
    ///
    /// Enables only the extensions / features the carve-out needs — the
    /// platform's import extensions plus the Vulkan 1.3 sync-2 and
    /// timeline-semaphore features. A missing [`REQUIRED_DEVICE_EXTENSIONS`]
    /// entry refuses the device with [`ConsumerRhiError::Gpu`]; a missing
    /// [`PROBED_DEVICE_EXTENSIONS`] entry is left disabled, and the import
    /// arm that needs it refuses by name.
    pub fn new_on_the_device_the_runtime_named(
        vulkan_device_uuid_the_runtime_named: [u8; 16],
    ) -> Result<Self> {
        Self::new_with_queues_on_the_device_the_runtime_named(
            vulkan_device_uuid_the_runtime_named,
            ConsumerVulkanDeviceQueues::ImportOnly,
        )
    }

    /// A consumer device that also owns a Vulkan Video queue coding in
    /// `direction` — the owned-device tier a codec module encodes or decodes
    /// on, created on the device whose `deviceUUID` is
    /// `vulkan_device_uuid_the_runtime_named` so frames move between it and
    /// the runtime. Refused by name when that device has no such queue family
    /// or lacks one of the direction's required extensions.
    #[cfg(target_os = "linux")]
    pub fn new_with_a_video_codec_queue_on_the_device_the_runtime_named(
        vulkan_device_uuid_the_runtime_named: [u8; 16],
        direction: VulkanVideoCodecDirection,
    ) -> Result<Self> {
        Self::new_with_queues_on_the_device_the_runtime_named(
            vulkan_device_uuid_the_runtime_named,
            ConsumerVulkanDeviceQueues::ImportAndVideoCodec(direction),
        )
    }

    fn new_with_queues_on_the_device_the_runtime_named(
        vulkan_device_uuid_the_runtime_named: [u8; 16],
        queues: ConsumerVulkanDeviceQueues,
    ) -> Result<Self> {
        #[cfg_attr(
            not(target_os = "linux"),
            expect(
                unused_variables,
                reason = "only the Linux external renderer reads the loader's vkGetInstanceProcAddr"
            )
        )]
        let (entry, instance, get_instance_proc_addr) = create_the_consumer_vulkan_instance()?;
        let (physical_devices, mut enumerated_devices): (Vec<_>, Vec<_>) =
            physical_devices_the_loader_enumerates_on(&instance)?
                .into_iter()
                .unzip();
        let index = index_of_the_device_the_runtime_named(
            &enumerated_devices,
            vulkan_device_uuid_the_runtime_named,
        )?;
        let physical_device = physical_devices[index];
        let VulkanDeviceTheLoaderEnumerates {
            device_name,
            device_uuid: physical_device_uuid,
            device_type,
        } = enumerated_devices.swap_remove(index);

        // Pick any GRAPHICS-capable queue family (graphics implies transfer +
        // compute on every conformant Vulkan implementation, which covers
        // every layout transition + sync the carve-out needs).
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let queue_family_index = queue_families
            .iter()
            .enumerate()
            .find(|(_, props)| props.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .map(|(idx, _)| idx as u32)
            .ok_or_else(|| {
                ConsumerRhiError::Gpu("No graphics queue family on consumer device".into())
            })?;

        let available_device_extension_properties = unsafe {
            instance
                .enumerate_device_extension_properties(physical_device, None)
                .map_err(|e| {
                    ConsumerRhiError::Gpu(format!("enumerate_device_extension_properties: {e}"))
                })?
        };
        let available_device_ext_names =
            vulkan_extension_names_borrowed_from_properties(&available_device_extension_properties);

        let device_is_software = device_type == vk::PhysicalDeviceType::CPU;
        let mut device_extensions: Vec<*const c_char> = import_device_extensions_to_enable(
            &available_device_ext_names,
            REQUIRED_DEVICE_EXTENSIONS,
            PROBED_DEVICE_EXTENSIONS,
            if device_is_software {
                PROBED_DEVICE_EXTENSIONS_WITHHELD_FROM_A_SOFTWARE_DEVICE
            } else {
                &[]
            },
        )
        .map_err(|missing_required_extension| {
            ConsumerRhiError::Gpu(format!(
                "ConsumerVulkanDevice: required extension {} not available on {device_name}",
                missing_required_extension.to_string_lossy()
            ))
        })?
        .into_iter()
        .map(CStr::as_ptr)
        .collect();

        // Optional extension for spec-correct cross-process layout
        // coordination. `VK_EXT_external_memory_acquire_unmodified`
        // lets the consumer-side acquire barrier declare the import is
        // unmodified so contents survive the transfer
        // (spec-permits-discard otherwise). When absent, the helpers
        // fall back to bridging UNDEFINED → target (content
        // preservation then relies on driver-empirical DMA-BUF kernel
        // semantics, which hold on NVIDIA Linux). The queue family
        // index `VK_QUEUE_FAMILY_EXTERNAL` used in the QFOT
        // release/acquire is core Vulkan 1.1 and doesn't need an
        // optional probe.
        let acquire_unmodified_ext = c"VK_EXT_external_memory_acquire_unmodified";
        let has_acquire_unmodified = available_device_ext_names.contains(&acquire_unmodified_ext);
        if has_acquire_unmodified {
            device_extensions.push(acquire_unmodified_ext.as_ptr());
        }

        #[cfg(target_os = "linux")]
        let video_codec_queue_family = match queues {
            ConsumerVulkanDeviceQueues::ImportOnly
            | ConsumerVulkanDeviceQueues::ExternalRenderer => None,
            ConsumerVulkanDeviceQueues::ImportAndVideoCodec(direction) => {
                let video_codec_queue_family_index = queue_families
                    .iter()
                    .position(|props| props.queue_flags.contains(direction.queue_flag()))
                    .map(|index| index as u32)
                    .ok_or_else(|| {
                        ConsumerRhiError::Gpu(format!(
                            "{device_name} has no queue family with {:?}, so it cannot {} with \
                             Vulkan Video",
                            direction.queue_flag(),
                            direction.verb()
                        ))
                    })?;
                for required_extension in direction.required_device_extensions() {
                    if !available_device_ext_names.contains(required_extension) {
                        return Err(ConsumerRhiError::Gpu(format!(
                            "{device_name} does not advertise {}, which Vulkan Video {} needs",
                            required_extension.to_string_lossy(),
                            direction.verb()
                        )));
                    }
                    device_extensions.push(required_extension.as_ptr());
                }
                let supports_h265 =
                    available_device_ext_names.contains(&direction.h265_device_extension());
                if supports_h265 {
                    device_extensions.push(direction.h265_device_extension().as_ptr());
                }
                let supports_video_maintenance1 =
                    available_device_ext_names.contains(&c"VK_KHR_video_maintenance1");
                if supports_video_maintenance1 {
                    device_extensions.push(c"VK_KHR_video_maintenance1".as_ptr());
                }
                Some((
                    direction,
                    video_codec_queue_family_index,
                    supports_h265,
                    supports_video_maintenance1,
                ))
            }
        };

        // A portability implementation that advertises the subset extension
        // requires it enabled (VUID-VkDeviceCreateInfo-pProperties-04451).
        let portability_subset_ext = c"VK_KHR_portability_subset";
        if available_device_ext_names.contains(&portability_subset_ext) {
            device_extensions.push(portability_subset_ext.as_ptr());
        }

        // Optional: without it a Metal shared event does not import, and the
        // helper's timeline edges fall back to host-side ordering rather than
        // the whole device refusing.
        #[cfg(target_os = "macos")]
        {
            let metal_objects_ext = c"VK_EXT_metal_objects";
            if available_device_ext_names.contains(&metal_objects_ext) {
                device_extensions.push(metal_objects_ext.as_ptr());
            }
        }

        let imported_host_pointer_alignment = REQUIRED_DEVICE_EXTENSIONS
            .contains(&c"VK_EXT_external_memory_host")
            .then(|| {
                let mut external_memory_host_properties =
                    vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::default();
                let mut properties2 = vk::PhysicalDeviceProperties2::builder()
                    .push_next(&mut external_memory_host_properties)
                    .build();
                unsafe {
                    instance.get_physical_device_properties2(physical_device, &mut properties2)
                };
                external_memory_host_properties.min_imported_host_pointer_alignment
            });

        // Logical device. Sync2 + timeline semaphore are core in 1.3 but
        // still need their feature flags enabled.
        let queue_priorities = [1.0f32];
        #[cfg_attr(
            not(target_os = "linux"),
            allow(unused_mut, reason = "only the Linux build mutates it")
        )]
        let mut queue_create_infos = vec![
            vk::DeviceQueueCreateInfo::builder()
                .queue_family_index(queue_family_index)
                .queue_priorities(&queue_priorities)
                .build(),
        ];
        #[cfg(target_os = "linux")]
        if let Some((_, video_codec_queue_family_index, _, _)) = video_codec_queue_family
            && video_codec_queue_family_index != queue_family_index
        {
            queue_create_infos.push(
                vk::DeviceQueueCreateInfo::builder()
                    .queue_family_index(video_codec_queue_family_index)
                    .queue_priorities(&queue_priorities)
                    .build(),
            );
        }

        let mut sync2_features = vk::PhysicalDeviceSynchronization2Features::builder()
            .synchronization2(true)
            .build();
        let mut timeline_features = vk::PhysicalDeviceTimelineSemaphoreFeatures::builder()
            .timeline_semaphore(true)
            .build();
        let mut dynamic_rendering_features = vk::PhysicalDeviceDynamicRenderingFeatures::builder()
            .dynamic_rendering(true)
            .build();
        // samplerYcbcrConversion: required to import NV12 textures on the
        // consumer side (multi-plane sampler-ycbcr support is core 1.1
        // but gated by the feature flag).
        let mut vulkan_1_1_features = vk::PhysicalDeviceVulkan11Features::builder()
            .sampler_ycbcr_conversion(true)
            .build();

        #[cfg(target_os = "linux")]
        let mut video_maintenance1_features =
            vk::PhysicalDeviceVideoMaintenance1FeaturesKHR::builder()
                .video_maintenance1(true)
                .build();

        #[cfg(target_os = "linux")]
        let enabled_physical_device_features = match queues {
            ConsumerVulkanDeviceQueues::ExternalRenderer => {
                physical_device_features_an_external_renderer_draws_with(&unsafe {
                    instance.get_physical_device_features(physical_device)
                })
            }
            _ => vk::PhysicalDeviceFeatures::default(),
        };
        #[cfg(not(target_os = "linux"))]
        let enabled_physical_device_features = vk::PhysicalDeviceFeatures::default();

        #[cfg_attr(
            not(target_os = "linux"),
            allow(unused_mut, reason = "only the Linux build mutates it")
        )]
        let mut device_create_info_builder = vk::DeviceCreateInfo::builder()
            .queue_create_infos(&queue_create_infos)
            .enabled_extension_names(&device_extensions)
            .enabled_features(&enabled_physical_device_features)
            .push_next(&mut sync2_features)
            .push_next(&mut timeline_features)
            .push_next(&mut dynamic_rendering_features)
            .push_next(&mut vulkan_1_1_features);
        #[cfg(target_os = "linux")]
        if let Some((_, _, _, true)) = video_codec_queue_family {
            device_create_info_builder =
                device_create_info_builder.push_next(&mut video_maintenance1_features);
        }
        let device_create_info = device_create_info_builder.build();

        let device = unsafe { instance.create_device(physical_device, &device_create_info, None) }
            .map_err(logical_device_creation_refusal)?;

        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };
        #[cfg(target_os = "linux")]
        let video_codec_queue = video_codec_queue_family.map(
            |(direction, video_codec_queue_family_index, supports_h265, _)| {
                ConsumerVulkanVideoCodecQueue {
                    direction,
                    queue: unsafe { device.get_device_queue(video_codec_queue_family_index, 0) },
                    queue_family_index: video_codec_queue_family_index,
                    supports_h265,
                }
            },
        );
        #[cfg(not(target_os = "linux"))]
        let video_codec_queue = {
            let ConsumerVulkanDeviceQueues::ImportOnly = queues;
            None
        };
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        tracing::info!(
            "ConsumerVulkanDevice initialized on the device the runtime named: {} (uuid={}, software={}, queue family {}, {} memory types, acquire_unmodified={}, dma_buf_import={}, drm_format_modifier_import={}, semaphore_fd_import={})",
            device_name,
            crate::lowercase_hex_of_vulkan_device_uuid(physical_device_uuid),
            device_is_software,
            queue_family_index,
            memory_properties.memory_type_count,
            has_acquire_unmodified,
            device_extension_is_enabled(&device, vk::EXT_EXTERNAL_MEMORY_DMA_BUF_EXTENSION),
            device_extension_is_enabled(&device, vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_EXTENSION),
            device_extension_is_enabled(&device, vk::KHR_EXTERNAL_SEMAPHORE_FD_EXTENSION),
        );

        Ok(Self {
            entry,
            instance: instance.keep(),
            physical_device,
            memory_properties,
            device,
            queue,
            queue_family_index,
            device_name,
            device_is_software,
            has_acquire_unmodified,
            physical_device_uuid,
            imported_host_pointer_alignment,
            live_allocation_count: AtomicUsize::new(0),
            video_codec_queue,
            queue_mutex: Mutex::new(()),
            #[cfg(target_os = "linux")]
            get_instance_proc_addr,
            #[cfg(target_os = "linux")]
            enabled_physical_device_features,
        })
    }

    /// A consumer device an embedded renderer draws on: the import queue, with
    /// the core features a vector renderer uses enabled wherever the device
    /// on `vulkan_device_uuid_the_runtime_named` supports them.
    #[cfg(target_os = "linux")]
    pub(crate) fn new_for_an_external_renderer_on_the_device_the_runtime_named(
        vulkan_device_uuid_the_runtime_named: [u8; 16],
    ) -> Result<Self> {
        Self::new_with_queues_on_the_device_the_runtime_named(
            vulkan_device_uuid_the_runtime_named,
            ConsumerVulkanDeviceQueues::ExternalRenderer,
        )
    }
}

/// The error a refused `vkCreateDevice` answers with: for now when the driver
/// lacked a resource another client of the GPU may free, such as memory or
/// the BAR1 aperture NVIDIA reports as `VK_ERROR_INITIALIZATION_FAILED` once
/// it is full.
pub(super) fn logical_device_creation_refusal(refusal: vk::ErrorCode) -> ConsumerRhiError {
    let message = format!("Failed to create consumer logical device: {refusal}");
    match refusal {
        vk::ErrorCode::INITIALIZATION_FAILED
        | vk::ErrorCode::OUT_OF_DEVICE_MEMORY
        | vk::ErrorCode::OUT_OF_HOST_MEMORY
        | vk::ErrorCode::TOO_MANY_OBJECTS => ConsumerRhiError::GpuOutOfResourcesForNow(message),
        _ => ConsumerRhiError::Gpu(message),
    }
}
