// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The surface-share verb a viewer asks a frame through, in memory it reads
//! the frame by.

/// The op a viewer sends with a published frame id it holds claimed. The
/// answer is the registration of memory holding that frame the way the
/// request's [`SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD`] asks — the frame's
/// own pool slot, or the engine's GPU copy of it — under the `surface_id` the
/// reply names, with that memory's handles unless the request lists the id
/// as already imported.
pub const SURFACE_SHARE_IMPORT_VIEWED_FRAME_OP: &str = "import_viewed_frame";

/// The request field listing the surface-share ids whose memory the viewer
/// already holds an import of on this connection.
pub const SURFACE_SHARE_VIEWED_FRAME_IMPORTS_HELD_FIELD: &str = "viewed_frame_imports_held";

/// The request field stating the raw `VkImageLayout` the frame's message
/// says its producer left the slot's image in; absent or 0 for a buffer slot.
pub const SURFACE_SHARE_FRAME_IMAGE_LAYOUT_FIELD: &str = "frame_image_layout";

/// The request field naming how the viewer reads the frame, as
/// [`ViewedFrameReadBy::wire_name`] spells it.
pub const SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD: &str = "viewed_frame_read_by";

/// How a viewer reads the frames it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewedFrameReadBy {
    /// Its display imports the memory: a DMA-BUF image with a tiled DRM
    /// format modifier.
    DisplayImport,
    /// The CPU reads the memory's rows: a tightly packed OPAQUE_FD buffer the
    /// viewer imports on its own Vulkan device and maps. For a display that
    /// shows black for every DMA-BUF it imports (Chromium on NVIDIA under
    /// X11).
    CpuRead,
}

impl ViewedFrameReadBy {
    /// The value [`SURFACE_SHARE_VIEWED_FRAME_READ_BY_FIELD`] carries.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::DisplayImport => "display_import",
            Self::CpuRead => "cpu_read",
        }
    }

    /// The reading `wire_name` names, `None` for any other value.
    pub fn from_wire_name(wire_name: &str) -> Option<Self> {
        match wire_name {
            "display_import" => Some(Self::DisplayImport),
            "cpu_read" => Some(Self::CpuRead),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_reading_round_trips_through_its_wire_name() {
        for read_by in [ViewedFrameReadBy::DisplayImport, ViewedFrameReadBy::CpuRead] {
            assert_eq!(
                ViewedFrameReadBy::from_wire_name(read_by.wire_name()),
                Some(read_by)
            );
        }
        assert_eq!(ViewedFrameReadBy::from_wire_name("dma_buf"), None);
    }
}
