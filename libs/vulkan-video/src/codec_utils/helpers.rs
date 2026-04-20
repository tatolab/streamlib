// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of `common/libs/VkCodecUtils/Helpers.h` and `Helpers.cpp`.
//!
//! The C++ original uses a `VkInterfaceFunctions` dispatch table for all Vulkan
//! calls. In the Rust port we accept `vulkanalia::Instance` and `vulkanalia::Device` directly,
//! which carry their own function-pointer tables loaded at creation time.

use std::fmt;
use std::os::unix::io::RawFd;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrSurfaceExtensionInstanceCommands;
use vulkanalia::vk::KhrSwapchainExtensionDeviceCommands;

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

/// Vertex with 2D position and 2D texture coordinate.
/// Port of `vk::Vertex` in Helpers.h.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Vertex {
    pub position: [f32; 2],
    pub tex_coord: [f32; 2],
}

/// Two-component float vector.
/// Port of `vk::Vec2` in Helpers.h.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Vec2 {
    pub val: [f32; 2],
}

impl Vec2 {
    pub fn new(val0: f32, val1: f32) -> Self {
        Self { val: [val0, val1] }
    }
}

/// Four-component float vector.
/// Port of `vk::Vec4` in Helpers.h.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct Vec4 {
    pub val: [f32; 4],
}

impl Vec4 {
    pub fn new(val0: f32, val1: f32, val2: f32, val3: f32) -> Self {
        Self {
            val: [val0, val1, val2, val3],
        }
    }
}

/// Push-constant block carrying a 4×4 position matrix and a 2×2 texture matrix.
/// Port of `vk::TransformPushConstants` in Helpers.h.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TransformPushConstants {
    pub pos_matrix: [Vec4; 4],
    pub tex_matrix: [Vec2; 2],
}

impl Default for TransformPushConstants {
    fn default() -> Self {
        Self {
            pos_matrix: [
                Vec4::new(1.0, 0.0, 0.0, 0.0),
                Vec4::new(0.0, 1.0, 0.0, 0.0),
                Vec4::new(0.0, 0.0, 1.0, 0.0),
                Vec4::new(0.0, 0.0, 0.0, 1.0),
            ],
            tex_matrix: [Vec2::new(1.0, 0.0), Vec2::new(0.0, 1.0)],
        }
    }
}

// ---------------------------------------------------------------------------
// aligned_size — generic alignment helper
// ---------------------------------------------------------------------------

/// Round `value` up to the next multiple of `alignment`.
///
/// Port of the C++ template `alignedSize<valueType, alignmentType>`.
/// Both types must support the basic integer arithmetic and bitwise ops.
pub fn aligned_size<T>(value: T, alignment: T) -> T
where
    T: Copy
        + std::ops::Add<Output = T>
        + std::ops::Sub<Output = T>
        + std::ops::BitAnd<Output = T>
        + std::ops::Not<Output = T>
        + From<u8>,
{
    let one = T::from(1u8);
    (value + alignment - one) & !(alignment - one)
}

// ---------------------------------------------------------------------------
// NativeHandle — wraps a Unix file descriptor for external memory
// ---------------------------------------------------------------------------

/// Wraps a Unix file descriptor used for Vulkan external memory import/export.
///
/// Port of `vk::NativeHandle` (Linux/Unix path only — Android
/// `AHardwareBuffer` path is omitted).
///
/// On `Drop` the fd is closed unless `disown()` has been called.
pub struct NativeHandle {
    fd: RawFd,
    external_memory_handle_type: vk::ExternalMemoryHandleTypeFlags,
}

impl NativeHandle {
    /// Sentinel value representing an invalid handle.
    pub const INVALID: Self = Self {
        fd: -1,
        external_memory_handle_type: vk::ExternalMemoryHandleTypeFlags::empty(),
    };

    /// Create an invalid (empty) handle.
    pub fn new() -> Self {
        Self {
            fd: -1,
            external_memory_handle_type: vk::ExternalMemoryHandleTypeFlags::empty(),
        }
    }

    /// Wrap an existing file descriptor as an opaque-fd external memory handle.
    pub fn from_fd(fd: RawFd) -> Self {
        Self {
            fd,
            external_memory_handle_type: vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
        }
    }

    /// Return the file descriptor.
    ///
    /// # Panics
    /// Panics if the handle type is not `OPAQUE_FD`.
    pub fn get_fd(&self) -> RawFd {
        assert!(
            self.external_memory_handle_type == vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD,
            "NativeHandle::get_fd called on non-fd handle"
        );
        self.fd
    }

