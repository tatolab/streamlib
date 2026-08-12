// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod audio_clock;
#[cfg(target_os = "linux")]
mod compute_kernel_bridge;
#[cfg(target_os = "linux")]
mod cpu_readback_bridge;
#[cfg(target_os = "linux")]
mod device_export_staging;
pub(crate) mod escalate_gate;
mod gpu_context;
#[cfg(target_os = "linux")]
mod graphics_kernel_bridge;
pub(crate) mod isolation;
#[cfg(target_os = "linux")]
mod ray_tracing_kernel_bridge;
mod runtime_context;
pub(crate) mod surface_store;
pub mod texture_pool;
pub(crate) mod texture_registration;
mod texture_ring;
mod time_context;

pub use audio_clock::{
    AudioClock, AudioClockConfig, AudioTickCallback, AudioTickContext, SharedAudioClock,
    SoftwareAudioClock,
};
#[cfg(target_os = "linux")]
pub use compute_kernel_bridge::ComputeKernelBridge;
#[cfg(target_os = "linux")]
pub use cpu_readback_bridge::{CpuReadbackBridge, CpuReadbackCopyDirection};
#[cfg(target_os = "linux")]
pub use device_export_staging::SurfaceDeviceExportStaging;
#[cfg(target_os = "linux")]
pub use gpu_context::GpuCapabilitiesSnapshot;
pub use gpu_context::{GpuContext, GpuContextFullAccess, GpuContextLimitedAccess};
#[cfg(target_os = "linux")]
pub use graphics_kernel_bridge::{
    BlendFactorWire, BlendOpWire, CullModeWire, DepthCompareOpWire, DepthFormatWire,
    DynamicStateWire, FrontFaceWire, GraphicsBindingDecl, GraphicsBindingKindWire,
    GraphicsBindingValue, GraphicsDrawSpec, GraphicsIndexBufferBinding, GraphicsKernelBridge,
    GraphicsKernelRegisterDecl, GraphicsKernelRunDraw, GraphicsPipelineStateWire,
    GraphicsVertexBufferBinding, IndexTypeWire, PolygonModeWire, PrimitiveTopologyWire,
    ScissorRectWire, VertexAttributeFormatWire, VertexInputAttributeDecl, VertexInputBindingDecl,
    VertexInputRateWire, ViewportWire,
};
pub(crate) use isolation::FullAccessGrant;
pub use isolation::IsolationTier;
#[cfg(target_os = "linux")]
pub use ray_tracing_kernel_bridge::{
    BlasRegisterDecl, RAY_TRACING_STAGE_INDEX_NONE, RayTracingBindingDecl,
    RayTracingBindingKindWire, RayTracingBindingValue, RayTracingKernelBridge,
    RayTracingKernelRegisterDecl, RayTracingKernelRunDispatch, RayTracingShaderGroupWire,
    RayTracingShaderStageWire, RayTracingStageDecl, TlasInstanceDeclWire, TlasRegisterDecl,
};
pub use runtime_context::{RuntimeContext, RuntimeContextFullAccess, RuntimeContextLimitedAccess};
pub use surface_store::SurfaceStore;
pub use texture_pool::*;
pub use texture_registration::TextureRegistration;
pub use texture_ring::{
    TEXTURE_RING_SLOT_SURFACE_ID_MAX_BYTES, TextureRing, TextureRingInner, TextureRingSlot,
};
pub use time_context::TimeContext;
