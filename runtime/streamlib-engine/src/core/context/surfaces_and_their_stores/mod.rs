// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Published surfaces: the store that holds them, their check-out leases, how they are shared,
//! resolved, converted and copied.

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod rgba_or_bgra_image_frame_to_pooled_rgba_conversion_stage;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod rgba_or_bgra_image_frame_to_pooled_rgba_conversion_stage_tests;
pub(crate) mod surface_backing_resolution;
pub(crate) mod surface_check_out_lease_registry;
pub(super) mod surface_claimed_against_producer_reuse;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod surface_color_conversion;
pub(crate) mod surface_share_wire_verbs;
pub(crate) mod surface_store;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod surface_to_surface_copy;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod texture_layouts_published_to_the_surface_store;
