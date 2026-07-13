// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generic Vulkan query-pool primitive.
//!
//! `HostVulkanQueryPool` wraps a single `VkQueryPool` parameterized by
//! `VkQueryType` plus the per-type pNext chain (pipeline-statistics
//! flags, video-encode-feedback flags + profile). One wrapper services
//! every query class — timestamp, occlusion, pipeline-statistics,
//! video-encode-feedback — per the engine-model rule "design for the
//! class of use case, not the example."

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use crate::core::{Error, Result};

use super::HostVulkanDevice;

/// Inputs for [`HostVulkanQueryPool::new`].
///
/// `query_type` selects the underlying Vulkan query class. Three
/// optional fields each apply to a single query type and are ignored
/// for the others; checked at construction so a misconfigured
/// descriptor fails fast rather than producing a query pool whose
/// results don't decode correctly.
pub struct QueryPoolDescriptor<'a> {
    pub label: &'a str,
    pub query_type: vk::QueryType,
    pub query_count: u32,
    /// Required when `query_type == PIPELINE_STATISTICS`; ignored
    /// otherwise.
    pub pipeline_statistics: vk::QueryPipelineStatisticFlags,
    /// Required when `query_type == VIDEO_ENCODE_FEEDBACK_KHR` — the
    /// feedback flags the codec wants to measure (offset / bytes
    /// written / has overrides). Ignored otherwise.
    pub video_encode_feedback_flags: vk::VideoEncodeFeedbackFlagsKHR,
    /// Required when `query_type == VIDEO_ENCODE_FEEDBACK_KHR` — the
    /// codec profile the queries serve. Vulkan validation requires
    /// both the encode-feedback create info AND the profile chained
    /// on the pool's `pNext`. Ignored otherwise.
    pub video_profile: Option<&'a vk::VideoProfileInfoKHR>,
}

/// Snapshot of the per-pool encode-feedback flags, kept on the
/// wrapper so [`HostVulkanQueryPool::fetch_video_encode_feedback`]
/// can decode the per-query result without the caller re-passing
/// what was already declared at construction.
#[derive(Clone, Copy, Debug, Default)]
struct EncodeFeedbackLayout {
    has_buffer_offset: bool,
    has_bytes_written: bool,
    has_overrides: bool,
}

impl EncodeFeedbackLayout {
    fn from_flags(flags: vk::VideoEncodeFeedbackFlagsKHR) -> Self {
        Self {
            has_buffer_offset: flags
                .contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BUFFER_OFFSET),
            has_bytes_written: flags
                .contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN),
            has_overrides: flags.contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_HAS_OVERRIDES),
        }
    }

    /// One u32 per requested bit, in declaration order per Vulkan
    /// spec for `VK_QUERY_TYPE_VIDEO_ENCODE_FEEDBACK_KHR`.
    fn slot_count(&self) -> usize {
        (self.has_buffer_offset as usize)
            + (self.has_bytes_written as usize)
            + (self.has_overrides as usize)
    }
}

/// Typed result of a single video-encode-feedback query. Each field
/// is `Some(value)` if the corresponding bit was declared in
/// [`QueryPoolDescriptor::video_encode_feedback_flags`] at pool
/// construction; `None` otherwise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoEncodeFeedbackResult {
    /// Byte offset into the bitstream buffer where the encoded
    /// frame begins. From `BITSTREAM_BUFFER_OFFSET_BIT_KHR`.
    pub bitstream_buffer_offset: Option<u32>,
    /// Number of encoded bytes written. From
    /// `BITSTREAM_BYTES_WRITTEN_BIT_KHR`.
    pub bitstream_bytes_written: Option<u32>,
    /// Whether the encoder overrode any rate-control / quality
    /// parameters for this frame. From `HAS_OVERRIDES_BIT_KHR`.
    pub has_overrides: Option<bool>,
}

