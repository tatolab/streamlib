// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Safe Rust over the module host table: the ports, log, clock and stop flag
//! of the node a module's native half runs for.

use std::time::Duration;

use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR,
    MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES, MODULE_HOST_TABLE_LOG_LEVEL_DEBUG,
    MODULE_HOST_TABLE_LOG_LEVEL_ERROR, MODULE_HOST_TABLE_LOG_LEVEL_INFO,
    MODULE_HOST_TABLE_LOG_LEVEL_TRACE, MODULE_HOST_TABLE_LOG_LEVEL_WARN,
    MODULE_HOST_TABLE_STATUS_EMPTY, MODULE_HOST_TABLE_STATUS_OK,
    MODULE_HOST_TABLE_STATUS_TIMED_OUT, MODULE_HOST_TABLE_TIMESTAMP_NOW, ModuleHostTable,
    ModuleHostTableBytes, ModuleHostTableColorInfo, ModuleHostTableDmaBufImport,
    ModuleHostTableErrorMessage, ModuleHostTableHostMemoryPixels,
    ModuleHostTableHostMemoryPixelsToFill, ModuleHostTableIOSurfaceImport,
    ModuleHostTableMutableBytes, ModuleHostTableNodeOutputTextureRequest,
    ModuleHostTablePayloadDescriptor, ModuleHostTablePortHandle, ModuleHostTableReadMessage,
    ModuleHostTableStatus, ModuleHostTableSurfaceHandle, ModuleHostTableUtf8,
};

/// Why the module host table, or one of its verbs, refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleHostError {
    /// The table pointer handed over was null.
    NoModuleHostTable,
    /// The runtime lends a table of a major this module was not built for.
    ModuleHostTableMajorMismatch {
        runtime_major: u32,
        runtime_minor: u32,
        module_major: u32,
        module_minor: u32,
    },
    /// The runtime lends an older minor than the one this module needs.
    ModuleHostTableOlderThanTheModuleNeeds {
        runtime_major: u32,
        runtime_minor: u32,
        module_major: u32,
        module_minor: u32,
    },
    /// The table is smaller than the fields its own minor states.
    ModuleHostTableSmallerThanItsMinorStates {
        table_size_bytes: u64,
        size_the_module_reads_bytes: u64,
    },
    /// A verb was refused by the host, naming why.
    VerbRefused { verb: &'static str, refusal: String },
    /// What the Python half handed over is not the module host table capsule.
    NotAModuleHostTableCapsule { refusal: String },
}

impl std::fmt::Display for ModuleHostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoModuleHostTable => {
                write!(
                    formatter,
                    "no module host table was handed over (a null pointer)"
                )
            }
            Self::ModuleHostTableMajorMismatch {
                runtime_major,
                runtime_minor,
                module_major,
                module_minor,
            } => write!(
                formatter,
                "this module was built against module host table {module_major}.{module_minor}, \
                 and this runtime lends {runtime_major}.{runtime_minor}; a module loads only on a \
                 runtime of its own major — install a release of the module built for \
                 {runtime_major}.x"
            ),
            Self::ModuleHostTableOlderThanTheModuleNeeds {
                runtime_major,
                runtime_minor,
                module_major,
                module_minor,
            } => write!(
                formatter,
                "this module needs module host table {module_major}.{module_minor}, and this \
                 runtime lends {runtime_major}.{runtime_minor} — update the runtime"
            ),
            Self::ModuleHostTableSmallerThanItsMinorStates {
                table_size_bytes,
                size_the_module_reads_bytes,
            } => write!(
                formatter,
                "the module host table is {table_size_bytes} bytes, short of the \
                 {size_the_module_reads_bytes} bytes its minor states"
            ),
            Self::VerbRefused { verb, refusal } => write!(formatter, "{verb}: {refusal}"),
            Self::NotAModuleHostTableCapsule { refusal } => formatter.write_str(refusal),
        }
    }
}

impl std::error::Error for ModuleHostError {}

/// Refuse a table this module cannot read: another major, an older minor, or
/// fewer bytes than its minor states.
pub fn refuse_a_module_host_table_this_module_cannot_read(
    runtime_major: u32,
    runtime_minor: u32,
    table_size_bytes: u64,
    module_major: u32,
    module_minor: u32,
    size_the_module_reads_bytes: u64,
) -> Result<(), ModuleHostError> {
    if runtime_major != module_major {
        return Err(ModuleHostError::ModuleHostTableMajorMismatch {
            runtime_major,
            runtime_minor,
            module_major,
            module_minor,
        });
    }
    if runtime_minor < module_minor {
        return Err(ModuleHostError::ModuleHostTableOlderThanTheModuleNeeds {
            runtime_major,
            runtime_minor,
            module_major,
            module_minor,
        });
    }
    if table_size_bytes < size_the_module_reads_bytes {
        return Err(ModuleHostError::ModuleHostTableSmallerThanItsMinorStates {
            table_size_bytes,
            size_the_module_reads_bytes,
        });
    }
    Ok(())
}

