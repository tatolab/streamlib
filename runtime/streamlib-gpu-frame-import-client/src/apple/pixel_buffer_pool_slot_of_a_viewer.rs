// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A pixel-buffer pool slot as a viewer holds it: its IOSurface, handed on to
//! whatever displays the frame.

use std::sync::Arc;

use objc2_core_foundation::CFRetained;
use objc2_io_surface::IOSurfaceRef;
use streamlib_surface_client::{
    SURFACE_SHARE_HAS_CONSUME_DONE_PORT, SURFACE_SHARE_HAS_PRODUCE_DONE_PORT,
    resource_type_of_a_check_out,
};

use super::SurfaceShareTransferredHandle;

/// One pixel-buffer pool slot's IOSurface, as the surface-sharing service
/// handed it to a viewer: shared by every frame over the slot.
pub struct PixelBufferPoolSlotOfAViewer {
    iosurface: CFRetained<IOSurfaceRef>,
    width: u32,
    height: u32,
    format_wire_name: String,
}

// SAFETY: an IOSurface is safe to use from any thread, and CFRetain and
// CFRelease are atomic; the slot hands out only shared references.
unsafe impl Send for PixelBufferPoolSlotOfAViewer {}
// SAFETY: as above.
unsafe impl Sync for PixelBufferPoolSlotOfAViewer {}

impl PixelBufferPoolSlotOfAViewer {
    /// Read the service's answer to an `import_pool_slot` of `pool_slot_key`
    /// and the ports it carried — the IOSurface's, then the timeline edges the
    /// reply names — releasing each once the surface is looked up.
    pub(crate) fn from_the_import_reply(
        pool_slot_key: &str,
        reply: &serde_json::Value,
        received_handles: Vec<SurfaceShareTransferredHandle>,
    ) -> Result<Self, String> {
        let resource_type = resource_type_of_a_check_out(reply);
        let handle_type = reply
            .get("handle_type")
            .and_then(|value| value.as_str())
            .unwrap_or("iosurface");
        // A GPU image pool registers its slots as `texture`s over the same
        // IOSurface a pixel-buffer pool registers as a `pixel_buffer`.
        if !matches!(resource_type, "pixel_buffer" | "texture") || handle_type != "iosurface" {
            return Err(format!(
                "pool slot {pool_slot_key:?} is registered as a {resource_type:?} over a \
                 {handle_type:?} handle; a viewer imports IOSurface-backed pixel buffers and \
                 textures only"
            ));
        }
        let width = positive_u32_field(reply, pool_slot_key, "width")?;
        let height = positive_u32_field(reply, pool_slot_key, "height")?;
        let format_wire_name = reply
            .get("format")
            .and_then(|value| value.as_str())
            .ok_or_else(|| format!("the import of pool slot {pool_slot_key:?} named no format"))?
            .to_string();
        let timeline_edge_ports_named = [
            SURFACE_SHARE_HAS_PRODUCE_DONE_PORT,
            SURFACE_SHARE_HAS_CONSUME_DONE_PORT,
        ]
        .into_iter()
        .filter(|flag| reply.get(*flag).and_then(|value| value.as_bool()) == Some(true))
        .count();
        let received_port_count = received_handles.len();
        let mut received_handles = received_handles.into_iter();
        // A viewer hands the IOSurface on whole; the frame's claim, not the
        // timeline edges, keeps the slot from being reused under it.
        let (Some(iosurface_port), true) = (
            received_handles.next(),
            received_port_count == 1 + timeline_edge_ports_named,
        ) else {
            return Err(format!(
                "the import of pool slot {pool_slot_key:?} carried {received_port_count} \
                 port(s), not its IOSurface and the {timeline_edge_ports_named} timeline \
                 edge(s) its reply names"
            ));
        };
        drop(received_handles);
        let iosurface = IOSurfaceRef::lookup_from_mach_port(iosurface_port.as_raw_name())
            .ok_or_else(|| {
                format!(
                    "the import of pool slot {pool_slot_key:?} carried a port that names no \
                     IOSurface"
                )
            })?;
        drop(iosurface_port);
        let bytes_per_row = iosurface.bytes_per_row() as u64;
        if iosurface.width() < width as usize
            || iosurface.height() < height as usize
            || bytes_per_row * u64::from(height) > iosurface.alloc_size() as u64
        {
            return Err(format!(
                "pool slot {pool_slot_key:?} is registered as {width}x{height}, which its {}x{} \
                 IOSurface of {bytes_per_row}-byte rows cannot hold",
                iosurface.width(),
                iosurface.height(),
            ));
        }
        Ok(Self {
            iosurface,
            width,
            height,
            format_wire_name,
        })
    }