impl VideoEncodeFeedbackResult {
    /// Decode the raw u32 slots returned by `vkGetQueryPoolResults`
    /// into typed fields, using the layout the pool was constructed
    /// with. Public for unit testing — production callers go through
    /// [`HostVulkanQueryPool::fetch_video_encode_feedback`].
    fn decode(slots: &[u32], layout: EncodeFeedbackLayout) -> Self {
        let mut next = 0usize;
        let take = |next: &mut usize| -> Option<u32> {
            let v = slots.get(*next).copied();
            *next += 1;
            v
        };
        let bitstream_buffer_offset = if layout.has_buffer_offset {
            take(&mut next)
        } else {
            None
        };
        let bitstream_bytes_written = if layout.has_bytes_written {
            take(&mut next)
        } else {
            None
        };
        let has_overrides = if layout.has_overrides {
            take(&mut next).map(|v| v != 0)
        } else {
            None
        };
        Self {
            bitstream_buffer_offset,
            bitstream_bytes_written,
            has_overrides,
        }
    }
}

/// Privileged RHI handle for a `VkQueryPool`. Mirrors the shape of
/// [`super::HostVulkanVideoSession`] and the new
/// [`super::HostVulkanTexture::new_video_dpb`] /
/// [`super::HostVulkanBuffer::new_video_bitstream`] constructors:
/// an `Arc<HostVulkanDevice>` back-reference, the raw Vulkan
/// handle, and a `Drop` impl that tears it down.
pub struct HostVulkanQueryPool {
    #[allow(dead_code)] // surfaced via tracing on construction
    label: String,
    vulkan_device: Arc<HostVulkanDevice>,
    handle: vk::QueryPool,
    query_type: vk::QueryType,
    query_count: u32,
    encode_feedback_layout: EncodeFeedbackLayout,
}

unsafe impl Send for HostVulkanQueryPool {}
unsafe impl Sync for HostVulkanQueryPool {}

