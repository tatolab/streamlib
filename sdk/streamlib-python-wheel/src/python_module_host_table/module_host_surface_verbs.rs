// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The surface verbs of the module host table: a foreign DMA-BUF or IOSurface
//! import, a node output pool texture, a lend of a surface a message names, the
//! engine's colour conversion between them, and a write of host-memory pixels
//! into a texture, each a round trip on the helper's native escalate channel or
//! its surface-share socket, with no Python on either end.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
#[cfg(target_os = "linux")]
use tatolab_module_sdk::module_host_table_abi::MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES;
use tatolab_module_sdk::module_host_table_abi::{
    ModuleHostTableColorInfo, ModuleHostTableDmaBufImport, ModuleHostTableHostMemoryPixels,
    ModuleHostTableIOSurfaceImport, ModuleHostTableNodeOutputTextureRequest,
    ModuleHostTableSurfaceHandle, ModuleHostTableUtf8,
};

use crate::helper_process_native_escalate_channel::HelperProcessNativeEscalateChannel;
use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;

/// Where a surface lent to native code came from, which decides what giving
/// it back owes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceLentToNativeCodeOrigin {
    /// A foreign DMA-BUF or IOSurface this helper registered; its registration
    /// is removed.
    AdoptedForeignSurface,
    /// A node output pool slot; the pool keeps it.
    NodeOutputPoolTexture,
    /// A surface a message named, such as an upstream node's frame; it stays
    /// its publisher's, and the parent never keeps it past a conversion.
    NamedByAMessage,
}

struct SurfaceLentToNativeCode {
    surface_id: String,
    origin: SurfaceLentToNativeCodeOrigin,
    /// The parent resolved it for a conversion and holds it until told.
    resolved_by_the_parent_for_a_conversion: bool,
}

/// One node's surfaces lent to native code, by the handle the table hands out,
/// and the channels their verbs ride.
pub(super) struct SurfacesLentToNativeCode {
    surfaces_by_handle_value: Mutex<HashMap<u64, SurfaceLentToNativeCode>>,
    /// Held across one host-memory write, so two never fill a staging mapping at once.
    #[cfg(target_os = "linux")]
    host_memory_write_in_progress: Mutex<()>,
    next_handle_value: AtomicU64,
    native_escalate_channel: Option<Arc<HelperProcessNativeEscalateChannel>>,
    surface_share_client: Option<Arc<HelperProcessGpuExchangeClient>>,
}

impl SurfacesLentToNativeCode {
    pub(super) fn new(
        native_escalate_channel: Option<Arc<HelperProcessNativeEscalateChannel>>,
        surface_share_client: Option<Arc<HelperProcessGpuExchangeClient>>,
    ) -> Self {
        Self {
            surfaces_by_handle_value: Mutex::new(HashMap::new()),
            #[cfg(target_os = "linux")]
            host_memory_write_in_progress: Mutex::new(()),
            next_handle_value: AtomicU64::new(1),
            native_escalate_channel,
            surface_share_client,
        }
    }

    fn native_escalate_channel(&self) -> Result<&HelperProcessNativeEscalateChannel, String> {
        self.native_escalate_channel.as_deref().ok_or_else(|| {
            "this helper was started without a native escalate channel to its parent, so \
             native code reaches no GPU"
                .to_string()
        })
    }

    fn lend(
        &self,
        surface_id: String,
        origin: SurfaceLentToNativeCodeOrigin,
    ) -> ModuleHostTableSurfaceHandle {
        let value = self.next_handle_value.fetch_add(1, Ordering::Relaxed);
        self.surfaces_by_handle_value.lock().insert(
            value,
            SurfaceLentToNativeCode {
                surface_id,
                origin,
                resolved_by_the_parent_for_a_conversion: false,
            },
        );
        ModuleHostTableSurfaceHandle { value }
    }

