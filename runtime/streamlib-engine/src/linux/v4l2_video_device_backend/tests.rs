// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::context::{DeviceReportedCaptureStamp, GpuCapabilitiesSnapshot};

use super::v4l2_capture_dequeue_loop::v4l2_buffer_capture_stamp;
use super::v4l2_capture_gpu_resources::{
    V4l2CaptureBufferPathToTheGpu, choose_v4l2_capture_buffer_path_to_the_gpu,
};
use super::*;

#[test]
fn a_buffer_flagged_monotonic_carries_its_stamp_in_nanoseconds() {
    let flags = v4l::buffer::Flags::TIMESTAMP_MONOTONIC.bits()
        | v4l::buffer::Flags::DONE.bits()
        | v4l::buffer::Flags::MAPPED.bits();
    assert_eq!(
        v4l2_buffer_capture_stamp(flags, 541_560, 123_456),
        DeviceReportedCaptureStamp::OnTheMachineMonotonicClock {
            capture_timestamp_ns: 541_560_123_456_000
        }
    );
}

#[test]
fn a_buffer_whose_timestamp_type_is_unknown_or_copied_is_off_the_monotonic_clock() {
    for timestamp_type in [
        v4l::buffer::Flags::TIMESTAMP_UNKNOWN,
        v4l::buffer::Flags::TIMESTAMP_COPY,
    ] {
        assert_eq!(
            v4l2_buffer_capture_stamp(timestamp_type.bits(), 541_560, 123_456),
            DeviceReportedCaptureStamp::OffTheMachineMonotonicClock,
            "{timestamp_type:?}"
        );
    }
}

fn capabilities_of_a_hardware_device_that_exports_dma_buf() -> GpuCapabilitiesSnapshot {
    GpuCapabilitiesSnapshot {
        device_name: "AMD Radeon RX 7900 XTX (RADV NAVI31)".to_string(),
        supports_external_memory: true,
        device_is_software: false,
        supports_dma_buf_export: true,
        supports_semaphore_export: true,
        supports_cross_device_dma_buf_probe: true,
        supports_ray_tracing_pipeline: true,
    }
}

#[test]
fn a_hardware_device_that_exports_dma_buf_probes_the_import_of_a_physical_camera() {
    assert_eq!(
        choose_v4l2_capture_buffer_path_to_the_gpu(
            &capabilities_of_a_hardware_device_that_exports_dma_buf(),
            false
        ),
        V4l2CaptureBufferPathToTheGpu::ProbeTheDmaBufImportOfEveryExportedDriverBuffer
    );
}

/// The software device advertises the DMA-BUF extensions where
/// `/dev/udmabuf` is reachable, and still exports no DMA-BUF by rule.
#[test]
fn a_software_device_takes_mmap_because_it_exports_no_dma_buf() {
    let lavapipe = GpuCapabilitiesSnapshot {
        device_name: "llvmpipe (LLVM 20.1.2, 256 bits)".to_string(),
        device_is_software: true,
        supports_dma_buf_export: false,
        supports_semaphore_export: false,
        ..capabilities_of_a_hardware_device_that_exports_dma_buf()
    };
    for capture_device_is_virtual in [false, true] {
        assert_eq!(
            choose_v4l2_capture_buffer_path_to_the_gpu(&lavapipe, capture_device_is_virtual),
            V4l2CaptureBufferPathToTheGpu::MmapBecauseTheVulkanDeviceExportsNoDmaBuf,
            "capture device virtual: {capture_device_is_virtual}"
        );
    }
}

#[test]
fn a_virtual_capture_device_takes_mmap_on_a_device_that_exports_dma_buf() {
    assert_eq!(
        choose_v4l2_capture_buffer_path_to_the_gpu(
            &capabilities_of_a_hardware_device_that_exports_dma_buf(),
            true
        ),
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheCaptureDeviceIsVirtual
    );
}

#[test]
fn a_driver_blocklisted_for_cross_device_imports_takes_mmap() {
    let nvidia = GpuCapabilitiesSnapshot {
        device_name: "NVIDIA GeForce RTX 3090".to_string(),
        supports_cross_device_dma_buf_probe: false,
        ..capabilities_of_a_hardware_device_that_exports_dma_buf()
    };
    assert_eq!(
        choose_v4l2_capture_buffer_path_to_the_gpu(&nvidia, false),
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheVulkanDriverIsBlocklistedForCrossDeviceImports
    );
}

#[test]
fn a_device_without_external_memory_takes_mmap() {
    let without_external_memory = GpuCapabilitiesSnapshot {
        supports_external_memory: false,
        supports_dma_buf_export: false,
        ..capabilities_of_a_hardware_device_that_exports_dma_buf()
    };
    assert_eq!(
        choose_v4l2_capture_buffer_path_to_the_gpu(&without_external_memory, false),
        V4l2CaptureBufferPathToTheGpu::MmapBecauseTheVulkanDeviceHasNoExternalMemory
    );
}

#[test]
fn list_devices_succeeds_with_or_without_cameras() {
    let devices = list_v4l2_capture_devices().expect("enumeration must not error");
    for device in &devices {
        assert!(device.id.starts_with("/dev/video"), "{}", device.id);
    }
}

/// A camera asked for by a name that does not exist is refused naming
/// it, never replaced by whatever camera happens to be attached.
#[test]
fn a_named_device_that_does_not_exist_is_refused_naming_it() {
    let refusal = match open_requested_v4l2_device(Some("/dev/video-streamlib-absent")) {
        Ok(_) => panic!("a device node that does not exist cannot open"),
        Err(refusal) => refusal.to_string(),
    };
    assert!(
        refusal.contains("'/dev/video-streamlib-absent' does not exist"),
        "{refusal}"
    );
}