impl HostVulkanQueryPool {
    /// Build a new query pool. Runs `vkCreateQueryPool` under the
    /// host's device-level resource lock so concurrent processor
    /// submissions can't race the create on NVIDIA Linux — same
    /// threading discipline the other privileged RHI primitives
    /// inherit (`HostVulkanVideoSession`, `HostVulkanTexture::new_video_dpb`,
    /// `HostVulkanBuffer::new_video_bitstream`).
    ///
    /// The descriptor's per-query-type optional fields are checked
    /// for consistency: `PIPELINE_STATISTICS` requires non-empty
    /// `pipeline_statistics`; `VIDEO_ENCODE_FEEDBACK_KHR` requires
    /// non-empty `video_encode_feedback_flags` AND a non-`None`
    /// `video_profile`. Mismatches surface as a clean
    /// [`Error::Configuration`] before any Vulkan API call.
    #[tracing::instrument(level = "trace", skip(vulkan_device, descriptor), fields(label = descriptor.label, query_type = ?descriptor.query_type, query_count = descriptor.query_count))]
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        descriptor: &QueryPoolDescriptor<'_>,
    ) -> Result<Self> {
        if descriptor.query_count == 0 {
            return Err(Error::Configuration(format!(
                "HostVulkanQueryPool::new ({}): query_count must be > 0",
                descriptor.label,
            )));
        }

        let mut layout = EncodeFeedbackLayout::default();
        let mut create_info = vk::QueryPoolCreateInfo::builder()
            .query_type(descriptor.query_type)
            .query_count(descriptor.query_count);

        match descriptor.query_type {
            vk::QueryType::PIPELINE_STATISTICS => {
                if descriptor.pipeline_statistics.is_empty() {
                    return Err(Error::Configuration(format!(
                        "HostVulkanQueryPool::new ({}): PIPELINE_STATISTICS requires \
                         non-empty pipeline_statistics flags",
                        descriptor.label,
                    )));
                }
                create_info = create_info.pipeline_statistics(descriptor.pipeline_statistics);
            }
            vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR => {
                if descriptor.video_encode_feedback_flags.is_empty() {
                    return Err(Error::Configuration(format!(
                        "HostVulkanQueryPool::new ({}): VIDEO_ENCODE_FEEDBACK_KHR \
                         requires non-empty video_encode_feedback_flags",
                        descriptor.label,
                    )));
                }
                if descriptor.video_profile.is_none() {
                    return Err(Error::Configuration(format!(
                        "HostVulkanQueryPool::new ({}): VIDEO_ENCODE_FEEDBACK_KHR \
                         requires a video_profile (chained as pNext)",
                        descriptor.label,
                    )));
                }
                layout = EncodeFeedbackLayout::from_flags(descriptor.video_encode_feedback_flags);
            }
            _ => {}
        }

        // The per-type pNext extensions live on this function's
        // stack for the duration of the create call. Vulkan spec
        // for `VK_QUERY_TYPE_VIDEO_ENCODE_FEEDBACK_KHR` requires both
        // `VkQueryPoolVideoEncodeFeedbackCreateInfoKHR` AND the
        // `VkVideoProfileInfoKHR` itself chained onto the pool's
        // `pNext` (the profile is chained directly, NOT through a
        // `VkVideoProfileListInfoKHR` wrapper as for images / buffers).
        let mut feedback_create_info = vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::builder()
            .encode_feedback_flags(descriptor.video_encode_feedback_flags)
            .build();
        // Copy the profile by value so `push_next` can take `&mut`.
        // The copy preserves `p_next` (and any codec-specific
        // extension structs the caller chained onto the profile),
        // which remain valid via the descriptor's `'a` lifetime for
        // the duration of this function call.
        let mut profile_for_chain: Option<vk::VideoProfileInfoKHR> =
            descriptor.video_profile.copied();
        if descriptor.query_type == vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR {
            create_info = create_info.push_next(&mut feedback_create_info);
            if let Some(stored) = profile_for_chain.as_mut() {
                create_info = create_info.push_next(stored);
            }
        }

        let device = vulkan_device.device();

        let _device_lock = vulkan_device.lock_device();
        let handle = unsafe { device.create_query_pool(&create_info, None) }.map_err(|e| {
            Error::GpuError(format!(
                "HostVulkanQueryPool::new ({}): vkCreateQueryPool failed: {e}",
                descriptor.label,
            ))
        })?;

        Ok(Self {
            label: descriptor.label.to_string(),
            vulkan_device: Arc::clone(vulkan_device),
            handle,
            query_type: descriptor.query_type,
            query_count: descriptor.query_count,
            encode_feedback_layout: layout,
        })
    }

    /// Raw `VkQueryPool` handle. Caller may pass it into
    /// `vkCmdBeginQuery` / `vkCmdEndQuery` / `vkCmdResetQueryPool`
    /// but must NOT destroy it — the `Drop` impl owns destruction.
    #[inline]
    pub fn handle(&self) -> vk::QueryPool {
        self.handle
    }

    /// Number of query slots the pool was created with.
    #[inline]
    pub fn query_count(&self) -> u32 {
        self.query_count
    }

    /// Underlying `VkQueryType`.
    #[inline]
    pub fn query_type(&self) -> vk::QueryType {
        self.query_type
    }

    /// Fetch a single video-encode-feedback query's results into a
    /// typed [`VideoEncodeFeedbackResult`]. Blocks until the GPU
    /// signals the query (`vk::QueryResultFlags::WAIT`).
    ///
    /// Errors when `query_type != VIDEO_ENCODE_FEEDBACK_KHR` or
    /// `query_index >= query_count`. The result's `Some(...)` fields
    /// correspond to the flags requested at construction; un-requested
    /// fields are `None`.
    pub fn fetch_video_encode_feedback(
        &self,
        query_index: u32,
    ) -> Result<VideoEncodeFeedbackResult> {
        if self.query_type != vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR {
            return Err(Error::Configuration(format!(
                "HostVulkanQueryPool::fetch_video_encode_feedback ({}): \
                 query_type is {:?}, expected VIDEO_ENCODE_FEEDBACK_KHR",
                self.label, self.query_type,
            )));
        }
        if query_index >= self.query_count {
            return Err(Error::Configuration(format!(
                "HostVulkanQueryPool::fetch_video_encode_feedback ({}): \
                 query_index {query_index} >= query_count {}",
                self.label, self.query_count,
            )));
        }

        let slot_count = self.encode_feedback_layout.slot_count();
        if slot_count == 0 {
            return Ok(VideoEncodeFeedbackResult::default());
        }

        let mut slots = vec![0u32; slot_count];
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                slots.as_mut_ptr() as *mut u8,
                slot_count * std::mem::size_of::<u32>(),
            )
        };

        let device = self.vulkan_device.device();
        unsafe {
            device.get_query_pool_results(
                self.handle,
                query_index,
                1,
                bytes,
                (slot_count * std::mem::size_of::<u32>()) as u64,
                vk::QueryResultFlags::WAIT,
            )
        }
        .map_err(|e| {
            Error::GpuError(format!(
                "HostVulkanQueryPool::fetch_video_encode_feedback ({}): \
                 vkGetQueryPoolResults failed: {e}",
                self.label,
            ))
        })?;

        Ok(VideoEncodeFeedbackResult::decode(
            &slots,
            self.encode_feedback_layout,
        ))
    }
}