    /// The surface id an output-pool texture's handle names; an imported
    /// foreign surface's handle is refused, because its pixels are its producer's.
    #[cfg(target_os = "linux")]
    fn output_pool_texture_id_of_a_handle(
        &self,
        surface: ModuleHostTableSurfaceHandle,
    ) -> Result<String, String> {
        match self.surfaces_by_handle_value.lock().get(&surface.value) {
            Some(lent) if lent.origin == SurfaceLentToNativeCodeOrigin::NodeOutputPoolTexture => {
                Ok(lent.surface_id.clone())
            }
            Some(lent) => Err(format!(
                "surface {} is an imported foreign surface, which takes no write: acquire a texture \
                 from a node output pool",
                lent.surface_id
            )),
            None => Err(format!(
                "surface handle {} names no surface this table lent",
                surface.value
            )),
        }
    }

    fn surface_id_of_a_handle(
        &self,
        surface: ModuleHostTableSurfaceHandle,
    ) -> Result<String, String> {
        self.surfaces_by_handle_value
            .lock()
            .get(&surface.value)
            .map(|lent| lent.surface_id.clone())
            .ok_or_else(|| {
                format!(
                    "surface handle {} names no surface this table lent",
                    surface.value
                )
            })
    }

    /// Adopt the DMA-BUF `dma_buf_import` describes.
    #[cfg(target_os = "linux")]
    pub(super) fn import_dma_buf(
        &self,
        dma_buf_import: &ModuleHostTableDmaBufImport,
    ) -> Result<ModuleHostTableSurfaceHandle, String> {
        use crate::python_helper_process_pixel_exchange::foreign_dma_buf_plane_layout;
        use streamlib::sdk::rhi::PixelFormat;

        let plane_count = dma_buf_import.plane_count as usize;
        if plane_count == 0 || plane_count > MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES {
            return Err(format!(
                "a DMA-BUF import states 1 to {MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES} \
                 planes, not {plane_count}"
            ));
        }
        // SAFETY: the SDK hands a view live for the call.
        let pixel_format_name = unsafe { dma_buf_import.pixel_format.as_str() }
            .map_err(|not_utf8| format!("a pixel format name is UTF-8: {not_utf8}"))?;
        let pixel_format = PixelFormat::parse_wire_name(pixel_format_name)?;
        let (allocation_byte_size, planes) = foreign_dma_buf_plane_layout(
            pixel_format,
            dma_buf_import.width,
            dma_buf_import.height,
            Some(dma_buf_import.allocation_byte_size),
            Some(dma_buf_import.plane_offsets_bytes[..plane_count].to_vec()),
            Some(dma_buf_import.plane_strides_bytes[..plane_count].to_vec()),
            dma_buf_import.drm_format_modifier,
        )?;
        let surface_share_client = self.surface_share_client.as_deref().ok_or_else(|| {
            "this helper was started without its surface-share socket, so it adopts no \
             DMA-BUF"
                .to_string()
        })?;
        let adopted_surface_id = surface_share_client
            .register_foreign_dma_buf(
                dma_buf_import.fd,
                dma_buf_import.width,
                dma_buf_import.height,
                pixel_format,
                allocation_byte_size,
                &planes,
            )
            .map_err(|refusal| refusal.to_string())?;
        Ok(self.lend(
            adopted_surface_id,
            SurfaceLentToNativeCodeOrigin::AdoptedForeignSurface,
        ))
    }

