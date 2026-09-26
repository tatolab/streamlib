// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod audio_clock;
mod audio_device_backend;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod captured_video_frame_to_pooled_rgba_conversion_stage;
mod device_backend_probe_chain;
mod device_stream_liveness_report;
pub(crate) mod escalate_gate;
mod gpu_context;
pub(crate) mod isolation;
mod refusing_null_video_codec_backend;
mod refusing_null_video_device_backend;
mod runtime_context;
pub(crate) mod silent_null_audio_device_backend;
pub(crate) mod surface_backing_resolution;
pub(crate) mod surface_check_out_lease_registry;
#[cfg(target_os = "linux")]
pub(crate) mod surface_export_staging;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod surface_pixel_exchange;
pub(crate) mod surface_share_wire_verbs;
pub(crate) mod surface_store;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod surface_to_surface_copy;
pub mod texture_pool;
pub(crate) mod texture_registration;
mod texture_ring;
mod time_context;
mod video_capture_instant_resolver;
mod video_codec_backend;
mod video_device_backend;

pub use audio_clock::{
    AudioClock, AudioClockConfig, AudioTickCallback, AudioTickContext, SharedAudioClock,
    SoftwareAudioClock,
};
pub use audio_device_backend::{
    AudioBlockForPlaybackHandOff, AudioBlockRequestedByDevice, AudioCaptureStream,
    AudioDeviceBackend, AudioDeviceStreamRequest, AudioPlaybackStream, AudioSampleFormat,
    AudioStreamFormat, CapturedAudioBlockFromDevice, CapturedAudioBlockHandOff,
    SharedAudioDeviceBackend, probe_audio_device_backend,
};
pub use device_backend_probe_chain::DeviceBackendArmUnavailableReason;
pub use device_stream_liveness_report::{
    DeviceStreamFailureReason, DeviceStreamFailureRecorder, DeviceStreamLivenessReport,
};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use gpu_context::GpuCapabilitiesSnapshot;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use gpu_context::{BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding};
pub use gpu_context::{GpuContext, GpuContextFullAccess, GpuContextLimitedAccess};
pub(crate) use isolation::FullAccessGrant;
pub use isolation::IsolationTier;
pub use runtime_context::{RuntimeContext, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
// Exported rather than crate-private so a test about deviceless pacing can open
// the arm it means. The chain's probe takes the first arm that opens, so a test
// that went through it would exercise whatever audio server the machine running
// it happens to have.
pub use silent_null_audio_device_backend::SilentNullAudioDeviceBackend;
pub use surface_check_out_lease_registry::{
    SurfaceCheckOutLeaseHandOff, SurfaceCheckOutLeaseHolderId, SurfaceCheckOutLeaseRegistry,
};
#[cfg(target_os = "linux")]
pub use surface_export_staging::{SurfaceExportStaging, SurfaceExportStagingResidency};
pub(crate) use surface_share_wire_verbs::SurfaceShareRegistrationsByRuntime;
pub use surface_store::SurfaceStore;
pub use texture_pool::*;
pub use texture_registration::TextureRegistration;
pub use texture_ring::{
    TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES, TextureRing, TextureRingInner, TextureRingSlot,
};
pub use time_context::TimeContext;
pub(crate) use video_capture_instant_resolver::{
    DeviceReportedCaptureStamp, VideoCaptureInstantResolver,
};
pub use video_codec_backend::{
    DecodedVideoPictureInPooledPixelBuffer, EncodedVideoAccessUnitFromSession,
    SharedVideoCodecBackend, VideoCodecBackend, VideoCodecElementaryStream,
    VideoDecodeMaximumCodedExtent, VideoDecodeSession, VideoDecodeSessionRequest, VideoEncodeKnobs,
    VideoEncodeSession, VideoEncodeSessionRequest, VideoEncodeSourceSurface,
    probe_video_codec_backend,
};
pub(crate) use video_device_backend::refusal_for_a_named_camera_that_is_not_attached;
pub use video_device_backend::{
    CapturedVideoFrameFromDevice, CapturedVideoFrameHandOff, SharedVideoDeviceBackend,
    VideoCaptureDevice, VideoCaptureStream, VideoCaptureStreamFormat, VideoDeviceBackend,
    VideoDeviceStreamRequest, probe_video_device_backend,
};