    /// Assign a new file descriptor, releasing the previous one if valid.
    pub fn set_fd(&mut self, fd: RawFd) {
        self.release_reference();
        self.fd = fd;
        self.external_memory_handle_type = vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;
    }

    pub fn external_memory_handle_type(&self) -> vk::ExternalMemoryHandleTypeFlags {
        self.external_memory_handle_type
    }

    /// Relinquish ownership of the underlying resource without closing it.
    pub fn disown(&mut self) {
        self.fd = -1;
        self.external_memory_handle_type = vk::ExternalMemoryHandleTypeFlags::empty();
    }

    /// Returns `true` when the handle wraps a valid resource.
    pub fn is_valid(&self) -> bool {
        self.external_memory_handle_type == vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD
            && self.fd >= 0
    }

    /// Close the underlying fd (if owned) and reset to invalid state.
    pub fn release_reference(&mut self) {
        if self.external_memory_handle_type == vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD
            && self.fd >= 0
        {
            // SAFETY: we own this fd and are done with it.
            unsafe {
                libc_close(self.fd);
            }
        }
        self.disown();
    }
}

impl Default for NativeHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NativeHandle {
    fn drop(&mut self) {
        self.release_reference();
    }
}

/// Thin wrapper around the POSIX `close(2)` syscall so we don't need a `libc`
/// crate dependency.
///
/// # Safety
/// `fd` must be a valid open file descriptor that the caller owns.
unsafe fn libc_close(fd: RawFd) {
    // `close` is provided by libc which is always linked on Unix targets.
    unsafe extern "C" {
        fn close(fd: std::os::raw::c_int) -> std::os::raw::c_int;
    }
    unsafe { let _ = close(fd); }
}

// ---------------------------------------------------------------------------
// pNext chain helper
// ---------------------------------------------------------------------------

/// Insert `chained` at the head of `node`'s pNext chain.
///
/// Port of the C++ template `ChainNextVkStruct`. In vulkanalia every Vulkan
/// structure that participates in pNext chains has `s_type` and `next`/`p_next`
/// as its first two fields. This helper operates on the raw pointers.
///
/// # Safety
/// Both pointers must be valid and point to Vulkan structures whose first two
/// fields are `s_type` and `next` (i.e. they extend `VkBaseInStructure`).
/// `chained.next` must be null on entry.
pub unsafe fn chain_next_vk_struct<N, C>(node: &mut N, chained: &mut C) {
    let node_base = node as *mut N as *mut vk::BaseOutStructure;
    let chained_base = chained as *mut C as *mut vk::BaseOutStructure;

    debug_assert!(
        (*chained_base).next.is_null(),
        "chained node must have null next on entry"
    );

    // Insert at the head: chained.next = node.next; node.next = chained
    (*chained_base).next = (*node_base).next;
    (*node_base).next = chained_base;
}

// ---------------------------------------------------------------------------
// Vulkan enumeration / query helpers
// ---------------------------------------------------------------------------

/// Enumerate instance extension properties for an optional `layer`.
///
/// Port of inline `vk::enumerate(vkIf, layer, exts)`.
pub fn enumerate_instance_extension_properties(
    entry: &vulkanalia::Entry,
    layer: Option<&std::ffi::CStr>,
) -> Result<Vec<vk::ExtensionProperties>, vk::ErrorCode> {
    unsafe {
        entry.enumerate_instance_extension_properties(layer)
    }
}