    /// The slot's IOSurface, for a display that imports it itself.
    pub fn iosurface(&self) -> &IOSurfaceRef {
        &self.iosurface
    }

    /// The slot's width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The slot's height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The wire name of the slot's pixel format (`"rgba32"`, `"bgra32"`, …).
    pub fn format_wire_name(&self) -> &str {
        &self.format_wire_name
    }

    /// The IOSurface's row pitch, padding included.
    pub fn bytes_per_row(&self) -> u64 {
        self.iosurface.bytes_per_row() as u64
    }
}

/// One viewed frame's raise of its slot's IOSurface use count, lowered on
/// drop: the claim `IOSurfaceIsInUse` answers across processes, which the
/// pool checks before it hands the slot out again.
pub(crate) struct IOSurfaceUseCountClaimOfAViewer {
    pool_slot: Arc<PixelBufferPoolSlotOfAViewer>,
}

impl IOSurfaceUseCountClaimOfAViewer {
    pub(crate) fn claiming(pool_slot: Arc<PixelBufferPoolSlotOfAViewer>) -> Self {
        pool_slot.iosurface.increment_use_count();
        Self { pool_slot }
    }
}

impl Drop for IOSurfaceUseCountClaimOfAViewer {
    fn drop(&mut self) {
        self.pool_slot.iosurface.decrement_use_count();
    }
}

fn positive_u32_field(
    reply: &serde_json::Value,
    pool_slot_key: &str,
    field: &str,
) -> Result<u32, String> {
    reply
        .get(field)
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            format!("the import of pool slot {pool_slot_key:?} carried no usable {field}")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn import_reply_registered_as(
        resource_type: &str,
        handle_type: &str,
        timeline_edge_ports_named: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "resource_type": resource_type,
            "handle_type": handle_type,
            "width": 64,
            "height": 32,
            "format": "bgra32",
            "has_produce_done_port": timeline_edge_ports_named,
            "has_consume_done_port": timeline_edge_ports_named,
        })
    }

    fn null_ports(count: usize) -> Vec<SurfaceShareTransferredHandle> {
        // SAFETY: a null name owns no reference, and dropping it releases none.
        (0..count)
            .map(|_| unsafe { SurfaceShareTransferredHandle::from_raw_name(0) })
            .collect()
    }

    fn refusal_of_an_import(reply: &serde_json::Value, port_count: usize) -> String {
        match PixelBufferPoolSlotOfAViewer::from_the_import_reply(
            "slot",
            reply,
            null_ports(port_count),
        ) {
            Ok(_) => panic!("an import whose ports name no IOSurface was admitted"),
            Err(refusal) => refusal,
        }
    }

    #[test]
    fn a_slot_carrying_its_iosurface_and_the_timeline_edges_its_reply_names_reaches_the_lookup() {
        for (resource_type, timeline_edge_ports_named, port_count) in [
            ("pixel_buffer", false, 1),
            ("texture", true, 3),
            ("texture", false, 1),
        ] {
            let refusal = refusal_of_an_import(
                &import_reply_registered_as(resource_type, "iosurface", timeline_edge_ports_named),
                port_count,
            );
            assert!(
                refusal.contains("carried a port that names no IOSurface"),
                "{resource_type} over {port_count} port(s): {refusal}"
            );
        }
    }

    #[test]
    fn a_slot_carrying_other_than_the_ports_its_reply_names_is_refused_counting_them() {
        for (timeline_edge_ports_named, port_count, edges_named) in
            [(true, 1, 2), (false, 3, 0), (true, 0, 2)]
        {
            let refusal = refusal_of_an_import(
                &import_reply_registered_as("texture", "iosurface", timeline_edge_ports_named),
                port_count,
            );
            assert!(
                refusal.contains(&format!(
                    "carried {port_count} port(s), not its IOSurface and the {edges_named} \
                     timeline edge(s) its reply names"
                )),
                "{refusal}"
            );
        }
    }

    #[test]
    fn a_slot_over_another_handle_or_of_another_resource_is_refused_by_its_registration() {
        for (resource_type, handle_type) in
            [("texture", "metal_shared_event"), ("buffer", "iosurface")]
        {
            let refusal = refusal_of_an_import(
                &import_reply_registered_as(resource_type, handle_type, false),
                1,
            );
            assert!(
                refusal.contains(&format!(
                    "is registered as a {resource_type:?} over a {handle_type:?} handle"
                )),
                "{refusal}"
            );
        }
    }
}