/// A record's severity on the node's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleHostLogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl ModuleHostLogLevel {
    fn as_module_host_table_level(self) -> u32 {
        match self {
            Self::Error => MODULE_HOST_TABLE_LOG_LEVEL_ERROR,
            Self::Warn => MODULE_HOST_TABLE_LOG_LEVEL_WARN,
            Self::Info => MODULE_HOST_TABLE_LOG_LEVEL_INFO,
            Self::Debug => MODULE_HOST_TABLE_LOG_LEVEL_DEBUG,
            Self::Trace => MODULE_HOST_TABLE_LOG_LEVEL_TRACE,
        }
    }
}

/// The node a module's native half runs for, reached from any thread without
/// Python.
///
/// Holds one reference on the node host for as long as it lives; a clone
/// holds another.
#[derive(Debug)]
pub struct ModuleHost {
    pub(crate) module_host_table: ModuleHostTable,
}

// SAFETY: the table's contract is that every verb is callable from any
// thread, and the node host it names is reference-counted by the host.
unsafe impl Send for ModuleHost {}
unsafe impl Sync for ModuleHost {}

impl ModuleHost {
    /// Read the table at `module_host_table`, refusing one this module cannot
    /// read, and retain its node host.
    ///
    /// # Safety
    ///
    /// `module_host_table` is null or points at a module host table at least
    /// as large as its `table_size_bytes` states, whose node host is alive for
    /// the length of this call.
    pub unsafe fn from_module_host_table_pointer(
        module_host_table: *const ModuleHostTable,
    ) -> Result<Self, ModuleHostError> {
        if module_host_table.is_null() {
            return Err(ModuleHostError::NoModuleHostTable);
        }
        // SAFETY: non-null and, by the caller's contract, at least the header
        // is readable.
        let (runtime_major, runtime_minor, table_size_bytes) = unsafe {
            (
                (*module_host_table).abi_major,
                (*module_host_table).abi_minor,
                (*module_host_table).table_size_bytes,
            )
        };
        refuse_a_module_host_table_this_module_cannot_read(
            runtime_major,
            runtime_minor,
            table_size_bytes,
            MODULE_HOST_TABLE_ABI_MAJOR,
            MODULE_HOST_TABLE_ABI_MINOR,
            std::mem::size_of::<ModuleHostTable>() as u64,
        )?;
        // SAFETY: the table holds at least the fields of this SDK's minor, and
        // a newer minor only appends after them.
        let module_host_table = unsafe { std::ptr::read(module_host_table) };
        // SAFETY: the node host is alive for this call, by the caller's contract.
        unsafe { (module_host_table.node_host_retain)(module_host_table.node_host) };
        Ok(Self { module_host_table })
    }

    /// The table's major and minor, as the runtime lends it.
    pub fn module_host_table_version(&self) -> (u32, u32) {
        (
            self.module_host_table.abi_major,
            self.module_host_table.abi_minor,
        )
    }

    /// Open the output port Python lent to native code under `port_name`.
    pub fn open_output_port(
        &self,
        port_name: &str,
    ) -> Result<ModuleHostOutputPort, ModuleHostError> {
        let handle = self.open_port(
            "output_port_open",
            port_name,
            self.module_host_table.output_port_open,
        )?;
        Ok(ModuleHostOutputPort {
            module_host: self.clone(),
            port_name: port_name.to_string(),
            handle,
        })
    }

    /// Open the input port Python lent to native code under `port_name`.
    pub fn open_input_port(&self, port_name: &str) -> Result<ModuleHostInputPort, ModuleHostError> {
        let handle = self.open_port(
            "input_port_open",
            port_name,
            self.module_host_table.input_port_open,
        )?;
        Ok(ModuleHostInputPort {
            module_host: self.clone(),
            port_name: port_name.to_string(),
            handle,
        })
    }

    fn open_port(
        &self,
        verb: &'static str,
        port_name: &str,
        open: unsafe extern "C" fn(
            *mut std::ffi::c_void,
            ModuleHostTableUtf8,
            *mut ModuleHostTablePortHandle,
            *mut ModuleHostTableErrorMessage,
        ) -> ModuleHostTableStatus,
    ) -> Result<ModuleHostTablePortHandle, ModuleHostError> {
        let mut handle = ModuleHostTablePortHandle { value: 0 };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained by `self`, and every pointer is
        // to a live local for the length of the call.
        let status = unsafe {
            open(
                self.module_host_table.node_host,
                ModuleHostTableUtf8::of(port_name),
                &mut handle,
                &mut error_message,
            )
        };
        refuse_unless_ok(verb, status, &error_message)?;
        Ok(handle)
    }

