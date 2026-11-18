// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

pub(crate) mod capture_and_playback_devices;
mod clocks;
pub(crate) mod escalate_gate;
mod gpu_context;
pub(crate) mod isolation;
mod runtime_context;
mod store_scoped_to_this_node;
pub(crate) mod surfaces_and_their_stores;
pub(crate) mod texture_pools_and_rings;
pub use texture_pools_and_rings::texture_pool;

pub use capture_and_playback_devices::audio_device_backend::{
    AudioBlockForPlaybackHandOff, AudioBlockRequestedByDevice, AudioCaptureStream,
    AudioDeviceBackend, AudioDeviceStreamRequest, AudioPlaybackStream, AudioSampleFormat,
    AudioStreamFormat, CapturedAudioBlockFromDevice, CapturedAudioBlockHandOff,
    RequestedAudioStreamFormat, SharedAudioDeviceBackend, open_capture_stream_in_requested_format,
    open_playback_stream_in_requested_format, probe_audio_device_backend,
};
pub use capture_and_playback_devices::capture_device_access_prompting::{
    CAPTURE_DEVICE_ACCESS_PROMPTING_ENVIRONMENT_VARIABLE, CaptureDeviceAccessPrompting,
};
pub use capture_and_playback_devices::device_backend_probe_chain::DeviceBackendArmUnavailableReason;
pub use capture_and_playback_devices::device_stream_liveness_report::{
    DeviceStreamFailureReason, DeviceStreamFailureRecorder, DeviceStreamLivenessReport,
};
pub use clocks::audio_clock::{
    AudioClock, AudioClockConfig, AudioTickCallback, AudioTickContext, SharedAudioClock,
    SoftwareAudioClock, platform_audio_clock_for_one_stream,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use gpu_context::GpuCapabilitiesSnapshot;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use gpu_context::StorageBufferAllocationFlavour;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use gpu_context::{
    BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding,
    GraphicsKernelHeldForOneDraw, RayTracingKernelHeldForOneTrace,
    SurfaceBoundKernelBindingResource,
};
pub use gpu_context::{
    GpuContext, GpuContextFullAccess, GpuContextLimitedAccess, OutputTextureRegisteredUntilDropped,
};
#[cfg(target_os = "linux")]
pub use gpu_context::{SurfaceExportStaging, SurfaceExportStagingResidency};
pub(crate) use isolation::FullAccessGrant;
pub use isolation::IsolationTier;
pub(crate) use runtime_context::{
    LoadedStreamARuntimeContextBelongsTo, StoreOfOneNodeOpenedOnFirstUse,
};
pub use runtime_context::{RuntimeContext, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
pub(crate) use store_scoped_to_this_node::{
    LIVE_INPUT_VALUES_TABLE, forget_the_value_the_node_keeps_under, key_of_the_node,
    the_node_a_key_leads_with, the_value_the_node_keeps_under,
};
pub use store_scoped_to_this_node::{ProjectStoresOpenInThisRuntime, StoreScopedToThisNode};
pub use texture_pools_and_rings::node_output_pixel_buffer_pool_of_one_native_node::{
    NativeNodeOutputPixelBufferFrame, NodeOutputPixelBufferPoolOfOneNativeNode,
};
pub use texture_pools_and_rings::one_off_acquisitions_counted_for_one_native_node::OneOffAcquisitionCountedForItsNode;
pub(crate) use texture_pools_and_rings::one_off_acquisitions_counted_for_one_native_node::{
    OneOffAcquisitionsCountedForOneNativeNode, acquire_a_one_off_counted_against_its_node,
};
pub(crate) use texture_pools_and_rings::one_off_acquisitions_held_for_one_node::{
    OneOffAcquisitionShape, OneOffAcquisitionsHeldForOneNode,
};
// Exported rather than crate-private so a test about deviceless pacing can open
// the arm it means. The chain's probe takes the first arm that opens, so a test
// that went through it would exercise whatever audio server the machine running
// it happens to have.
pub use capture_and_playback_devices::silent_null_audio_device_backend::SilentNullAudioDeviceBackend;
pub(crate) use capture_and_playback_devices::video_device_backend::refusal_for_a_named_camera_that_is_not_attached;
pub use capture_and_playback_devices::video_device_backend::{
    CapturedVideoFrameFromDevice, CapturedVideoFrameHandOff, SharedVideoDeviceBackend,
    VideoCaptureDevice, VideoCaptureStream, VideoCaptureStreamFormat, VideoDeviceBackend,
    VideoDeviceStreamRequest, probe_video_device_backend,
};
pub(crate) use clocks::stream_clock::stream_time_now_ns_on;
pub use clocks::stream_clock::{
    DEFAULT_SCALED_STREAM_CLOCK_STEP_NS, StreamClock, StreamClockReading, StreamClockSpec,
    machine_time_ns_a_published_clock_reaches, stream_clock_reading_of_a_published_clock,
    stream_time_ns_on_a_published_clock,
};
pub(crate) use clocks::video_capture_instant_resolver::{
    DeviceReportedCaptureStamp, VideoCaptureInstantResolver,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use streamlib_ipc_types::GpuPoolClaimTableSharedRows;
pub use streamlib_ipc_types::{
    GpuPoolClaimTableRow, GpuPoolClaimantColumn, GpuPoolFrameRecycled,
    GpuPoolSlotFrameDescriptorBody, GpuPoolSlotRehandTest,
};
pub use surfaces_and_their_stores::surface_check_out_lease_registry::{
    SurfaceCheckOutLeaseHandOff, SurfaceCheckOutLeaseHolderId, SurfaceCheckOutLeaseRegistry,
};
pub use surfaces_and_their_stores::surface_claimed_against_producer_reuse::SurfaceClaimedAgainstProducerReuse;
pub(crate) use surfaces_and_their_stores::surface_share_wire_verbs::SurfaceShareRegistrationsByRuntime;
pub use surfaces_and_their_stores::surface_store::SurfaceStore;
pub(crate) use texture_pools_and_rings::gpu_pool_claim_table::gpu_pool_claim_table_row_for_a_shared_slot;
pub use texture_pools_and_rings::gpu_pool_claim_table::{
    GpuPoolClaimTable, GpuPoolClaimTableRowLease, GpuPoolClaimTables, GpuPoolClaimTablesOfOnePool,
    GpuPoolClaimantProcessId,
};
pub use texture_pools_and_rings::texture_pool::{
    PoolSlotId, PooledTextureHandle, TextureCrossProcessImportability, TexturePool,
    TexturePoolDescriptor, TexturePoolKey, TexturePoolStats,
};
pub use texture_pools_and_rings::texture_registration::TextureRegistration;
pub use texture_pools_and_rings::texture_ring::{
    TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES, TextureRing, TextureRingInner, TextureRingSlot,
};
