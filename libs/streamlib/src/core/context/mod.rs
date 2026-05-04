// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod audio_clock;
#[cfg(target_os = "linux")]
mod compute_kernel_bridge;
#[cfg(target_os = "linux")]
mod cpu_readback_bridge;
mod gpu_context;
#[cfg(target_os = "linux")]
mod graphics_kernel_bridge;
mod runtime_context;
mod surface_store;
pub mod texture_pool;
mod texture_registration;
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
pub use graphics_kernel_bridge::{
    BlendFactorWire, BlendOpWire, CullModeWire, DepthCompareOpWire, DepthFormatWire,
    DynamicStateWire, FrontFaceWire, GraphicsBindingDecl, GraphicsBindingKindWire,
    GraphicsBindingValue, GraphicsDrawSpec, GraphicsIndexBufferBinding, GraphicsKernelBridge,
    GraphicsKernelRegisterDecl, GraphicsKernelRunDraw, GraphicsPipelineStateWire,
    GraphicsVertexBufferBinding, IndexTypeWire, PolygonModeWire, PrimitiveTopologyWire,
    ScissorRectWire, VertexAttributeFormatWire, VertexInputAttributeDecl,
    VertexInputBindingDecl, VertexInputRateWire, ViewportWire,
};
pub use gpu_context::{GpuContext, GpuContextFullAccess, GpuContextLimitedAccess};
pub use runtime_context::{
    RuntimeContext, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
pub use surface_store::SurfaceStore;
pub use texture_pool::*;
pub use texture_registration::TextureRegistration;
pub use time_context::TimeContext;