    /// Wait up to `timeout` for any input port to hold a message; `true` when
    /// one does.
    pub fn wait_for_any_input_port(&self, timeout: Duration) -> Result<bool, ModuleHostError> {
        let mut error_message = ModuleHostTableErrorMessage::empty();
        let timeout_ns = u64::try_from(timeout.as_nanos()).unwrap_or(u64::MAX);
        // SAFETY: as in `open_port`.
        let status = unsafe {
            (self.module_host_table.input_wait_for_any_port)(
                self.module_host_table.node_host,
                timeout_ns,
                &mut error_message,
            )
        };
        match status {
            MODULE_HOST_TABLE_STATUS_TIMED_OUT => Ok(false),
            status => {
                refuse_unless_ok("input_wait_for_any_port", status, &error_message).map(|()| true)
            }
        }
    }

    /// Whether the node has been asked to stop; a native loop ends when it is.
    pub fn stop_requested(&self) -> bool {
        // SAFETY: the node host is retained by `self`.
        unsafe { (self.module_host_table.stop_requested)(self.module_host_table.node_host) != 0 }
    }

    /// Fail this node for good with `reason`, which the graph shows; the rest
    /// of the stream keeps running and the node is not started again.
    pub fn fail_the_node_unrecoverably(&self, reason: &str) -> Result<(), ModuleHostError> {
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained by `self`; every pointer is live
        // for the call.
        let status = unsafe {
            (self.module_host_table.node_fail_unrecoverably)(
                self.module_host_table.node_host,
                ModuleHostTableUtf8::of(reason),
                &mut error_message,
            )
        };
        refuse_unless_ok("node_fail_unrecoverably", status, &error_message)
    }

    /// The engine's monotonic clock in nanoseconds.
    pub fn monotonic_now_ns(&self) -> i64 {
        // SAFETY: the node host is retained by `self`.
        unsafe { (self.module_host_table.monotonic_now_ns)(self.module_host_table.node_host) }
    }

    /// The time on the node's stream clock at `machine_instant_ns`, a reading
    /// of [`ModuleHost::monotonic_now_ns`]'s clock such as a device's capture
    /// stamp.
    pub fn stream_time_ns_of_a_machine_instant(&self, machine_instant_ns: i64) -> i64 {
        // SAFETY: the node host is retained by `self`.
        unsafe {
            (self.module_host_table.stream_time_ns_of_a_machine_instant)(
                self.module_host_table.node_host,
                machine_instant_ns,
            )
        }
    }

    /// Hand one record to the node's log under `target`.
    pub fn log(&self, level: ModuleHostLogLevel, target: &str, message: &str) {
        // SAFETY: the node host is retained by `self`; both views are live.
        unsafe {
            (self.module_host_table.log)(
                self.module_host_table.node_host,
                level.as_module_host_table_level(),
                ModuleHostTableUtf8::of(target),
                ModuleHostTableUtf8::of(message),
            )
        }
    }

    /// Adopt a foreign DMA-BUF as a surface the graph can resolve: `fd` stays
    /// the caller's, and the pixels of `pixel_format` sit at each plane's
    /// offset and row stride inside its `allocation_byte_size` bytes.
    pub fn import_dma_buf(
        &self,
        dma_buf: &ModuleHostForeignDmaBuf<'_>,
    ) -> Result<ModuleHostSurface, ModuleHostError> {
        let plane_count = dma_buf.plane_offsets_bytes.len();
        if plane_count != dma_buf.plane_strides_bytes.len()
            || plane_count == 0
            || plane_count > MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES
        {
            return Err(ModuleHostError::VerbRefused {
                verb: "surface_import_dma_buf",
                refusal: format!(
                    "a DMA-BUF import states 1 to {MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES} \
                     planes, one offset and one stride each; this one states {} offset(s) and \
                     {} stride(s)",
                    dma_buf.plane_offsets_bytes.len(),
                    dma_buf.plane_strides_bytes.len()
                ),
            });
        }
        let mut plane_offsets_bytes = [0u64; MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES];
        let mut plane_strides_bytes = [0u64; MODULE_HOST_TABLE_DMA_BUF_IMPORT_MAX_PLANES];
        plane_offsets_bytes[..plane_count].copy_from_slice(dma_buf.plane_offsets_bytes);
        plane_strides_bytes[..plane_count].copy_from_slice(dma_buf.plane_strides_bytes);
        let dma_buf_import = ModuleHostTableDmaBufImport {
            fd: dma_buf.fd,
            width: dma_buf.width,
            height: dma_buf.height,
            plane_count: plane_count as u32,
            pixel_format: ModuleHostTableUtf8::of(dma_buf.pixel_format),
            allocation_byte_size: dma_buf.allocation_byte_size,
            drm_format_modifier: dma_buf.drm_format_modifier,
            plane_offsets_bytes,
            plane_strides_bytes,
        };
        let mut imported_surface = ModuleHostTableSurfaceHandle { value: 0 };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained by `self`, and every pointer is
        // to a live local for the length of the call.
        let status = unsafe {
            (self.module_host_table.surface_import_dma_buf)(
                self.module_host_table.node_host,
                &dma_buf_import,
                &mut imported_surface,
                &mut error_message,
            )
        };
        refuse_unless_ok("surface_import_dma_buf", status, &error_message)?;
        Ok(ModuleHostSurface {
            module_host: self.clone(),
            handle: imported_surface,
        })
    }

