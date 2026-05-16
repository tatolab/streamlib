// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/color` — GPU color-management primitives carved out of
//! the streamlib engine.
//!
//! Today this package hosts:
//! - [`RhiToneMapper`] / [`ToneMapperPushConstants`] / [`ToneCurveId`] —
//!   image→image tone-curve compute primitive (BT.2390 EETF +
//!   BT.2446-1 method A2 inverse).
//! - [`TransferId`] + closed-form transfer functions (PQ, sRGB) used
//!   by the tone-curve math and tests.
//! - CPU reference math for tone curves (test-time only — production
//!   uses the GPU compute kernel via `RhiToneMapper`).
//!
//! Engine purity precedent #794 (audio-converter retirement): color
//! science is domain knowledge, packaged separately from the
//! `libs/streamlib-engine` substrate. Consumers (compositors,
//! displays, encoders) take a Cargo dep on this package alongside
//! their engine dep.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

pub mod tone;
pub mod tone_mapper;
pub mod transfer;

#[cfg(target_os = "linux")]
pub mod vulkan_tone_mapper;

pub use tone::{bt2390_eetf_per_channel, bt2446a_inverse_per_channel};
pub use tone_mapper::{
    RhiToneMapper, ToneCurveId, ToneMapperPushConstants, TONE_MAPPER_PUSH_CONSTANT_SIZE,
};
pub use transfer::{
    linear_to_pq, linear_to_srgb, pq_to_linear, srgb_to_linear, TransferId,
};

#[cfg(target_os = "linux")]
pub use vulkan_tone_mapper::{VulkanToneMapper, TONE_MAPPER_WORKGROUP_SIZE};
