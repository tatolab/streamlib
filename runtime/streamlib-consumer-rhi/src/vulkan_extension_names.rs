// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Borrowing extension names out of an enumerated `VkExtensionProperties` buffer.
//!
//! Both Vulkan device bring-up paths — the host `HostVulkanDevice` and the
//! carve-out `ConsumerVulkanDevice` — enumerate device extensions and then test
//! the results by name. Lives in consumer-rhi so the two share one spelling
//! rather than each re-deriving the borrow.

use std::ffi::CStr;

use vulkanalia::vk;

/// Borrows every extension name out of an enumerated properties buffer.
///
/// Each `vk::ExtensionProperties` stores its name as an inline
/// `StringArray`, so the returned `&CStr`s point into
/// `available_extension_properties` itself. Taking the buffer by reference ties
/// that borrow to the caller's owner: a caller that passes a temporary, or lets
/// the owner fall out of scope first, fails to compile.
///
/// The trap this exists to close is `CStr::from_ptr`, whose *unbounded* return
/// lifetime silences the borrow checker entirely and let two bring-up paths
/// read freed memory (#1846). `StringArray::as_cstr` borrows from `&self`, so
/// the enforcement is the type system rather than reviewer attention.
pub fn vulkan_extension_names_borrowed_from_properties(
    available_extension_properties: &[vk::ExtensionProperties],
) -> Vec<&CStr> {
    available_extension_properties
        .iter()
        .map(|extension_properties| extension_properties.extension_name.as_cstr())
        .collect()
}

/// Compile-time proof that the borrow is tied to the properties buffer.
///
/// The negative cases below are the exact shapes that shipped as
/// use-after-free in #1846: an owner that is a statement temporary, and an
/// owner confined to a block that closes before the names are read. Both must
/// stay `compile_fail`. If a future change reintroduces an unbounded lifetime
/// (a bare `CStr::from_ptr`, or a signature taking the buffer by value), these
/// start compiling and the suite fails.
///
/// The owner is a statement temporary — `ConsumerVulkanDevice::new`'s shape.
///
/// ```compile_fail,E0716
/// use streamlib_consumer_rhi::vulkan_extension_names_borrowed_from_properties;
/// use vulkanalia::vk;
///
/// let names = vulkan_extension_names_borrowed_from_properties(&vec![
///     vk::ExtensionProperties::default(),
/// ]);
/// assert_eq!(names.len(), 1);
/// ```
///
/// The owner is confined to a block — `HostVulkanDevice::new`'s shape.
///
/// ```compile_fail,E0597
/// use streamlib_consumer_rhi::vulkan_extension_names_borrowed_from_properties;
/// use vulkanalia::vk;
///
/// let names = {
///     let available = vec![vk::ExtensionProperties::default()];
///     vulkan_extension_names_borrowed_from_properties(&available)
/// };
/// assert_eq!(names.len(), 1);
/// ```
///
/// Positive control — the owner outlives the borrow, so this compiles.
///
/// ```
/// use streamlib_consumer_rhi::vulkan_extension_names_borrowed_from_properties;
/// use vulkanalia::vk;
///
/// let available = vec![vk::ExtensionProperties::default()];
/// let names = vulkan_extension_names_borrowed_from_properties(&available);
/// assert_eq!(names.len(), 1);
/// ```
#[doc(hidden)]
pub mod __extension_name_borrow_doctests {}

#[cfg(test)]
mod tests {
    use super::*;

    fn extension_properties_named(name: &[u8]) -> vk::ExtensionProperties {
        vk::ExtensionProperties {
            extension_name: vk::StringArray::from_bytes(name),
            spec_version: 1,
        }
    }

    #[test]
    fn borrows_each_name_in_order() {
        let available = vec![
            extension_properties_named(b"VK_KHR_external_memory"),
            extension_properties_named(b"VK_KHR_external_memory_fd"),
            extension_properties_named(b"VK_EXT_external_memory_dma_buf"),
        ];

        let names = vulkan_extension_names_borrowed_from_properties(&available);

        assert_eq!(
            names,
            vec![
                c"VK_KHR_external_memory",
                c"VK_KHR_external_memory_fd",
                c"VK_EXT_external_memory_dma_buf",
            ]
        );
    }

    #[test]
    fn contains_distinguishes_a_prefix_from_a_full_name() {
        let available = vec![extension_properties_named(b"VK_KHR_external_memory_fd")];

        let names = vulkan_extension_names_borrowed_from_properties(&available);

        assert!(names.contains(&c"VK_KHR_external_memory_fd"));
        assert!(!names.contains(&c"VK_KHR_external_memory"));
    }

    #[test]
    fn empty_properties_yield_no_names() {
        let available: Vec<vk::ExtensionProperties> = Vec::new();

        assert!(vulkan_extension_names_borrowed_from_properties(&available).is_empty());
    }

    /// A driver that fills the array to its last byte leaves no room for a
    /// terminator; reading must stop at the array bound rather than run into
    /// the next element.
    #[test]
    fn a_name_filling_the_array_stops_at_the_bound() {
        let longest = vec![b'x'; vk::MAX_EXTENSION_NAME_SIZE];
        let available = vec![
            extension_properties_named(&longest),
            extension_properties_named(b"VK_KHR_external_memory"),
        ];

        let names = vulkan_extension_names_borrowed_from_properties(&available);

        assert_eq!(names[0].to_bytes().len(), vk::MAX_EXTENSION_NAME_SIZE - 1);
        assert_eq!(names[1], c"VK_KHR_external_memory");
    }

    /// The names must stay readable after the borrow is handed across a scope
    /// the owner outlives — the property the two #1846 sites violated.
    #[test]
    fn names_stay_readable_while_the_owner_lives() {
        let available = vec![extension_properties_named(
            b"VK_EXT_image_drm_format_modifier",
        )];

        let names = vulkan_extension_names_borrowed_from_properties(&available);
        let observed: Vec<String> = names
            .iter()
            .map(|name| name.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            observed,
            vec!["VK_EXT_image_drm_format_modifier".to_string()]
        );
    }
}