    /// Adopt a foreign IOSurface as a surface the graph can resolve: the
    /// `IOSurfaceRef` stays the caller's, and the host keeps its own reference
    /// for as long as the returned surface lives. macOS only.
    pub fn import_iosurface(
        &self,
        iosurface: &ModuleHostForeignIOSurface<'_>,
    ) -> Result<ModuleHostSurface, ModuleHostError> {
        if iosurface.iosurface.is_null() {
            return Err(ModuleHostError::VerbRefused {
                verb: "surface_import_iosurface",
                refusal: "an IOSurface import names a null IOSurfaceRef".to_string(),
            });
        }
        let iosurface_import = ModuleHostTableIOSurfaceImport {
            iosurface: iosurface.iosurface,
            pixel_format: ModuleHostTableUtf8::of(iosurface.pixel_format),
            width: iosurface.width,
            height: iosurface.height,
        };
        let mut imported_surface = ModuleHostTableSurfaceHandle { value: 0 };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `import_dma_buf`.
        let status = unsafe {
            (self.module_host_table.surface_import_iosurface)(
                self.module_host_table.node_host,
                &iosurface_import,
                &mut imported_surface,
                &mut error_message,
            )
        };
        refuse_unless_ok("surface_import_iosurface", status, &error_message)?;
        Ok(ModuleHostSurface {
            module_host: self.clone(),
            handle: imported_surface,
        })
    }

    /// Take the next texture out of the node output pool `pool_key`.
    pub fn acquire_texture_from_node_output_pool(
        &self,
        request: &ModuleHostNodeOutputTextureRequest<'_>,
    ) -> Result<ModuleHostSurface, ModuleHostError> {
        let usage = request.usage.join(",");
        let table_request = ModuleHostTableNodeOutputTextureRequest {
            pool_key: ModuleHostTableUtf8::of(request.pool_key),
            texture_format: ModuleHostTableUtf8::of(request.texture_format),
            usage: ModuleHostTableUtf8::of(&usage),
            width: request.width,
            height: request.height,
            rotation_depth: request.rotation_depth,
            reserved: 0,
        };
        let mut acquired_surface = ModuleHostTableSurfaceHandle { value: 0 };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `import_dma_buf`.
        let status = unsafe {
            (self
                .module_host_table
                .output_texture_acquire_from_node_output_pool)(
                self.module_host_table.node_host,
                &table_request,
                &mut acquired_surface,
                &mut error_message,
            )
        };
        refuse_unless_ok(
            "output_texture_acquire_from_node_output_pool",
            status,
            &error_message,
        )?;
        Ok(ModuleHostSurface {
            module_host: self.clone(),
            handle: acquired_surface,
        })
    }

    /// Lend native code the surface a message names by `surface_id`, such
    /// as an upstream node's frame, to convert from; dropping the lend leaves
    /// the surface its publisher's.
    pub fn lend_surface_a_message_names(
        &self,
        surface_id: &str,
    ) -> Result<ModuleHostSurface, ModuleHostError> {
        let mut lent_surface = ModuleHostTableSurfaceHandle { value: 0 };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `import_dma_buf`.
        let status = unsafe {
            (self.module_host_table.surface_lend_by_surface_id)(
                self.module_host_table.node_host,
                ModuleHostTableUtf8::of(surface_id),
                &mut lent_surface,
                &mut error_message,
            )
        };
        refuse_unless_ok("surface_lend_by_surface_id", status, &error_message)?;
        Ok(ModuleHostSurface {
            module_host: self.clone(),
            handle: lent_surface,
        })
    }