/// Enumerate device extension properties.
///
/// Port of inline `vk::enumerate(vkIf, phy, layer, exts)`.
pub fn enumerate_device_extension_properties(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> Result<Vec<vk::ExtensionProperties>, vk::ErrorCode> {
    unsafe {
        instance.enumerate_device_extension_properties(physical_device, None)
    }
}

/// Enumerate physical devices.
///
/// Port of inline `vk::enumerate(vkIf, instance, phys)`.
pub fn enumerate_physical_devices(
    instance: &vulkanalia::Instance,
) -> Result<Vec<vk::PhysicalDevice>, vk::ErrorCode> {
    unsafe { instance.enumerate_physical_devices() }
}

/// Enumerate instance layer properties.
///
/// Port of inline `vk::enumerate(vkIf, layer_props)`.
pub fn enumerate_instance_layer_properties(
    entry: &vulkanalia::Entry,
) -> Result<Vec<vk::LayerProperties>, vk::ErrorCode> {
    unsafe { entry.enumerate_instance_layer_properties() }
}

/// Query result for [`get_physical_device_queue_family_properties2`].
pub struct QueueFamilyInfo {
    pub properties: vk::QueueFamilyProperties2,
    pub video_properties: vk::QueueFamilyVideoPropertiesKHR,
    pub query_result_status: vk::QueueFamilyQueryResultStatusPropertiesKHR,
}

/// Retrieve queue-family properties with video and query-result-status
/// extensions chained.
///
/// Port of inline `vk::get(vkIf, phy, queues, videoQueues, queryResultStatus)`.
pub unsafe fn get_physical_device_queue_family_properties2(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
) -> Vec<QueueFamilyInfo> {
    // Use the raw function pointer because vulkanalia's high-level wrapper
    // doesn't support pNext chains on the output structures.
    let get_props_fn = instance.commands().get_physical_device_queue_family_properties2;

    // First call to get the count.
    let mut count: u32 = 0;
    get_props_fn(physical_device, &mut count, std::ptr::null_mut());

    // Allocate and chain structures.
    let mut infos: Vec<QueueFamilyInfo> = (0..count)
        .map(|_| QueueFamilyInfo {
            properties: vk::QueueFamilyProperties2::default(),
            video_properties: vk::QueueFamilyVideoPropertiesKHR::default(),
            query_result_status: vk::QueueFamilyQueryResultStatusPropertiesKHR::default(),
        })
        .collect();

    // Build the mutable slice of QueueFamilyProperties2 with pNext chains.
    let mut props: Vec<vk::QueueFamilyProperties2> = infos
        .iter_mut()
        .map(|info| {
            info.video_properties.next =
                &mut info.query_result_status as *mut _ as *mut std::ffi::c_void;
            let mut p = vk::QueueFamilyProperties2::default();
            p.next = &mut info.video_properties as *mut _ as *mut std::ffi::c_void;
            p
        })
        .collect();

    get_props_fn(physical_device, &mut count, props.as_mut_ptr());

    // Copy results back into infos.
    for (info, prop) in infos.iter_mut().zip(props.iter()) {
        info.properties.queue_family_properties = prop.queue_family_properties;
    }

    infos
}

/// Get surface formats.
///
/// Port of inline `vk::get(vkIf, phy, surface, formats)`.
pub unsafe fn get_physical_device_surface_formats(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> Result<Vec<vk::SurfaceFormatKHR>, vk::ErrorCode> {
    instance.get_physical_device_surface_formats_khr(physical_device, surface)
}

/// Get present modes.
///
/// Port of inline `vk::get(vkIf, phy, surface, modes)`.
pub unsafe fn get_physical_device_surface_present_modes(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> Result<Vec<vk::PresentModeKHR>, vk::ErrorCode> {
    instance.get_physical_device_surface_present_modes_khr(physical_device, surface)
}

/// Get swapchain images.
///
/// Port of inline `vk::get(vkIf, dev, swapchain, images)`.
pub unsafe fn get_swapchain_images(
    device: &vulkanalia::Device,
    swapchain: vk::SwapchainKHR,
) -> Result<Vec<vk::Image>, vk::ErrorCode> {
    device.get_swapchain_images_khr(swapchain)
}

// ---------------------------------------------------------------------------
// Memory type mapping
// ---------------------------------------------------------------------------

/// Find the first memory type index that matches `type_bits` and
/// `requirements_mask`.
///
/// Port of inline `vk::MapMemoryTypeToIndex`.
pub unsafe fn map_memory_type_to_index(
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    type_bits: u32,
    requirements_mask: vk::MemoryPropertyFlags,
) -> Result<u32, vk::ErrorCode> {
    let memory_properties = instance.get_physical_device_memory_properties(physical_device);
    let mut bits = type_bits;
    for i in 0..32u32 {
        if (bits & 1) == 1
            && memory_properties.memory_types[i as usize]
                .property_flags
                .contains(requirements_mask)
        {
            return Ok(i);
        }
        bits >>= 1;
    }
    Err(vk::ErrorCode::VALIDATION_FAILED)
}

// ---------------------------------------------------------------------------
// Fence helpers
// ---------------------------------------------------------------------------

/// Default per-iteration fence wait timeout: 100 ms in nanoseconds.
pub const DEFAULT_FENCE_WAIT_TIMEOUT: u64 = 100 * 1_000 * 1_000;
/// Default total fence wait timeout: 5 s in nanoseconds.
pub const DEFAULT_FENCE_TOTAL_WAIT_TIMEOUT: u64 = 5 * 1_000 * 1_000 * 1_000;

/// Wait for `fence` to be signaled and optionally reset it.
///
/// Port of inline `vk::WaitAndResetFence`.
pub unsafe fn wait_and_reset_fence(
    device: &vulkanalia::Device,
    fence: vk::Fence,
    reset_after_wait: bool,
    fence_name: &str,
    fence_wait_timeout: u64,
    fence_total_wait_timeout: u64,
) -> vk::Result {
    assert!(fence != vk::Fence::null());

    let mut current_wait: u64 = 0;
    let mut result = vk::Result::SUCCESS;

    while fence_total_wait_timeout >= current_wait {
        current_wait += fence_wait_timeout;

        match device.wait_for_fences(&[fence], true, fence_wait_timeout) {
            Ok(vk::SuccessCode::SUCCESS) => {
                result = vk::Result::SUCCESS;
                break;
            }
            Ok(vk::SuccessCode::TIMEOUT) => {
                tracing::warn!(
                    "fence {}({:?}) is not done after {} ms",
                    fence_name,
                    fence,
                    current_wait / (1_000 * 1_000),
                );
                result = vk::Result::TIMEOUT;
            }
            Ok(_) => {
                result = vk::Result::SUCCESS;
                break;
            }
            Err(e) => {
                result = vk::Result::from(e);
                break;
            }
        }
    }

    if result != vk::Result::SUCCESS {
        let status = device.get_fence_status(fence);
        tracing::error!(
            "fence {}({:?}) is not done after {} ms — status: {:?}",
            fence_name,
            fence,
            fence_total_wait_timeout / (1_000 * 1_000),
            status,
        );
        panic!("Fence is not signaled yet after total wait timeout");
    }

    if reset_after_wait {
        if let Err(e) = device.reset_fences(&[fence]) {
            tracing::error!("ResetFences() result: {:?}", e);
            panic!("ResetFences failed: {:?}", e);
        }

        debug_assert!(
            matches!(device.get_fence_status(fence), Ok(vk::SuccessCode::NOT_READY)),
            "Fence should be NOT_READY after reset"
        );
    }

    result
}

/// Wait for `fence`, check the video decode query-pool status, and retry on
/// timeout.
///
/// Port of inline `vk::WaitAndGetStatus`.
pub unsafe fn wait_and_get_status(
    device: &vulkanalia::Device,
    fence: vk::Fence,
    query_pool: vk::QueryPool,
    start_query_id: i32,
    picture_index: u32,
    reset_after_wait: bool,
    fence_name: &str,
    fence_wait_timeout: u64,
    fence_total_wait_timeout: u64,
    mut retry_count: u32,
) -> vk::Result {
    let mut result;

    loop {
        result = wait_and_reset_fence(
            device,
            fence,
            reset_after_wait,
            fence_name,
            fence_wait_timeout,
            fence_total_wait_timeout,
        );

        if result != vk::Result::SUCCESS {
            tracing::warn!(
                "WaitForFences timeout {} result {:?} retry {}",
                fence_wait_timeout,
                result,
                retry_count,
            );

            let mut decode_status_raw: i32 = vk::QueryResultStatusKHR::NOT_READY.as_raw();
            let data_slice = std::slice::from_raw_parts_mut(
                &mut decode_status_raw as *mut i32 as *mut u8,
                std::mem::size_of::<i32>(),
            );
            let query_result = device.get_query_pool_results(
                query_pool,
                start_query_id as u32,
                1, // query_count
                data_slice,
                std::mem::size_of::<i32>() as vk::DeviceSize,
                vk::QueryResultFlags::WITH_STATUS_KHR,
            );
            let decode_status = vk::QueryResultStatusKHR::from_raw(decode_status_raw);

            tracing::error!("GetQueryPoolResults() result: {:?}", query_result);
            tracing::error!(
                "Decode status for CurrPicIdx {}: {:?}",
                picture_index,
                decode_status,
            );

            if let Err(vk::ErrorCode::DEVICE_LOST) = query_result {
                tracing::error!("Dropping frame");
                break;
            }

            if query_result.is_ok()
                && decode_status == vk::QueryResultStatusKHR::ERROR
            {
                tracing::error!("Decoding of the frame failed.");
                break;
            }
        }

        retry_count -= 1;
        if result != vk::Result::TIMEOUT || retry_count == 0 {
            break;
        }
    }

    result
}

// ---------------------------------------------------------------------------
// DeviceUuidUtils
// ---------------------------------------------------------------------------

/// UUID size constant matching `VK_UUID_SIZE`.
pub const VK_UUID_SIZE: usize = vk::UUID_SIZE;

/// Utility for storing, parsing, formatting, and comparing Vulkan device UUIDs.
///
/// Port of `vk::DeviceUuidUtils` in Helpers.h.
#[derive(Clone)]
pub struct DeviceUuidUtils {
    device_uuid: [u8; VK_UUID_SIZE],
    is_valid: bool,
}

impl DeviceUuidUtils {
    /// Create an invalid (zeroed) UUID.
    pub fn new() -> Self {
        Self {
            device_uuid: [0u8; VK_UUID_SIZE],
            is_valid: false,
        }
    }

    /// Create from a raw byte array.
    pub fn from_bytes(uuid: &[u8; VK_UUID_SIZE]) -> Self {
        Self {
            device_uuid: *uuid,
            is_valid: true,
        }
    }

    /// Parse a UUID from the standard 8-4-4-4-12 hex string format
    /// (e.g. `"550e8400-e29b-41d4-a716-446655440000"`).
    ///
    /// Returns the number of hex digit pairs (bytes) successfully parsed.
    /// Port of `DeviceUuidUtils::StringToUUID`.
    pub fn string_to_uuid(&mut self, uuid_str: &str) -> usize {
        let mut num_hex_digits: usize = 0;

        if uuid_str.len() != 36 {
            tracing::error!("UUID string must be 36 characters long");
            return num_hex_digits;
        }

        let bytes = uuid_str.as_bytes();
        let mut i = 0usize;

        while i < 36 {
            if bytes[i] == b'-' {
                i += 1;
                continue;
            }

            if i + 1 >= bytes.len() {
                tracing::error!("Invalid hex character in UUID");
                return num_hex_digits;
            }

            let hi = hex_digit(bytes[i]);
            let lo = hex_digit(bytes[i + 1]);

            match (hi, lo) {
                (Some(h), Some(l)) => {
                    self.device_uuid[num_hex_digits] = (h << 4) | l;
                    i += 2;
                    num_hex_digits += 1;
                    if num_hex_digits == VK_UUID_SIZE {
                        self.is_valid = true;
                        break;
                    }
                }
                _ => {
                    tracing::error!("Invalid hex character in UUID");
                    return num_hex_digits;
                }
            }
        }

        num_hex_digits
    }

    /// Return the raw UUID bytes, or `None` if invalid.
    pub fn get_device_uuid(&self) -> Option<&[u8; VK_UUID_SIZE]> {
        if self.is_valid {
            Some(&self.device_uuid)
        } else {
            None
        }
    }

    /// Format as the standard 8-4-4-4-12 hex string.
    /// Port of `DeviceUuidUtils::ToString`.
    pub fn to_string(&self) -> String {
        let mut s = String::with_capacity(36);
        for (i, byte) in self.device_uuid.iter().enumerate() {
            if i == 4 || i == 6 || i == 8 || i == 10 {
                s.push('-');
            }
            s.push_str(&format!("{:02x}", byte));
        }
        s
    }

    /// Return `true` when the stored UUID is valid.
    pub fn is_valid(&self) -> bool {
        self.is_valid
    }

    /// Compare against a raw UUID byte array.
    /// Port of `DeviceUuidUtils::Compare`.
    pub fn compare(&self, device_uuid: &[u8; VK_UUID_SIZE]) -> bool {
        self.is_valid && self.device_uuid == *device_uuid
    }
}

impl Default for DeviceUuidUtils {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DeviceUuidUtils {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.to_string())
    }
}

impl fmt::Debug for DeviceUuidUtils {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("DeviceUuidUtils")
            .field("uuid", &self.to_string())
            .field("is_valid", &self.is_valid)
            .finish()
    }
}

/// Convert a single ASCII hex digit to its numeric value.
fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- aligned_size ---------------------------------------------------------

    #[test]
    fn test_aligned_size_already_aligned() {
        assert_eq!(aligned_size(256u64, 256u64), 256);
    }

    #[test]
    fn test_aligned_size_needs_rounding() {
        assert_eq!(aligned_size(1u64, 256u64), 256);
        assert_eq!(aligned_size(257u64, 256u64), 512);
    }

    #[test]
    fn test_aligned_size_alignment_one() {
        assert_eq!(aligned_size(42u32, 1u32), 42);
    }

    #[test]
    fn test_aligned_size_power_of_two() {
        assert_eq!(aligned_size(5u32, 4u32), 8);
        assert_eq!(aligned_size(8u32, 4u32), 8);
        assert_eq!(aligned_size(9u32, 4u32), 12);
    }

    // -- DeviceUuidUtils ------------------------------------------------------

    #[test]
    fn test_uuid_default_is_invalid() {
        let uuid = DeviceUuidUtils::new();
        assert!(!uuid.is_valid());
        assert!(uuid.get_device_uuid().is_none());
    }

    #[test]
    fn test_uuid_from_bytes() {
        let bytes: [u8; VK_UUID_SIZE] = [
            0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ];
        let uuid = DeviceUuidUtils::from_bytes(&bytes);
        assert!(uuid.is_valid());
        assert_eq!(uuid.get_device_uuid(), Some(&bytes));
    }

    #[test]
    fn test_uuid_string_to_uuid_and_back() {
        let input = "550e8400-e29b-41d4-a716-446655440000";
        let mut uuid = DeviceUuidUtils::new();
        let parsed = uuid.string_to_uuid(input);
        assert_eq!(parsed, VK_UUID_SIZE);
        assert!(uuid.is_valid());
        assert_eq!(uuid.to_string(), input);
    }

    #[test]
    fn test_uuid_string_to_uuid_uppercase() {
        let input = "550E8400-E29B-41D4-A716-446655440000";
        let mut uuid = DeviceUuidUtils::new();
        let parsed = uuid.string_to_uuid(input);
        assert_eq!(parsed, VK_UUID_SIZE);
        assert!(uuid.is_valid());
        // to_string outputs lowercase
        assert_eq!(uuid.to_string(), input.to_lowercase());
    }

    #[test]
    fn test_uuid_string_to_uuid_wrong_length() {
        let mut uuid = DeviceUuidUtils::new();
        let parsed = uuid.string_to_uuid("too-short");
        assert_eq!(parsed, 0);
        assert!(!uuid.is_valid());
    }

    #[test]
    fn test_uuid_compare() {
        let bytes: [u8; VK_UUID_SIZE] = [
            0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ];
        let uuid = DeviceUuidUtils::from_bytes(&bytes);
        assert!(uuid.compare(&bytes));

        let mut different = bytes;
        different[0] = 0xFF;
        assert!(!uuid.compare(&different));
    }

    #[test]
    fn test_uuid_compare_invalid() {
        let uuid = DeviceUuidUtils::new();
        let bytes = [0u8; VK_UUID_SIZE];
        assert!(!uuid.compare(&bytes));
    }

    #[test]
    fn test_uuid_to_string_format() {
        let bytes: [u8; VK_UUID_SIZE] = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let uuid = DeviceUuidUtils::from_bytes(&bytes);
        assert_eq!(uuid.to_string(), "00112233-4455-6677-8899-aabbccddeeff");
    }

    // -- NativeHandle ---------------------------------------------------------

    #[test]
    fn test_native_handle_default_invalid() {
        let handle = NativeHandle::new();
        assert!(!handle.is_valid());
    }

    #[test]
    fn test_native_handle_disown() {
        let mut handle = NativeHandle::from_fd(999);
        assert!(handle.is_valid());
        handle.disown();
        assert!(!handle.is_valid());
    }

    // -- Vertex / Vec2 / Vec4 / TransformPushConstants layout -----------------

    #[test]
    fn test_vertex_default() {
        let v = Vertex::default();
        assert_eq!(v.position, [0.0, 0.0]);
        assert_eq!(v.tex_coord, [0.0, 0.0]);
    }

    #[test]
    fn test_transform_push_constants_identity() {
        let t = TransformPushConstants::default();
        // Diagonal should be 1.0
        for i in 0..4 {
            assert_eq!(t.pos_matrix[i].val[i], 1.0);
        }
        assert_eq!(t.tex_matrix[0].val, [1.0, 0.0]);
        assert_eq!(t.tex_matrix[1].val, [0.0, 1.0]);
    }

    // -- hex_digit ------------------------------------------------------------

    #[test]
    fn test_hex_digit() {
        assert_eq!(hex_digit(b'0'), Some(0));
        assert_eq!(hex_digit(b'9'), Some(9));
        assert_eq!(hex_digit(b'a'), Some(10));
        assert_eq!(hex_digit(b'f'), Some(15));
        assert_eq!(hex_digit(b'A'), Some(10));
        assert_eq!(hex_digit(b'F'), Some(15));
        assert_eq!(hex_digit(b'g'), None);
        assert_eq!(hex_digit(b' '), None);
    }
}
