// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Safe Rust over the table's virtual camera verbs: a PipeWire camera node the
//! host registers in this process, fed one YUY2 picture at a time.

use crate::module_host::{ModuleHost, ModuleHostError, refuse_unless_ok};
use crate::module_host_table_abi::{
    MODULE_HOST_TABLE_STATUS_ENDED,
    MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_EVERY_BUFFER_IS_HELD_BY_THE_CONSUMER,
    MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_NO_CONSUMER_IS_WATCHING,
    MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_PUBLISHED, ModuleHostTableBytes,
    ModuleHostTableErrorMessage, ModuleHostTableUtf8, ModuleHostTableVirtualCameraStreamHandle,
};

/// What became of one frame handed to a virtual camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleHostVirtualCameraFramePresentation {
    /// The frame is on its way to a consumer.
    Published,
    /// No application has the camera open, so the frame was dropped.
    NoConsumerIsWatching,
    /// Every buffer is still with the consumer, so the frame was dropped.
    EveryBufferIsHeldByTheConsumer,
}

impl ModuleHostVirtualCameraFramePresentation {
    /// The presentation's name as a log field.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::NoConsumerIsWatching => "no_consumer_is_watching",
            Self::EveryBufferIsHeldByTheConsumer => "every_buffer_is_held_by_the_consumer",
        }
    }

    fn of_module_host_table_frame_presentation(frame_presentation: u32) -> Option<Self> {
        match frame_presentation {
            MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_PUBLISHED => Some(Self::Published),
            MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_NO_CONSUMER_IS_WATCHING => {
                Some(Self::NoConsumerIsWatching)
            }
            MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_EVERY_BUFFER_IS_HELD_BY_THE_CONSUMER => {
                Some(Self::EveryBufferIsHeldByTheConsumer)
            }
            _ => None,
        }
    }
}

/// A PipeWire camera node the host registered for this module; removed when it
/// drops.
#[derive(Debug)]
pub struct ModuleHostVirtualCameraStream {
    module_host: ModuleHost,
    handle: ModuleHostTableVirtualCameraStreamHandle,
}

impl ModuleHost {
    /// Register a PipeWire `Video/Source` camera node named `camera_name`.
    ///
    /// Refused by name where no libpipewire loads, no PipeWire daemon answers,
    /// or the platform is not Linux.
    pub fn open_virtual_camera_stream(
        &self,
        camera_name: &str,
    ) -> Result<ModuleHostVirtualCameraStream, ModuleHostError> {
        let table = &self.module_host_table;
        let mut handle = ModuleHostTableVirtualCameraStreamHandle { value: 0 };
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained by `self`, and every pointer is to
        // a live local for the length of the call.
        let status = unsafe {
            (table.virtual_camera_stream_open)(
                table.node_host,
                ModuleHostTableUtf8::of(camera_name),
                &mut handle,
                &mut error_message,
            )
        };
        refuse_unless_ok("virtual_camera_stream_open", status, &error_message)?;
        Ok(ModuleHostVirtualCameraStream {
            module_host: self.clone(),
            handle,
        })
    }
}

impl ModuleHostVirtualCameraStream {
    /// Offer one even-width extent as YUY2 frames, at `frames_per_second` or
    /// the camera's default; consumers re-negotiate.
    pub fn offer_extent(
        &self,
        width: u32,
        height: u32,
        frames_per_second: Option<u32>,
    ) -> Result<(), ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained; the buffer is live for the call.
        let status = unsafe {
            (table.virtual_camera_stream_offer_extent)(
                table.node_host,
                self.handle,
                width,
                height,
                frames_per_second.unwrap_or(0),
                &mut error_message,
            )
        };
        refuse_unless_ok("virtual_camera_stream_offer_extent", status, &error_message)
    }

    /// Copy one YUY2 picture of the offered extent, `height` rows
    /// `stride_bytes` apart, into the camera's next free buffer, stamped
    /// `timestamp_ns`.
    pub fn write_yuyv_frame(
        &self,
        yuyv_rows: &[u8],
        stride_bytes: u32,
        timestamp_ns: i64,
    ) -> Result<ModuleHostVirtualCameraFramePresentation, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut frame_presentation = MODULE_HOST_TABLE_VIRTUAL_CAMERA_FRAME_PUBLISHED;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained; the rows and both out-slots are
        // live for the call.
        let status = unsafe {
            (table.virtual_camera_stream_write_yuyv_frame)(
                table.node_host,
                self.handle,
                ModuleHostTableBytes::of(yuyv_rows),
                stride_bytes,
                timestamp_ns,
                &mut frame_presentation,
                &mut error_message,
            )
        };
        refuse_unless_ok(
            "virtual_camera_stream_write_yuyv_frame",
            status,
            &error_message,
        )?;
        ModuleHostVirtualCameraFramePresentation::of_module_host_table_frame_presentation(
            frame_presentation,
        )
        .ok_or_else(|| ModuleHostError::VerbRefused {
            verb: "virtual_camera_stream_write_yuyv_frame",
            refusal: format!(
                "the host reported frame presentation {frame_presentation}, which this module \
                 does not know"
            ),
        })
    }

    /// Why PipeWire dropped the camera node, or `None` while it lives.
    pub fn failure_that_ended_the_stream(&self) -> Result<Option<String>, ModuleHostError> {
        let table = &self.module_host.module_host_table;
        let mut error_message = ModuleHostTableErrorMessage::empty();
        // SAFETY: the node host is retained; the buffer is live for the call.
        let status = unsafe {
            (table.virtual_camera_stream_liveness)(table.node_host, self.handle, &mut error_message)
        };
        if status == MODULE_HOST_TABLE_STATUS_ENDED {
            return Ok(Some(error_message.message()));
        }
        refuse_unless_ok("virtual_camera_stream_liveness", status, &error_message)?;
        Ok(None)
    }
}

impl Drop for ModuleHostVirtualCameraStream {
    fn drop(&mut self) {
        let table = &self.module_host.module_host_table;
        // SAFETY: closes the camera this value opened, once.
        unsafe { (table.virtual_camera_stream_close)(table.node_host, self.handle) };
    }
}