    /// Convert between a YCbCr surface and an RGBA texture of the same extent,
    /// returning once `destination`'s next reader sees the result:
    /// `ycbcr_side_color` describes whichever side is YCbCr. A YCbCr source
    /// converts into an RGBA texture; on macOS an RGBA texture also converts
    /// into an adopted NV12 IOSurface.
    pub fn convert_surface_into(
        &self,
        source: &ModuleHostSurface,
        destination: &ModuleHostSurface,
        ycbcr_side_color: &ModuleHostColorInfo<'_>,
    ) -> Result<(), ModuleHostError> {
        let table_color = ModuleHostTableColorInfo {
            primaries: ModuleHostTableUtf8::of(ycbcr_side_color.primaries.unwrap_or("")),
            transfer: ModuleHostTableUtf8::of(ycbcr_side_color.transfer.unwrap_or("")),
            matrix: ModuleHostTableUtf8::of(ycbcr_side_color.matrix.unwrap_or("")),
            range: ModuleHostTableUtf8::of(ycbcr_side_color.range.unwrap_or("")),
        };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `import_dma_buf`; both surfaces are this host's.
        let status = unsafe {
            (self.module_host_table.surface_convert_into)(
                self.module_host_table.node_host,
                source.handle,
                destination.handle,
                &table_color,
                &mut error_message,
            )
        };
        refuse_unless_ok("surface_convert_into", status, &error_message)
    }

    /// Write `pixels` over every pixel of `destination`, an output-pool
    /// texture of the same extent, returning once its next reader sees them.
    pub fn write_surface_from_host_memory(
        &self,
        destination: &ModuleHostSurface,
        pixels: &ModuleHostHostMemoryPixels<'_>,
    ) -> Result<(), ModuleHostError> {
        let bytes_the_rows_span =
            pixels
                .bytes_the_rows_span()
                .ok_or_else(|| ModuleHostError::VerbRefused {
                    verb: "surface_write_from_host_memory",
                    refusal: format!(
                        "{}x{} pixels {} bytes apart do not fit in memory",
                        pixels.width, pixels.height, pixels.row_stride_bytes
                    ),
                })?;
        if (pixels.pixels.len() as u64) < bytes_the_rows_span {
            return Err(ModuleHostError::VerbRefused {
                verb: "surface_write_from_host_memory",
                refusal: format!(
                    "{} rows of {} four-byte pixels, {} bytes apart, span {bytes_the_rows_span} \
                     bytes; {} were handed",
                    pixels.height,
                    pixels.width,
                    pixels.row_stride_bytes,
                    pixels.pixels.len()
                ),
            });
        }
        let table_pixels = ModuleHostTableHostMemoryPixels {
            pixels: ModuleHostTableBytes::of(pixels.pixels),
            pixel_format: ModuleHostTableUtf8::of(pixels.pixel_format),
            width: pixels.width,
            height: pixels.height,
            row_stride_bytes: pixels.row_stride_bytes,
        };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `import_dma_buf`; the surface is this host's.
        let status = unsafe {
            (self.module_host_table.surface_write_from_host_memory)(
                self.module_host_table.node_host,
                destination.handle,
                &table_pixels,
                &mut error_message,
            )
        };
        refuse_unless_ok("surface_write_from_host_memory", status, &error_message)
    }

    /// Read every pixel of `source`, a four-byte surface of the same extent,
    /// into `pixels`.
    pub fn read_surface_into_host_memory(
        &self,
        source: &ModuleHostSurface,
        pixels: &mut ModuleHostHostMemoryPixelsToFill<'_>,
    ) -> Result<(), ModuleHostError> {
        let rows = ModuleHostHostMemoryPixels {
            pixels: pixels.pixels,
            pixel_format: pixels.pixel_format,
            width: pixels.width,
            height: pixels.height,
            row_stride_bytes: pixels.row_stride_bytes,
        };
        let bytes_the_rows_span =
            rows.bytes_the_rows_span()
                .ok_or_else(|| ModuleHostError::VerbRefused {
                    verb: "surface_read_into_host_memory",
                    refusal: format!(
                        "{}x{} pixels {} bytes apart do not fit in memory",
                        pixels.width, pixels.height, pixels.row_stride_bytes
                    ),
                })?;
        if (pixels.pixels.len() as u64) < bytes_the_rows_span {
            return Err(ModuleHostError::VerbRefused {
                verb: "surface_read_into_host_memory",
                refusal: format!(
                    "{} rows of {} four-byte pixels, {} bytes apart, span {bytes_the_rows_span} \
                     bytes; {} were handed",
                    pixels.height,
                    pixels.width,
                    pixels.row_stride_bytes,
                    pixels.pixels.len()
                ),
            });
        }
        let table_pixels = ModuleHostTableHostMemoryPixelsToFill {
            pixels: ModuleHostTableMutableBytes {
                bytes: pixels.pixels.as_mut_ptr(),
                length: pixels.pixels.len(),
            },
            pixel_format: ModuleHostTableUtf8::of(pixels.pixel_format),
            width: pixels.width,
            height: pixels.height,
            row_stride_bytes: pixels.row_stride_bytes,
        };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `import_dma_buf`; the surface is this host's, and the
        // memory is borrowed mutably for the length of the call.
        let status = unsafe {
            (self.module_host_table.surface_read_into_host_memory)(
                self.module_host_table.node_host,
                source.handle,
                &table_pixels,
                &mut error_message,
            )
        };
        refuse_unless_ok("surface_read_into_host_memory", status, &error_message)
    }