impl Drop for HostVulkanQueryPool {
    fn drop(&mut self) {
        if self.handle != vk::QueryPool::null() {
            let device = self.vulkan_device.device();
            let _device_lock = self.vulkan_device.lock_device();
            unsafe { device.destroy_query_pool(self.handle, None) };
            self.handle = vk::QueryPool::null();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure-logic decode test. Each layout permutation maps the input
    /// u32 array to the correct typed fields. Mentally reverting the
    /// `if layout.has_*` checks in [`VideoEncodeFeedbackResult::decode`]
    /// would shift slot indices and break these assertions.
    #[test]
    fn decode_buffer_offset_and_bytes_written() {
        let layout = EncodeFeedbackLayout {
            has_buffer_offset: true,
            has_bytes_written: true,
            has_overrides: false,
        };
        let result = VideoEncodeFeedbackResult::decode(&[123, 456], layout);
        assert_eq!(result.bitstream_buffer_offset, Some(123));
        assert_eq!(result.bitstream_bytes_written, Some(456));
        assert_eq!(result.has_overrides, None);
    }

    #[test]
    fn decode_only_bytes_written() {
        let layout = EncodeFeedbackLayout {
            has_buffer_offset: false,
            has_bytes_written: true,
            has_overrides: false,
        };
        let result = VideoEncodeFeedbackResult::decode(&[789], layout);
        assert_eq!(result.bitstream_buffer_offset, None);
        assert_eq!(result.bitstream_bytes_written, Some(789));
        assert_eq!(result.has_overrides, None);
    }

    #[test]
    fn decode_all_three_bits() {
        let layout = EncodeFeedbackLayout {
            has_buffer_offset: true,
            has_bytes_written: true,
            has_overrides: true,
        };
        let result = VideoEncodeFeedbackResult::decode(&[100, 200, 1], layout);
        assert_eq!(result.bitstream_buffer_offset, Some(100));
        assert_eq!(result.bitstream_bytes_written, Some(200));
        assert_eq!(result.has_overrides, Some(true));
    }

    #[test]
    fn decode_has_overrides_false() {
        let layout = EncodeFeedbackLayout {
            has_buffer_offset: false,
            has_bytes_written: false,
            has_overrides: true,
        };
        let result = VideoEncodeFeedbackResult::decode(&[0], layout);
        assert_eq!(result.has_overrides, Some(false));
    }

    #[test]
    fn slot_count_matches_bits_set() {
        assert_eq!(EncodeFeedbackLayout::default().slot_count(), 0);
        assert_eq!(
            EncodeFeedbackLayout {
                has_buffer_offset: true,
                has_bytes_written: false,
                has_overrides: false,
            }
            .slot_count(),
            1
        );
        assert_eq!(
            EncodeFeedbackLayout {
                has_buffer_offset: true,
                has_bytes_written: true,
                has_overrides: true,
            }
            .slot_count(),
            3
        );
    }

    /// `EncodeFeedbackLayout::from_flags` round-trips the
    /// `VkVideoEncodeFeedbackFlagsKHR` bits into the typed snapshot.
    /// Catches future spec drift if the vulkanalia bitflag values change.
    #[test]
    fn from_flags_round_trip() {
        let layout = EncodeFeedbackLayout::from_flags(
            vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BUFFER_OFFSET
                | vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN,
        );
        assert!(layout.has_buffer_offset);
        assert!(layout.has_bytes_written);
        assert!(!layout.has_overrides);
    }

    /// `new_query_pool` rejects `query_count = 0` with a Configuration
    /// error before reaching `vkCreateQueryPool`. Hardware-gated because
    /// constructing the descriptor requires a `HostVulkanDevice`.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn new_rejects_zero_query_count() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        let descriptor = QueryPoolDescriptor {
            label: "test/zero-count",
            query_type: vk::QueryType::TIMESTAMP,
            query_count: 0,
            pipeline_statistics: vk::QueryPipelineStatisticFlags::empty(),
            video_encode_feedback_flags: vk::VideoEncodeFeedbackFlagsKHR::empty(),
            video_profile: None,
        };
        match HostVulkanQueryPool::new(&device, &descriptor) {
            Err(Error::Configuration(msg)) => {
                assert!(msg.contains("query_count must be > 0"));
                assert!(msg.contains("test/zero-count"));
            }
            Err(e) => panic!("expected Configuration error, got {e}"),
            Ok(_) => panic!("query_count=0 must be rejected, got Ok"),
        }
    }