    /// Refuses by name: DMA-BUF is a Linux handle.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn import_dma_buf(
        &self,
        _dma_buf_import: &ModuleHostTableDmaBufImport,
    ) -> Result<ModuleHostTableSurfaceHandle, String> {
        Err("surface_import_dma_buf is Linux-only: a DMA-BUF is a Linux fd".to_string())
    }

    /// Adopt the IOSurface `iosurface_import` names.
    #[cfg(target_os = "macos")]
    pub(super) fn import_iosurface(
        &self,
        iosurface_import: &ModuleHostTableIOSurfaceImport,
    ) -> Result<ModuleHostTableSurfaceHandle, String> {
        use streamlib::sdk::rhi::PixelFormat;

        // SAFETY: the SDK hands a view live for the call.
        let pixel_format_name = unsafe { iosurface_import.pixel_format.as_str() }
            .map_err(|not_utf8| format!("a pixel format name is UTF-8: {not_utf8}"))?;
        let pixel_format = PixelFormat::parse_wire_name(pixel_format_name)?;
        let iosurface = std::ptr::NonNull::new(iosurface_import.iosurface.cast_mut())
            .ok_or_else(|| "an IOSurface import names a null IOSurfaceRef".to_string())?;
        // SAFETY: the SDK's contract is a live IOSurfaceRef the caller keeps
        // for the call; this borrows it without taking a reference.
        let iosurface: &objc2_io_surface::IOSurfaceRef = unsafe { iosurface.cast().as_ref() };
        let surface_share_client = self.surface_share_client.as_deref().ok_or_else(|| {
            "this helper was started without its surface-share service, so it adopts no \
             IOSurface"
                .to_string()
        })?;
        let adopted_surface_id = surface_share_client
            .register_foreign_iosurface(
                iosurface,
                iosurface_import.width,
                iosurface_import.height,
                pixel_format,
            )
            .map_err(|refusal| refusal.to_string())?;
        Ok(self.lend(
            adopted_surface_id,
            SurfaceLentToNativeCodeOrigin::AdoptedForeignSurface,
        ))
    }

    /// Refuses by name: an IOSurface is a macOS handle.
    #[cfg(not(target_os = "macos"))]
    pub(super) fn import_iosurface(
        &self,
        _iosurface_import: &ModuleHostTableIOSurfaceImport,
    ) -> Result<ModuleHostTableSurfaceHandle, String> {
        Err("surface_import_iosurface is macOS-only: an IOSurface is a macOS handle".to_string())
    }

    /// Lend the surface `surface_id` names, as a message named it.
    pub(super) fn lend_surface_a_message_names(
        &self,
        surface_id: &str,
    ) -> Result<ModuleHostTableSurfaceHandle, String> {
        if surface_id.is_empty() {
            return Err("a lend by surface id names no surface id".to_string());
        }
        Ok(self.lend(
            surface_id.to_string(),
            SurfaceLentToNativeCodeOrigin::NamedByAMessage,
        ))
    }

    /// Take the next texture out of the node output pool `request` names.
    pub(super) fn acquire_texture_from_node_output_pool(
        &self,
        request: &ModuleHostTableNodeOutputTextureRequest,
    ) -> Result<ModuleHostTableSurfaceHandle, String> {
        // SAFETY: the SDK hands views live for the call.
        let (pool_key, texture_format, usage) = unsafe {
            (
                request.pool_key.as_str(),
                request.texture_format.as_str(),
                request.usage.as_str(),
            )
        };
        let not_utf8 =
            |not_utf8: std::str::Utf8Error| format!("a texture request is UTF-8: {not_utf8}");
        let (pool_key, texture_format, usage) = (
            pool_key.map_err(not_utf8)?,
            texture_format.map_err(not_utf8)?,
            usage.map_err(not_utf8)?,
        );
        let usage: Vec<&str> = usage
            .split(',')
            .map(str::trim)
            .filter(|usage_name| !usage_name.is_empty())
            .collect();
        let answer = self
            .native_escalate_channel()?
            .request_from_parent(serde_json::json!({
                "op": "acquire_texture",
                "width": request.width,
                "height": request.height,
                "format": texture_format,
                "usage": usage,
                "processor_output_pool": {
                    "pool_key": pool_key,
                    "rotation_depth": request.rotation_depth,
                },
            }))?;
        let surface_id = answer
            .get("handle_id")
            .and_then(|handle_id| handle_id.as_str())
            .ok_or_else(|| {
                format!("the parent's acquire_texture answer named no handle_id: {answer}")
            })?
            .to_string();
        Ok(self.lend(
            surface_id,
            SurfaceLentToNativeCodeOrigin::NodeOutputPoolTexture,
        ))
    }

    /// Convert `source` into `destination` with the engine's colour converter;
    /// `ycbcr_side_color` describes whichever side is YCbCr.
    pub(super) fn convert_surface_into(
        &self,
        source: ModuleHostTableSurfaceHandle,
        destination: ModuleHostTableSurfaceHandle,
        ycbcr_side_color: &ModuleHostTableColorInfo,
    ) -> Result<(), String> {
        let source_surface_id = self.surface_id_of_a_handle(source)?;
        let destination_surface_id = self.surface_id_of_a_handle(destination)?;
        let mut ycbcr_side_color_names = serde_json::Map::new();
        for (axis, name) in [
            ("primaries", ycbcr_side_color.primaries),
            ("transfer", ycbcr_side_color.transfer),
            ("matrix", ycbcr_side_color.matrix),
            ("range", ycbcr_side_color.range),
        ] {
            let name = color_axis_name(axis, name)?;
            if !name.is_empty() {
                ycbcr_side_color_names.insert(axis.to_string(), name.into());
            }
        }
        {
            let mut surfaces_by_handle_value = self.surfaces_by_handle_value.lock();
            if let Some(lent) = surfaces_by_handle_value.get_mut(&source.value)
                && lent.origin != SurfaceLentToNativeCodeOrigin::NamedByAMessage
            {
                lent.resolved_by_the_parent_for_a_conversion = true;
            }
            if let Some(lent) = surfaces_by_handle_value.get_mut(&destination.value)
                && lent.origin == SurfaceLentToNativeCodeOrigin::AdoptedForeignSurface
            {
                lent.resolved_by_the_parent_for_a_conversion = true;
            }
        }
        self.native_escalate_channel()?
            .request_from_parent(serde_json::json!({
                "op": "convert_surface_into",
                "source_surface_id": source_surface_id,
                "destination_surface_id": destination_surface_id,
                "ycbcr_side_color": ycbcr_side_color_names,
            }))
            .map(|_answer| ())
    }

    /// Write `pixels` over every pixel of `destination`: the rows into the
    /// surface's host-visible staging, then the staging over the texture.
    #[cfg(target_os = "linux")]
    pub(super) fn write_from_host_memory(
        &self,
        destination: ModuleHostTableSurfaceHandle,
        pixels: &ModuleHostTableHostMemoryPixels,
    ) -> Result<(), String> {
        use super::host_memory_pixel_rows::{FourBytePixelRows, copy_four_byte_pixel_rows};
        use crate::python_helper_process_pixel_exchange::CpuReadbackCopyDirection;
        use streamlib::sdk::rhi::PixelFormat;

        let destination_surface_id = self.output_pool_texture_id_of_a_handle(destination)?;
        // SAFETY: the SDK hands views live for the call.
        let (pixel_format_name, pixel_bytes) =
            unsafe { (pixels.pixel_format.as_str(), pixels.pixels.as_slice()) };
        let pixel_format_name = pixel_format_name
            .map_err(|not_utf8| format!("a pixel format name is UTF-8: {not_utf8}"))?;
        let source = FourBytePixelRows {
            bytes: pixel_bytes,
            format: PixelFormat::parse_wire_name(pixel_format_name)?,
            width: pixels.width,
            height: pixels.height,
            row_stride_bytes: pixels.row_stride_bytes,
        };
        let native_escalate_channel = self.native_escalate_channel()?;
        let surface_share_client = self.surface_share_client.as_deref().ok_or_else(|| {
            "this helper was started without its surface-share socket, so it reaches no \
             staging to write host memory through"
                .to_string()
        })?;
        let _only_write_through_this_nodes_stagings = self.host_memory_write_in_progress.lock();
        let staging = surface_share_client.open_cpu_readback_export_from_native_code(
            native_escalate_channel,
            &destination_surface_id,
        )?;
        if !staging.writable {
            return Err(format!(
                "surface {destination_surface_id} takes no write: acquire it from a node \
                 output pool, whose textures take one"
            ));
        }
        let staging_byte_size = staging.consumer_buffer.size() as usize;
        // SAFETY: the mapping spans the staging's whole allocation and lives
        // as long as the memoised import; the write lock above keeps every
        // other host-memory write of this node out of it, and the parent reads
        // the staging only inside the copy below, after this borrow ends.
        let staging_bytes = unsafe {
            std::slice::from_raw_parts_mut(staging.consumer_buffer.mapped_ptr(), staging_byte_size)
        };
        copy_four_byte_pixel_rows(
            &source,
            staging.format,
            staging.bytes_per_row,
            staging.width,
            staging.height,
            staging_bytes,
        )?;
        surface_share_client.run_cpu_readback_copy_from_native_code(
            native_escalate_channel,
            CpuReadbackCopyDirection::WholeStagingOverTheSurface,
            &destination_surface_id,
            &staging,
        )
    }

    /// Refuses by name: the host-visible staging this writes through is
    /// Linux's.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn write_from_host_memory(
        &self,
        _destination: ModuleHostTableSurfaceHandle,
        _pixels: &ModuleHostTableHostMemoryPixels,
    ) -> Result<(), String> {
        Err("surface_write_from_host_memory is served on Linux only".to_string())
    }

    /// The surface id a handle names, as a view the map keeps alive until the
    /// handle is released; empty for a handle that names nothing.
    pub(super) fn surface_id_view(
        &self,
        surface: ModuleHostTableSurfaceHandle,
    ) -> ModuleHostTableUtf8 {
        match self.surfaces_by_handle_value.lock().get(&surface.value) {
            // The text lives on the heap the entry owns, which stays put until
            // the entry is removed.
            Some(lent) => ModuleHostTableUtf8 {
                bytes: lent.surface_id.as_ptr(),
                length: lent.surface_id.len(),
            },
            None => ModuleHostTableUtf8::of(""),
        }
    }

    /// Give `surface` back, paying what its origin owes.
    pub(super) fn release(&self, surface: ModuleHostTableSurfaceHandle) {
        let Some(lent) = self.surfaces_by_handle_value.lock().remove(&surface.value) else {
            return;
        };
        self.pay_what_a_released_surface_owes(lent);
    }

    fn pay_what_a_released_surface_owes(&self, lent: SurfaceLentToNativeCode) {
        if lent.resolved_by_the_parent_for_a_conversion
            && let Ok(native_escalate_channel) = self.native_escalate_channel()
            && let Err(release_failure) =
                native_escalate_channel.request_from_parent(serde_json::json!({
                    "op": "release_handle",
                    "handle_id": lent.surface_id,
                }))
        {
            tracing::warn!(
                "the parent did not take the release of conversion surface {} ({release_failure}); \
                 it holds the import until this node stops",
                lent.surface_id
            );
        }
        if lent.origin == SurfaceLentToNativeCodeOrigin::AdoptedForeignSurface {
            self.unregister_an_adoption(&lent.surface_id);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn unregister_an_adoption(&self, surface_id: &str) {
        let Some(surface_share_client) = &self.surface_share_client else {
            return;
        };
        if let Err(release_failure) = surface_share_client.unregister_foreign_surface(surface_id) {
            tracing::warn!(
                "unregistering adopted surface {surface_id} failed ({release_failure}); the \
                 service's reference to the foreign surface stays until this node stops"
            );
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn unregister_an_adoption(&self, _surface_id: &str) {}
}

impl Drop for SurfacesLentToNativeCode {
    /// Native code that never gave a surface back has it given back here, when
    /// the last count on the node host goes.
    fn drop(&mut self) {
        let still_lent: Vec<SurfaceLentToNativeCode> = self
            .surfaces_by_handle_value
            .lock()
            .drain()
            .map(|(_handle_value, lent)| lent)
            .collect();
        for lent in still_lent {
            self.pay_what_a_released_surface_owes(lent);
        }
    }
}

fn color_axis_name(axis: &str, name: ModuleHostTableUtf8) -> Result<&str, String> {
    if name.length == 0 {
        return Ok("");
    }
    // SAFETY: the SDK hands views live for the call.
    unsafe { name.as_str() }.map_err(|not_utf8| format!("a {axis} name is UTF-8: {not_utf8}"))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use super::*;
    #[cfg(target_os = "linux")]
    use tatolab_module_sdk::module_host_table_abi::ModuleHostTableBytes;

    /// A parent stand-in that answers each request with `answer_for` and hands
    /// back every request it saw once the channel closes.
    fn parent_answering(
        mut parent_end: UnixStream,
        answer_for: fn(&serde_json::Value) -> serde_json::Value,
    ) -> std::thread::JoinHandle<Vec<serde_json::Value>> {
        std::thread::spawn(move || {
            let mut requests = Vec::new();
            loop {
                let mut length = [0u8; 4];
                if parent_end.read_exact(&mut length).is_err() {
                    return requests;
                }
                let mut request = vec![0u8; u32::from_be_bytes(length) as usize];
                parent_end.read_exact(&mut request).expect("a request");
                let request: serde_json::Value =
                    serde_json::from_slice(&request).expect("a JSON request");
                let mut answer = answer_for(&request);
                answer["request_id"] = request["request_id"].clone();
                requests.push(request);
                let answer = serde_json::to_vec(&answer).expect("an answer encodes");
                parent_end
                    .write_all(&(answer.len() as u32).to_be_bytes())
                    .expect("an answer length");
                parent_end.write_all(&answer).expect("an answer");
            }
        })
    }

    fn answer_like_the_engine(request: &serde_json::Value) -> serde_json::Value {
        match request["op"].as_str() {
            Some("acquire_texture") => serde_json::json!({
                "result": "ok",
                "handle_id": format!("pool-slot#{}", request["width"]),
            }),
            Some("convert_surface_into") if request["ycbcr_side_color"]["range"] == "tv" => {
                serde_json::json!({"result": "err", "message": "unknown range \"tv\""})
            }
            _ => serde_json::json!({"result": "ok"}),
        }
    }

    fn texture_request(width: u32) -> ModuleHostTableNodeOutputTextureRequest {
        ModuleHostTableNodeOutputTextureRequest {
            pool_key: ModuleHostTableUtf8::of("camera"),
            texture_format: ModuleHostTableUtf8::of("rgba8_unorm"),
            usage: ModuleHostTableUtf8::of("storage_binding, texture_binding"),
            width,
            height: 16,
            rotation_depth: 3,
            reserved: 0,
        }
    }

    fn color(range: &'static str) -> ModuleHostTableColorInfo {
        ModuleHostTableColorInfo {
            primaries: ModuleHostTableUtf8::of(""),
            transfer: ModuleHostTableUtf8::of(""),
            matrix: ModuleHostTableUtf8::of("bt709"),
            range: ModuleHostTableUtf8::of(range),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_host_memory_write_over_an_imported_dma_buf_is_refused_before_any_staging_opens() {
        let surfaces = SurfacesLentToNativeCode::new(None, None);
        let imported = surfaces.lend(
            "foreign-dma-buf#1".to_string(),
            SurfaceLentToNativeCodeOrigin::AdoptedForeignSurface,
        );
        let pixels = [0u8; 16];
        let host_memory_pixels = ModuleHostTableHostMemoryPixels {
            pixels: ModuleHostTableBytes::of(&pixels),
            pixel_format: ModuleHostTableUtf8::of("bgra32"),
            width: 2,
            height: 2,
            row_stride_bytes: 8,
        };

        let refusal = surfaces
            .write_from_host_memory(imported, &host_memory_pixels)
            .unwrap_err();

        assert!(refusal.contains("imported foreign surface"), "{refusal}");
    }

    #[test]
    fn a_conversion_reaches_the_parent_by_surface_id_and_its_source_is_released_once() {
        let (child_end, parent_end) = UnixStream::pair().expect("socketpair");
        let parent = parent_answering(parent_end, answer_like_the_engine);
        let surfaces = SurfacesLentToNativeCode::new(
            Some(Arc::new(HelperProcessNativeEscalateChannel::over(
                child_end,
            ))),
            None,
        );

        let source = surfaces
            .acquire_texture_from_node_output_pool(&texture_request(64))
            .expect("the source is acquired");
        let destination = surfaces
            .acquire_texture_from_node_output_pool(&texture_request(65))
            .expect("the destination is acquired");
        // SAFETY: the view lives until the handle is released.
        assert_eq!(
            unsafe { surfaces.surface_id_view(destination).as_str() },
            Ok("pool-slot#65")
        );
        surfaces
            .convert_surface_into(source, destination, &color("full"))
            .expect("the conversion is answered ok");
        let refusal = surfaces
            .convert_surface_into(source, destination, &color("tv"))
            .expect_err("the parent refuses");
        assert_eq!(refusal, "unknown range \"tv\"");
        surfaces.release(destination);
        surfaces.release(source);
        surfaces.release(source);
        drop(surfaces);

        let requests = parent.join().expect("the parent stand-in");
        let ops: Vec<&str> = requests
            .iter()
            .map(|request| request["op"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            ops,
            [
                "acquire_texture",
                "acquire_texture",
                "convert_surface_into",
                "convert_surface_into",
                "release_handle"
            ]
        );
        assert_eq!(
            requests[0]["usage"],
            serde_json::json!(["storage_binding", "texture_binding"])
        );
        assert_eq!(
            requests[0]["processor_output_pool"],
            serde_json::json!({"pool_key": "camera", "rotation_depth": 3})
        );
        assert_eq!(
            requests[2]["ycbcr_side_color"],
            serde_json::json!({"matrix": "bt709", "range": "full"})
        );
        assert_eq!(requests[2]["source_surface_id"], "pool-slot#64");
        assert_eq!(requests[4]["handle_id"], "pool-slot#64");
    }

    #[test]
    fn a_lent_frame_is_never_released_at_the_parent_and_an_adopted_destination_is() {
        let (child_end, parent_end) = UnixStream::pair().expect("socketpair");
        let parent = parent_answering(parent_end, answer_like_the_engine);
        let surfaces = SurfacesLentToNativeCode::new(
            Some(Arc::new(HelperProcessNativeEscalateChannel::over(
                child_end,
            ))),
            None,
        );

        let upstream_frame = surfaces
            .lend_surface_a_message_names("upstream-pool-slot#2")
            .expect("the frame is lent");
        let adopted_nv12_surface = surfaces.lend(
            "adopted-iosurface".to_string(),
            SurfaceLentToNativeCodeOrigin::AdoptedForeignSurface,
        );
        surfaces
            .convert_surface_into(upstream_frame, adopted_nv12_surface, &color("limited"))
            .expect("the conversion is answered ok");
        surfaces.release(upstream_frame);
        surfaces.release(adopted_nv12_surface);
        assert!(
            surfaces.lend_surface_a_message_names("").is_err(),
            "an empty id names nothing"
        );
        drop(surfaces);

        let requests = parent.join().expect("the parent stand-in");
        let ops: Vec<&str> = requests
            .iter()
            .map(|request| request["op"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(ops, ["convert_surface_into", "release_handle"]);
        assert_eq!(requests[0]["source_surface_id"], "upstream-pool-slot#2");
        assert_eq!(requests[1]["handle_id"], "adopted-iosurface");
    }

    #[test]
    fn a_handle_the_table_never_lent_is_refused_by_value() {
        let surfaces = SurfacesLentToNativeCode::new(None, None);
        let refusal = surfaces
            .convert_surface_into(
                ModuleHostTableSurfaceHandle { value: 7 },
                ModuleHostTableSurfaceHandle { value: 8 },
                &color(""),
            )
            .expect_err("no surface was lent");
        assert!(refusal.contains("handle 7"), "{refusal}");
        // SAFETY: an empty view.
        assert_eq!(
            unsafe {
                surfaces
                    .surface_id_view(ModuleHostTableSurfaceHandle { value: 7 })
                    .as_str()
            },
            Ok("")
        );
    }

    #[test]
    fn a_helper_without_a_native_channel_refuses_naming_it() {
        let surfaces = SurfacesLentToNativeCode::new(None, None);
        let refusal = surfaces
            .acquire_texture_from_node_output_pool(&texture_request(64))
            .expect_err("no channel");
        assert!(refusal.contains("native escalate channel"), "{refusal}");
    }
}