    /// What the host offers, as one JSON object.
    pub fn host_capabilities_json(&self) -> &str {
        // SAFETY: the host keeps the text alive while the node host is
        // retained, which `self` does for at least as long as the borrow.
        unsafe {
            (self.module_host_table.host_capabilities_json)(self.module_host_table.node_host)
                .as_str()
                .unwrap_or("{}")
        }
    }
}

impl Clone for ModuleHost {
    fn clone(&self) -> Self {
        // SAFETY: `self` holds a reference, so the node host is alive.
        unsafe { (self.module_host_table.node_host_retain)(self.module_host_table.node_host) };
        Self {
            module_host_table: self.module_host_table,
        }
    }
}

impl Drop for ModuleHost {
    fn drop(&mut self) {
        // SAFETY: releases the reference this value took.
        unsafe { (self.module_host_table.node_host_release)(self.module_host_table.node_host) };
    }
}

/// A foreign DMA-BUF as [`ModuleHost::import_dma_buf`] adopts it.
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostForeignDmaBuf<'description> {
    pub fd: i32,
    pub width: u32,
    pub height: u32,
    /// A pixel format wire name, such as `nv12_video_range` or `yuyv`.
    pub pixel_format: &'description str,
    pub allocation_byte_size: u64,
    pub plane_offsets_bytes: &'description [u64],
    pub plane_strides_bytes: &'description [u64],
    /// `0`, `DRM_FORMAT_MOD_LINEAR`, for rows at the plane strides; a tiled
    /// modifier for one plane of `rgba32` or `bgra32`, imported as a texture.
    pub drm_format_modifier: u64,
}

/// A foreign IOSurface as [`ModuleHost::import_iosurface`] adopts it.
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostForeignIOSurface<'description> {
    /// An `IOSurfaceRef`, live for the length of the call.
    pub iosurface: *const std::ffi::c_void,
    /// A pixel format wire name, such as `nv12_video_range`.
    pub pixel_format: &'description str,
    pub width: u32,
    pub height: u32,
}

/// What [`ModuleHost::acquire_texture_from_node_output_pool`] takes a texture by.
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostNodeOutputTextureRequest<'description> {
    pub pool_key: &'description str,
    pub rotation_depth: u32,
    pub width: u32,
    pub height: u32,
    /// A texture format wire name, such as `rgba8_unorm`.
    pub texture_format: &'description str,
    /// Usage names, such as `storage_binding` and `texture_binding`.
    pub usage: &'description [&'description str],
}

/// A surface's H.273 colour description by axis name; `None` leaves an axis
/// to the YCbCr default.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModuleHostColorInfo<'description> {
    pub primaries: Option<&'description str>,
    pub transfer: Option<&'description str>,
    pub matrix: Option<&'description str>,
    pub range: Option<&'description str>,
}

/// Four-byte pixels in the module's memory, as
/// [`ModuleHost::write_surface_from_host_memory`] takes them.
#[derive(Debug, Clone, Copy)]
pub struct ModuleHostHostMemoryPixels<'pixels> {
    pub pixels: &'pixels [u8],
    /// A pixel format wire name: `bgra32` or `rgba32`.
    pub pixel_format: &'pixels str,
    pub width: u32,
    pub height: u32,
    /// The distance from one row's first byte to the next row's.
    pub row_stride_bytes: u64,
}

impl ModuleHostHostMemoryPixels<'_> {
    /// The bytes from the first row's first pixel to the last row's last;
    /// `None` when that overflows.
    pub fn bytes_the_rows_span(&self) -> Option<u64> {
        if self.width == 0 || self.height == 0 {
            return Some(0);
        }
        let row_bytes = u64::from(self.width).checked_mul(4)?;
        self.row_stride_bytes
            .checked_mul(u64::from(self.height) - 1)?
            .checked_add(row_bytes)
    }
}

