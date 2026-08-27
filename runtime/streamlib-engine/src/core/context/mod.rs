// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod audio_clock;
mod audio_device_backend;
pub(crate) mod escalate_gate;
mod gpu_context;
pub(crate) mod isolation;
mod runtime_context;
mod silent_null_audio_device_backend;
pub(crate) mod surface_check_out_lease_registry;
#[cfg(target_os = "linux")]
pub(crate) mod surface_export_staging;
#[cfg(target_os = "linux")]
pub(crate) mod surface_pixel_exchange;
pub(crate) mod surface_store;
pub mod texture_pool;
pub(crate) mod texture_registration;
mod texture_ring;
mod time_context;

pub use audio_clock::{
    AudioClock, AudioClockConfig, AudioTickCallback, AudioTickContext, SharedAudioClock,
    SoftwareAudioClock,
};
pub use audio_device_backend::{
    AudioCaptureSampleFormat, AudioCaptureStream, AudioCaptureStreamFormat,
    AudioCaptureStreamRequest, AudioDeviceBackend, CapturedAudioBlockFromDevice,
    CapturedAudioBlockHandOff, SharedAudioDeviceBackend, probe_audio_device_backend,
};
#[cfg(target_os = "linux")]
pub use gpu_context::GpuCapabilitiesSnapshot;
#[cfg(target_os = "linux")]
pub use gpu_context::{BatchedComputeKernelDispatch, BatchedComputeKernelDispatchBinding};
pub use gpu_context::{GpuContext, GpuContextFullAccess, GpuContextLimitedAccess};
pub(crate) use isolation::FullAccessGrant;
pub use isolation::IsolationTier;
pub use runtime_context::{RuntimeContext, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
pub use silent_null_audio_device_backend::SilentNullAudioDeviceBackend;
pub use surface_check_out_lease_registry::{
    SurfaceCheckOutLeaseHandOff, SurfaceCheckOutLeaseHolderId, SurfaceCheckOutLeaseRegistry,
};
#[cfg(target_os = "linux")]
pub use surface_export_staging::{SurfaceExportStaging, SurfaceExportStagingResidency};
pub use surface_store::SurfaceStore;
pub use texture_pool::*;
pub use texture_registration::TextureRegistration;
pub use texture_ring::{
    TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES, TextureRing, TextureRingInner, TextureRingSlot,
};
pub use time_context::TimeContext;