    /// `PIPELINE_STATISTICS` rejects an empty `pipeline_statistics`
    /// bitmask — the Vulkan spec requires at least one stat bit,
    /// and an empty mask produces a pool whose results are
    /// silently empty.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn new_rejects_pipeline_stats_empty_flags() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        let descriptor = QueryPoolDescriptor {
            label: "test/empty-stats",
            query_type: vk::QueryType::PIPELINE_STATISTICS,
            query_count: 4,
            pipeline_statistics: vk::QueryPipelineStatisticFlags::empty(),
            video_encode_feedback_flags: vk::VideoEncodeFeedbackFlagsKHR::empty(),
            video_profile: None,
        };
        match HostVulkanQueryPool::new(&device, &descriptor) {
            Err(Error::Configuration(msg)) => {
                assert!(msg.contains("PIPELINE_STATISTICS"));
                assert!(msg.contains("test/empty-stats"));
            }
            Err(e) => panic!("expected Configuration error, got {e}"),
            Ok(_) => panic!("empty pipeline_statistics must be rejected, got Ok"),
        }
    }

    /// `VIDEO_ENCODE_FEEDBACK_KHR` rejects an empty feedback-flag
    /// mask AND missing video profile.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn new_rejects_encode_feedback_missing_fields() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        let profile = vk::VideoProfileInfoKHR::default();

        let no_flags = QueryPoolDescriptor {
            label: "test/no-feedback-flags",
            query_type: vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR,
            query_count: 1,
            pipeline_statistics: vk::QueryPipelineStatisticFlags::empty(),
            video_encode_feedback_flags: vk::VideoEncodeFeedbackFlagsKHR::empty(),
            video_profile: Some(&profile),
        };
        match HostVulkanQueryPool::new(&device, &no_flags) {
            Err(Error::Configuration(msg)) => {
                assert!(msg.contains("video_encode_feedback_flags"));
            }
            Err(e) => panic!("expected Configuration error, got {e}"),
            Ok(_) => panic!("empty feedback flags must be rejected, got Ok"),
        }

        let no_profile = QueryPoolDescriptor {
            label: "test/no-profile",
            query_type: vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR,
            query_count: 1,
            pipeline_statistics: vk::QueryPipelineStatisticFlags::empty(),
            video_encode_feedback_flags: vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BUFFER_OFFSET,
            video_profile: None,
        };
        match HostVulkanQueryPool::new(&device, &no_profile) {
            Err(Error::Configuration(msg)) => {
                assert!(msg.contains("video_profile"));
            }
            Err(e) => panic!("expected Configuration error, got {e}"),
            Ok(_) => panic!("missing video_profile must be rejected, got Ok"),
        }
    }

    /// Positive: `TIMESTAMP` query pool with a small count constructs
    /// cleanly against a real device. Locks the "non-video query types
    /// don't need the video-specific pNext" path.
    #[cfg(target_os = "linux")]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    #[test]
    fn new_timestamp_query_pool_succeeds() {
        let device = match HostVulkanDevice::new() {
            Ok(d) => d,
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                return;
            }
        };
        let descriptor = QueryPoolDescriptor {
            label: "test/timestamp",
            query_type: vk::QueryType::TIMESTAMP,
            query_count: 8,
            pipeline_statistics: vk::QueryPipelineStatisticFlags::empty(),
            video_encode_feedback_flags: vk::VideoEncodeFeedbackFlagsKHR::empty(),
            video_profile: None,
        };
        let pool = HostVulkanQueryPool::new(&device, &descriptor)
            .expect("timestamp query pool construction must succeed");
        assert_ne!(pool.handle(), vk::QueryPool::null());
        assert_eq!(pool.query_count(), 8);
        assert_eq!(pool.query_type(), vk::QueryType::TIMESTAMP);
    }
}