/// The module's memory [`ModuleHost::read_surface_into_host_memory`] fills
/// with four-byte pixels.
#[derive(Debug)]
pub struct ModuleHostHostMemoryPixelsToFill<'pixels> {
    pub pixels: &'pixels mut [u8],
    /// A pixel format wire name: `bgra32` or `rgba32`.
    pub pixel_format: &'pixels str,
    pub width: u32,
    pub height: u32,
    /// The distance from one row's first byte to the next row's.
    pub row_stride_bytes: u64,
}

/// A surface lent to native code; dropping it gives it back.
#[derive(Debug)]
pub struct ModuleHostSurface {
    module_host: ModuleHost,
    handle: ModuleHostTableSurfaceHandle,
}

impl ModuleHostSurface {
    /// The surface id a message names this surface by.
    pub fn surface_id(&self) -> &str {
        let table = &self.module_host.module_host_table;
        // SAFETY: the host keeps the text alive until this surface is
        // released, which drop does after every borrow of `self` ends.
        unsafe {
            (table.surface_id)(table.node_host, self.handle)
                .as_str()
                .unwrap_or("")
        }
    }
}

impl ModuleHostSurface {
    /// The GPU pool slot frame descriptor of this texture, acquired from the
    /// node's output pool: what an Image frame written of it names its slot
    /// with.
    pub fn gpu_pool_slot_frame(&self) -> Result<ModuleHostTablePayloadDescriptor, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut gpu_pool_slot_frame = ModuleHostTablePayloadDescriptor::bytes();
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained by `self.module_host`; the
        // descriptor and the message buffer are live for the call.
        let status = unsafe {
            (table.surface_gpu_pool_slot_frame)(
                table.node_host,
                self.handle,
                &mut gpu_pool_slot_frame,
                &mut error_message,
            )
        };
        refuse_unless_ok("surface_gpu_pool_slot_frame", status, &error_message)?;
        Ok(gpu_pool_slot_frame)
    }
}

impl Drop for ModuleHostSurface {
    fn drop(&mut self) {
        let table = &self.module_host.module_host_table;
        // SAFETY: gives back the surface this value holds, once.
        unsafe { (table.surface_release)(table.node_host, self.handle) };
    }
}

/// An output port Python lent to native code.
#[derive(Debug, Clone)]
pub struct ModuleHostOutputPort {
    module_host: ModuleHost,
    port_name: String,
    handle: ModuleHostTablePortHandle,
}

impl ModuleHostOutputPort {
    /// The port's declared name.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// Publish `payload` to every downstream link, stamped `timestamp_ns` or,
    /// with `None`, its stream's clock now.
    pub fn write(
        &self,
        payload: &[u8],
        timestamp_ns: Option<i64>,
        starts_group: bool,
    ) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained by `self.module_host`; the
        // payload and the message buffer are live for the call.
        let status = unsafe {
            (table.output_port_write)(
                table.node_host,
                self.handle,
                ModuleHostTableBytes::of(payload),
                timestamp_ns.unwrap_or(MODULE_HOST_TABLE_TIMESTAMP_NOW),
                u32::from(starts_group),
                &mut error_message,
            )
        };
        refuse_unless_ok("output_port_write", status, &error_message)
    }

    /// Publish `payload` described by `payload_descriptor` to every
    /// downstream link, stamped as [`Self::write`] stamps.
    pub fn write_described(
        &self,
        payload: &[u8],
        timestamp_ns: Option<i64>,
        starts_group: bool,
        payload_descriptor: &ModuleHostTablePayloadDescriptor,
    ) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `write`; the descriptor is live for the call.
        let status = unsafe {
            (table.output_port_write_described)(
                table.node_host,
                self.handle,
                ModuleHostTableBytes::of(payload),
                timestamp_ns.unwrap_or(MODULE_HOST_TABLE_TIMESTAMP_NOW),
                u32::from(starts_group),
                payload_descriptor,
                &mut error_message,
            )
        };
        refuse_unless_ok("output_port_write_described", status, &error_message)
    }

    /// Declare `given_up_group_count` groups given up before this port's next
    /// write; its readers count them as lost since their last read.
    pub fn declare_groups_given_up_before_the_next_write(
        &self,
        given_up_group_count: u64,
    ) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `write`.
        let status = unsafe {
            (table.output_port_declare_groups_given_up_before_the_next_write)(
                table.node_host,
                self.handle,
                given_up_group_count,
                &mut error_message,
            )
        };
        refuse_unless_ok(
            "output_port_declare_groups_given_up_before_the_next_write",
            status,
            &error_message,
        )
    }

    /// Replace the port's stream facts: `family` is `image` or `packets`, the
    /// port's own, and `stream_facts_json` the facts' fields as one JSON object.
    pub fn set_stream_facts(
        &self,
        family: &str,
        stream_facts_json: &str,
    ) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `write`.
        let status = unsafe {
            (table.output_port_set_stream_facts)(
                table.node_host,
                self.handle,
                ModuleHostTableUtf8::of(family),
                ModuleHostTableUtf8::of(stream_facts_json),
                &mut error_message,
            )
        };
        refuse_unless_ok("output_port_set_stream_facts", status, &error_message)
    }
}

