// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The Image frame a `BrowserSource` publishes on `video`: a painted page named
//! by its output-pool slot, in sRGB.

use tatolab_module_sdk::{
    ModuleHostImageFrame, ModuleHostImageFrameH273Colour, ModuleHostSurface,
    ModuleHostTablePayloadDescriptor,
};

/// A page paints sRGB: BT.709 primaries (1), the IEC 61966-2-1 curve (13),
/// RGB samples (identity matrix, 0) at full range (1), as H.273 numbers them.
pub const PAINTED_PAGE_H273_COLOUR: ModuleHostImageFrameH273Colour =
    ModuleHostImageFrameH273Colour {
        primaries: 1,
        transfer: 13,
        matrix: 0,
        range: 1,
    };

/// The Image frame descriptor of `painted_frame_texture`, an output-pool
/// texture of `texture_format_wire_name`.
pub fn image_frame_payload_descriptor_of_a_painted_page(
    painted_frame_texture: &ModuleHostSurface,
    texture_format_wire_name: &str,
    width: u32,
    height: u32,
) -> Result<ModuleHostTablePayloadDescriptor, String> {
    let texture_format =
        ModuleHostImageFrame::texture_format_of_the_wire_name(texture_format_wire_name)
            .ok_or_else(|| format!("{texture_format_wire_name:?} names no texture format"))?;
    let gpu_pool_slot_frame = painted_frame_texture
        .gpu_pool_slot_frame()
        .map_err(|refusal| refusal.to_string())?;
    ModuleHostImageFrame {
        width,
        height,
        texture_format,
        colour: PAINTED_PAGE_H273_COLOUR,
    }
    .payload_descriptor_in_the_slot(&gpu_pool_slot_frame)
    .map_err(|refusal| refusal.to_string())
}
