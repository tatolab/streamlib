// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The Image frame this camera publishes on `video`: a converted frame named by
//! its output-pool slot, with the colour it was captured in.

use tatolab_module_sdk::{
    ModuleHostImageFrame, ModuleHostSurface, ModuleHostTablePayloadDescriptor,
};

use crate::camera_frame_publication::CONVERTED_FRAME_TEXTURE_FORMAT;
use crate::captured_frame_colour_description::CapturedFrameColourDescription;

/// The Image frame descriptor of `converted_frame_texture`, a texture from the
/// converted-frame pool, captured in `colour`.
pub fn image_frame_payload_descriptor_of_a_converted_frame(
    converted_frame_texture: &ModuleHostSurface,
    width: u32,
    height: u32,
    colour: &CapturedFrameColourDescription,
) -> Result<ModuleHostTablePayloadDescriptor, String> {
    let texture_format =
        ModuleHostImageFrame::texture_format_of_the_wire_name(CONVERTED_FRAME_TEXTURE_FORMAT)
            .ok_or_else(|| format!("{CONVERTED_FRAME_TEXTURE_FORMAT:?} names no texture format"))?;
    let gpu_pool_slot_frame = converted_frame_texture
        .gpu_pool_slot_frame()
        .map_err(|refusal| refusal.to_string())?;
    ModuleHostImageFrame {
        width,
        height,
        texture_format,
        colour: colour.image_frame_h273_colour(),
    }
    .payload_descriptor_in_the_slot(&gpu_pool_slot_frame)
    .map_err(|refusal| refusal.to_string())
}