/// An input port Python lent to native code.
#[derive(Debug, Clone)]
pub struct ModuleHostInputPort {
    module_host: ModuleHost,
    port_name: String,
    handle: ModuleHostTablePortHandle,
}

impl ModuleHostInputPort {
    /// The port's declared name.
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// Take the port's next message, or `None` when its mailbox is empty.
    pub fn read(&self) -> Result<Option<ModuleHostReadMessage>, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut read_message = ModuleHostTableReadMessage::empty();
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `ModuleHostOutputPort::write`.
        let status = unsafe {
            (table.input_port_read)(
                table.node_host,
                self.handle,
                &mut read_message,
                &mut error_message,
            )
        };
        if status == MODULE_HOST_TABLE_STATUS_EMPTY {
            return Ok(None);
        }
        refuse_unless_ok("input_port_read", status, &error_message)?;
        Ok(Some(ModuleHostReadMessage {
            module_host: self.module_host.clone(),
            read_message,
        }))
    }
}

/// One message read off an input port; the payload is handed back to the host
/// when this drops.
#[derive(Debug)]
pub struct ModuleHostReadMessage {
    module_host: ModuleHost,
    read_message: ModuleHostTableReadMessage,
}

// SAFETY: the host owns the payload until release, which any thread may call.
unsafe impl Send for ModuleHostReadMessage {}

impl ModuleHostReadMessage {
    /// The payload exactly as the producer wrote it.
    pub fn payload(&self) -> &[u8] {
        // SAFETY: the host keeps the payload alive until this message drops.
        unsafe {
            ModuleHostTableBytes {
                bytes: self.read_message.payload,
                length: self.read_message.payload_length,
            }
            .as_slice()
        }
    }

    /// The producer's stamp, on the engine's monotonic clock.
    pub fn timestamp_ns(&self) -> i64 {
        self.read_message.timestamp_ns
    }

    /// Whether the message opens a group.
    pub fn starts_group(&self) -> bool {
        self.read_message.starts_group != 0
    }

    /// The messages this one's inbound link lost since the read before it.
    pub fn lost_since_last_read(&self) -> u64 {
        self.read_message.lost_since_last_read
    }

    /// What the payload is: its kind and that kind's body.
    pub fn payload_descriptor(&self) -> Result<ModuleHostTablePayloadDescriptor, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut payload_descriptor = ModuleHostTablePayloadDescriptor::bytes();
        // SAFETY: the host owns the message until this value drops.
        let status = unsafe {
            (table.input_message_payload_descriptor)(
                table.node_host,
                &self.read_message,
                &mut payload_descriptor,
            )
        };
        refuse_unless_ok(
            "input_message_payload_descriptor",
            status,
            &ModuleHostTableErrorMessage::empty(),
        )?;
        Ok(payload_descriptor)
    }

    /// Lend native code the pooled GPU frame this message's Image frame or
    /// GPU pool slot frame names, held against its producer's reuse until the
    /// lent surface drops.
    pub fn lend_pooled_gpu_frame(&self) -> Result<ModuleHostSurface, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut lent_surface = ModuleHostTableSurfaceHandle { value: 0 };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: as in `payload_descriptor`; the slots are live for the call.
        let status = unsafe {
            (table.input_message_lend_pooled_gpu_frame)(
                table.node_host,
                &self.read_message,
                &mut lent_surface,
                &mut error_message,
            )
        };
        refuse_unless_ok(
            "input_message_lend_pooled_gpu_frame",
            status,
            &error_message,
        )?;
        Ok(ModuleHostSurface {
            module_host: self.module_host.clone(),
            handle: lent_surface,
        })
    }
}

impl Drop for ModuleHostReadMessage {
    fn drop(&mut self) {
        let table = &self.module_host.module_host_table;
        // SAFETY: hands back the message this value took, once.
        unsafe { (table.input_message_release)(table.node_host, &mut self.read_message) };
    }
}

pub(crate) fn refuse_unless_ok(
    verb: &'static str,
    status: ModuleHostTableStatus,
    error_message: &ModuleHostTableErrorMessage,
) -> Result<(), ModuleHostError> {
    if status == MODULE_HOST_TABLE_STATUS_OK {
        return Ok(());
    }
    let refusal = error_message.message();
    Err(ModuleHostError::VerbRefused {
        verb,
        refusal: if refusal.is_empty() {
            format!("refused with status {status} and no message")
        } else {
            refusal
        },
    })
}
