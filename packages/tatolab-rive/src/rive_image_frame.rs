// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The Image frame a `RiveSource` publishes on `video`: a rendered frame named
//! by its output-pool slot, its colour unspecified.

use tatolab_module_sdk::{
    MODULE_HOST_H273_UNSPECIFIED, ModuleHostImageFrame, ModuleHostImageFrameH273Colour,
    ModuleHostSurface, ModuleHostTablePayloadDescriptor,
};

/// The Image frame descriptor of `rendered_frame_texture`, an output-pool
/// texture of `texture_format_wire_name`.
pub fn image_frame_payload_descriptor_of_a_rendered_frame(
    rendered_frame_texture: &ModuleHostSurface,
    texture_format_wire_name: &str,
    width: u32,
    height: u32,
) -> Result<ModuleHostTablePayloadDescriptor, String> {
    let texture_format =
        ModuleHostImageFrame::texture_format_of_the_wire_name(texture_format_wire_name)
            .ok_or_else(|| format!("{texture_format_wire_name:?} names no texture format"))?;
    let gpu_pool_slot_frame = rendered_frame_texture
        .gpu_pool_slot_frame()
        .map_err(|refusal| refusal.to_string())?;
    ModuleHostImageFrame {
        width,
        height,
        texture_format,
        colour: ModuleHostImageFrameH273Colour {
            primaries: MODULE_HOST_H273_UNSPECIFIED,
            transfer: MODULE_HOST_H273_UNSPECIFIED,
            matrix: MODULE_HOST_H273_UNSPECIFIED,
            range: MODULE_HOST_H273_UNSPECIFIED,
        },
    }
    .payload_descriptor_in_the_slot(&gpu_pool_slot_frame)
    .map_err(|refusal| refusal.to_string())
}
