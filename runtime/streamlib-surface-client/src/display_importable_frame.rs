// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The surface-share verb a viewer asks a frame through in memory its
//! display imports.

/// The op a viewer sends with a published frame id it holds claimed. The
/// answer is the registration of memory a display imports holding that frame
/// — the frame's own pool slot, or the engine's GPU copy of it — under the
/// `surface_id` the reply names, with that memory's handles unless the
/// request lists the id as already imported.
pub const SURFACE_SHARE_IMPORT_DISPLAY_IMPORTABLE_FRAME_OP: &str =
    "import_display_importable_frame";

/// The request field listing the surface-share ids whose memory the viewer
/// already holds an import of on this connection.
pub const SURFACE_SHARE_DISPLAY_IMPORTS_HELD_FIELD: &str = "display_imports_held";

/// The request field stating the raw `VkImageLayout` the frame's message
/// says its producer left the slot's image in; absent or 0 for a buffer slot.
pub const SURFACE_SHARE_FRAME_IMAGE_LAYOUT_FIELD: &str = "frame_image_layout";
