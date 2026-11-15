// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

#[cfg(target_os = "linux")]
use super::registration_payloads::*;
#[cfg(target_os = "linux")]
use super::unix_socket_surface_share_replies::*;
use super::*;
#[cfg(target_os = "linux")]
use crate::core::context::surfaces_and_their_stores::surface_share_wire_verbs::{
    SURFACE_RESOURCE_TYPE_PIXEL_BUFFER, SURFACE_WIRE_FIELD_EXPORTING_DEVICE_UUID,
    stated_vk_memory_type_index,
};

#[cfg(target_os = "linux")]
mod dma_buf_registration_payload_tests;
#[cfg(target_os = "linux")]
mod external_handle_flavour_tests;
#[cfg(target_os = "linux")]
mod fd_ownership_tests;
#[cfg(target_pointer_width = "64")]
mod layout_tests_ss;
#[cfg(target_os = "macos")]
mod mach_surface_share_pool_tests;
#[cfg(target_os = "linux")]
mod opaque_fd_texture_registration_payload_tests;
#[cfg(target_os = "linux")]
mod pixel_buffer_registration_payload_tests;
#[cfg(target_os = "linux")]
mod plane_fd_ownership_tests;
#[cfg(target_os = "linux")]
mod surface_export_staging_registration_payload_tests;
#[cfg(target_os = "linux")]
mod tiled_dma_buf_texture_lookup_reply_tests;
