// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The virtual camera's PipeWire door: a `Video/Source` node with
//! `media.role = Camera` that any portal-based application can select.
//!
//! The node exists from `open` to `close` whether or not anything ever watches
//! it — a camera in the picker with nobody looking — and needs no kernel
//! module and no root.
//!
//! libpipewire is reached through the process's one `dlsym`'d table
//! ([`crate::linux::pipewire_runtime_library`]) and SPA's pod builders through
//! `pipewire_video_source_shim.c`, so nothing here links a video library.
//!
//! The camera offers YUY2 in shared memory it owns, and each frame arrives as
//! host rows a caller already converted, copied once into the slot PipeWire
//! hands out.

use std::ffi::{CStr, CString};
use std::sync::Arc;

use crate::core::{Error, Result};
use crate::linux::pipewire_runtime_library::{PipeWireLibraryEntryPoints, ShimFailureText};

/// How many buffers a camera node offers: what the loopback door's readers ask
/// for and what Chromium's enumerator expects.
const CAMERA_BUFFER_COUNT: u32 = 4;

/// The frame interval offered when a frame carries no rate of its own. It is a
/// hint on a source that publishes when it has a picture, not a promise.
const DEFAULT_FRAMES_PER_SECOND: u32 = 30;

/// Bytes one YUY2 pixel occupies.
const YUYV_BYTES_PER_PIXEL: usize = 2;

mod video_shim {
    use std::ffi::{c_char, c_int, c_void};

    /// Mirrors `struct StreamLibPipeWireVideoSource`, which is opaque here.
    #[repr(C)]
    pub struct VideoSource {
        _opaque: [u8; 0],
    }

    unsafe extern "C" {
        pub fn streamlib_pipewire_video_source_open(
            entry_points: *const *mut c_void,
            camera_name: *const c_char,
            failure_text: *mut c_char,
            failure_text_capacity: usize,
        ) -> *mut VideoSource;
        pub fn streamlib_pipewire_video_source_set_extent(
            video_source: *mut VideoSource,
            width: u32,
            height: u32,
            framerate_numerator: u32,
            framerate_denominator: u32,
            buffer_count: u32,
            failure_text: *mut c_char,
            failure_text_capacity: usize,
        ) -> c_int;
        pub fn streamlib_pipewire_video_source_consumer_negotiated(
            video_source: *mut VideoSource,
        ) -> bool;
        pub fn streamlib_pipewire_video_source_dequeue_slot(video_source: *mut VideoSource) -> i32;
        pub fn streamlib_pipewire_video_source_slot_shared_memory(
            video_source: *mut VideoSource,
            slot: i32,
            stride_bytes_out: *mut u32,
            byte_size_out: *mut u32,
        ) -> *mut u8;
        pub fn streamlib_pipewire_video_source_queue_slot(
            video_source: *mut VideoSource,
            slot: i32,
            timestamp_ns: i64,
            sequence: u64,
        ) -> c_int;
        pub fn streamlib_pipewire_video_source_failure(
            video_source: *mut VideoSource,
        ) -> *const c_char;
        pub fn streamlib_pipewire_video_source_close(video_source: *mut VideoSource);
    }

    /// Mirrors `struct StreamLibPipeWireVideoOfferedFormat`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    #[cfg(test)]
    pub struct OfferedFormat {
        pub width: u32,
        pub height: u32,
        pub framerate_numerator: u32,
        pub framerate_denominator: u32,
    }

    /// Mirrors `struct StreamLibPipeWireVideoOfferReport`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    #[cfg(test)]
    pub struct OfferReport {
        pub width: u32,
        pub height: u32,
        pub video_format: u32,
        pub carries_a_modifier: bool,
        pub was_built: bool,
    }

    // The `node.name` a camera registers under, the property dict it announces
    // itself with, and what its offered format actually says. The shim calls
    // the first two on every open; Rust only ever calls them to hold the
    // composition in a test, which is why they are declared there.
    #[cfg(test)]
    unsafe extern "C" {
        pub fn streamlib_pipewire_video_source_describe_offer(
            offered_format: *const OfferedFormat,
        ) -> OfferReport;
        pub fn streamlib_pipewire_video_source_offered_video_format() -> u32;
        pub fn streamlib_pipewire_video_source_node_name(
            camera_name: *const c_char,
            node_name: *mut c_char,
            node_name_capacity: usize,
        ) -> usize;
        pub fn streamlib_pipewire_video_source_properties(
            items: *mut StreamProperty,
            item_capacity: u32,
            camera_name: *const c_char,
            node_name: *mut c_char,
            node_name_capacity: usize,
        ) -> u32;
    }

    /// Mirrors `struct StreamLibPipeWireStreamProperty`.
    #[repr(C)]
    #[derive(Clone, Copy)]
    #[cfg(test)]
    pub struct StreamProperty {
        pub key: *const c_char,
        pub value: *const c_char,
    }
}

/// What became of one frame handed to the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeWireCameraFramePresentation {
    /// The frame is on its way to a consumer.
    Published,
    /// No application has the camera open, so the frame is dropped — which is
    /// what a camera nobody is watching does with its pictures.
    NoConsumerIsWatching,
    /// Every buffer is still with the consumer, so this frame is dropped
    /// rather than waited on.
    EveryBufferIsHeldByTheConsumer,
}

/// The shim-owned source, held as its own type so that `Send` is promised for
/// the pointer alone.
struct ShimOwnedVideoSourcePointer(*mut video_shim::VideoSource);

// The source is owned by the value that holds this pointer, and every shim
// entry point takes PipeWire's thread-loop lock, so it is only ever used from
// whichever thread holds that value.
unsafe impl Send for ShimOwnedVideoSourcePointer {}

/// A `Video/Source` node, alive for as long as this value is.
pub struct PipeWireCameraNode {
    /// Kept solely so the resolved entry points outlive the shim's use of them.
    _entry_points: Arc<PipeWireLibraryEntryPoints>,
    video_source: ShimOwnedVideoSourcePointer,
    camera_name: String,
    offered_extent: Option<(u32, u32)>,
    published_frame_count: u64,
}

impl PipeWireCameraNode {
    /// Register a camera node named `camera_name`.
    ///
    /// Refuses by name when no libpipewire loads or no session daemon answers —
    /// the same probe-by-opening the audio chain uses, because a library
    /// present with no daemon behind it is the ordinary container case.
    pub fn open(camera_name: &str) -> Result<Self> {
        let entry_points = PipeWireLibraryEntryPoints::loaded_once_per_process()
            .map_err(|reason| Self::no_door(camera_name, &reason.to_string()))?;
        entry_points
            .daemon_answers()
            .map_err(|reason| Self::no_door(camera_name, &reason))?;

        let camera_name_c = CString::new(camera_name).map_err(|_| {
            Error::Runtime(format!(
                "VirtualCameraSink \"{camera_name}\": a camera name cannot contain a NUL byte"
            ))
        })?;
        let mut failure_text = ShimFailureText::new();
        let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
        // SAFETY: the table is fully resolved, the name is NUL-terminated and
        // outlives the call, and the out-buffer's pointer and capacity come
        // from the buffer itself.
        let video_source = unsafe {
            video_shim::streamlib_pipewire_video_source_open(
                entry_points.as_ptr(),
                camera_name_c.as_ptr(),
                failure_text_ptr,
                failure_text_capacity,
            )
        };
        if video_source.is_null() {
            return Err(Self::no_door(camera_name, &failure_text.read()));
        }
        Ok(Self {
            _entry_points: Arc::clone(entry_points),
            video_source: ShimOwnedVideoSourcePointer(video_source),
            camera_name: camera_name.to_string(),
            offered_extent: None,
            published_frame_count: 0,
        })
    }

    fn no_door(camera_name: &str, reason: &str) -> Error {
        Error::Runtime(format!(
            "VirtualCameraSink \"{camera_name}\": the PipeWire camera door is not available: \
             {reason}"
        ))
    }

    /// Offer one extent as YUY2 shared-memory buffers. Replaces any extent
    /// offered earlier; consumers re-negotiate.
    pub fn offer_extent(
        &mut self,
        width: u32,
        height: u32,
        frames_per_second: Option<u32>,
    ) -> Result<()> {
        if !width.is_multiple_of(2) {
            return Err(Error::Runtime(format!(
                "VirtualCameraSink \"{}\": a {width}x{height} frame has an odd width, and YUY2 \
                 packs two pixels into each four bytes",
                self.camera_name
            )));
        }
        let mut failure_text = ShimFailureText::new();
        let (failure_text_ptr, failure_text_capacity) = failure_text.as_shim_out_parameters();
        // SAFETY: the source is live for as long as this value is.
        let taken = unsafe {
            video_shim::streamlib_pipewire_video_source_set_extent(
                self.video_source.0,
                width,
                height,
                frames_per_second
                    .filter(|&rate| rate > 0)
                    .unwrap_or(DEFAULT_FRAMES_PER_SECOND),
                1,
                CAMERA_BUFFER_COUNT,
                failure_text_ptr,
                failure_text_capacity,
            )
        };
        if taken != 0 {
            return Err(Error::Runtime(format!(
                "VirtualCameraSink \"{}\": PipeWire would not take a {width}x{height} camera \
                 format: {}",
                self.camera_name,
                failure_text.read()
            )));
        }
        self.offered_extent = Some((width, height));
        tracing::info!(
            camera = %self.camera_name,
            width,
            height,
            buffers = CAMERA_BUFFER_COUNT,
            "VirtualCameraSink: the PipeWire camera offers this extent"
        );
        Ok(())
    }

    /// The extent currently offered, if one is.
    pub fn offered_extent(&self) -> Option<(u32, u32)> {
        self.offered_extent
    }

    /// Whether a consumer has settled a format.
    pub fn consumer_negotiated(&self) -> bool {
        // SAFETY: the source is live for as long as this value is.
        unsafe {
            video_shim::streamlib_pipewire_video_source_consumer_negotiated(self.video_source.0)
        }
    }

    /// The reason the node failed after it was registered, if it has.
    pub fn failure(&self) -> Option<String> {
        // SAFETY: the source is live. The text is written once and never
        // rewritten — `record_stream_failure` returns early on an already
        // failed source — so the pointer names an immutable buffer.
        unsafe {
            let failure = video_shim::streamlib_pipewire_video_source_failure(self.video_source.0);
            if failure.is_null() {
                None
            } else {
                Some(CStr::from_ptr(failure).to_string_lossy().into_owned())
            }
        }
    }

    /// Copy one YUY2 picture of the offered extent into the next free buffer
    /// and publish it, stamped `timestamp_ns`. `yuyv_rows` holds `height` rows
    /// `source_stride_bytes` apart, each at least `width * 2` bytes.
    pub fn write_yuyv_frame(
        &mut self,
        yuyv_rows: &[u8],
        source_stride_bytes: usize,
        timestamp_ns: i64,
    ) -> Result<PipeWireCameraFramePresentation> {
        if let Some(reason) = self.failure() {
            return Err(Error::Runtime(format!(
                "VirtualCameraSink \"{}\": {reason}",
                self.camera_name
            )));
        }
        let Some((width, height)) = self.offered_extent else {
            return Ok(PipeWireCameraFramePresentation::NoConsumerIsWatching);
        };
        let row_bytes = width as usize * YUYV_BYTES_PER_PIXEL;
        let rows_needed = (height as usize)
            .checked_sub(1)
            .and_then(|last_row| last_row.checked_mul(source_stride_bytes))
            .and_then(|last_row_start| last_row_start.checked_add(row_bytes));
        if source_stride_bytes < row_bytes
            || rows_needed.is_none_or(|needed| needed > yuyv_rows.len())
        {
            return Err(Error::Runtime(format!(
                "VirtualCameraSink \"{}\": a {width}x{height} YUY2 picture needs rows of {row_bytes} \
                 bytes, {source_stride_bytes} apart, and {} bytes were handed over",
                self.camera_name,
                yuyv_rows.len()
            )));
        }

        // SAFETY: the source is live; a negative answer means no buffer, and a
        // non-negative one is a slot the shim now holds for this caller.
        let slot = unsafe {
            video_shim::streamlib_pipewire_video_source_dequeue_slot(self.video_source.0)
        };
        if slot < 0 {
            return Ok(if self.consumer_negotiated() {
                PipeWireCameraFramePresentation::EveryBufferIsHeldByTheConsumer
            } else {
                PipeWireCameraFramePresentation::NoConsumerIsWatching
            });
        }

        let mut destination_stride_bytes = 0_u32;
        let mut destination_byte_size = 0_u32;
        // SAFETY: the source is live and the two out-parameters are this
        // frame's own locals.
        let mapping = unsafe {
            video_shim::streamlib_pipewire_video_source_slot_shared_memory(
                self.video_source.0,
                slot,
                &mut destination_stride_bytes,
                &mut destination_byte_size,
            )
        };
        let destination_stride_bytes = destination_stride_bytes as usize;
        if mapping.is_null()
            || destination_stride_bytes < row_bytes
            || destination_stride_bytes * height as usize > destination_byte_size as usize
        {
            return Err(Error::Runtime(format!(
                "VirtualCameraSink \"{}\": PipeWire's buffer slot {slot} maps \
                 {destination_byte_size} bytes at a stride of {destination_stride_bytes}, short \
                 of a {width}x{height} YUY2 picture",
                self.camera_name
            )));
        }
        // SAFETY: the shim maps `destination_byte_size` bytes at `mapping` for
        // the offered extent's whole life, and the check above bounds every
        // row written below inside it.
        let destination =
            unsafe { std::slice::from_raw_parts_mut(mapping, destination_byte_size as usize) };
        for row in 0..height as usize {
            let source_row_start = row * source_stride_bytes;
            let destination_row_start = row * destination_stride_bytes;
            destination[destination_row_start..destination_row_start + row_bytes]
                .copy_from_slice(&yuyv_rows[source_row_start..source_row_start + row_bytes]);
        }

        // SAFETY: the source is live and `slot` is the one this call dequeued.
        let queued = unsafe {
            video_shim::streamlib_pipewire_video_source_queue_slot(
                self.video_source.0,
                slot,
                timestamp_ns,
                self.published_frame_count,
            )
        };
        if queued != 0 {
            return Err(Error::Runtime(format!(
                "VirtualCameraSink \"{}\": PipeWire refused buffer slot {slot}",
                self.camera_name
            )));
        }
        self.published_frame_count += 1;
        Ok(PipeWireCameraFramePresentation::Published)
    }
}

impl Drop for PipeWireCameraNode {
    fn drop(&mut self) {
        // SAFETY: the source was produced by `open` and is closed exactly once.
        unsafe { video_shim::streamlib_pipewire_video_source_close(self.video_source.0) };
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::c_char;

    use super::*;

    fn node_name_of(camera_name: &str) -> String {
        let camera_name = CString::new(camera_name).expect("no NUL in a test name");
        let mut node_name = [0_u8; 128];
        let written = unsafe {
            video_shim::streamlib_pipewire_video_source_node_name(
                camera_name.as_ptr(),
                node_name.as_mut_ptr().cast::<c_char>(),
                node_name.len(),
            )
        };
        String::from_utf8(node_name[..written].to_vec()).expect("an ASCII identifier")
    }

    /// `node.name` is an identifier a session manager and every `pw-dump`
    /// reader index by, so a camera called "Desk cam" has to reduce to one
    /// rather than carry a space into the graph.
    #[test]
    fn a_camera_name_reduces_to_a_pipewire_identifier() {
        assert_eq!(node_name_of("Desk cam"), "streamlib-camera-desk-cam");
        assert_eq!(
            node_name_of("StreamLib Camera 4f2a"),
            "streamlib-camera-streamlib-camera-4f2a"
        );
    }

    /// Runs of punctuation collapse and trailing ones are dropped, so two names
    /// that differ only in padding do not become two different node names.
    #[test]
    fn punctuation_collapses_rather_than_repeating() {
        assert_eq!(node_name_of("Desk  cam!!"), "streamlib-camera-desk-cam");
        assert_eq!(node_name_of("Desk-cam"), "streamlib-camera-desk-cam");
        assert_eq!(node_name_of("!!!"), "streamlib-camera");
    }

    /// The two properties WirePlumber's portal access rule keys on. Without
    /// both, the node is one no portal-based picker will ever list — a camera
    /// that exists in `pw-dump` and nowhere a user can see.
    ///
    /// Mental revert: drop `media.role` and this fails.
    #[test]
    fn a_camera_node_announces_the_class_and_role_a_portal_grants() {
        let camera_name = CString::new("Desk cam").expect("no NUL");
        let mut node_name = [0_u8; 128];
        let mut items = [video_shim::StreamProperty {
            key: std::ptr::null(),
            value: std::ptr::null(),
        }; 6];
        let count = unsafe {
            video_shim::streamlib_pipewire_video_source_properties(
                items.as_mut_ptr(),
                items.len() as u32,
                camera_name.as_ptr(),
                node_name.as_mut_ptr().cast::<c_char>(),
                node_name.len(),
            )
        };
        assert_eq!(count, 6, "every property the portal rule and a picker read");

        let announced: Vec<(String, String)> = items[..count as usize]
            .iter()
            .map(|item| unsafe {
                (
                    CStr::from_ptr(item.key).to_string_lossy().into_owned(),
                    CStr::from_ptr(item.value).to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert!(announced.contains(&("media.class".to_string(), "Video/Source".to_string())));
        assert!(announced.contains(&("media.role".to_string(), "Camera".to_string())));
        assert!(
            announced.contains(&("node.description".to_string(), "Desk cam".to_string())),
            "the name a picker shows is the camera's own: {announced:?}"
        );
        assert!(
            announced.contains(&(
                "node.name".to_string(),
                "streamlib-camera-desk-cam".to_string()
            )),
            "the identifier is the reduced name: {announced:?}"
        );
    }

    /// The offer is raw YUY2 with no modifier property: a shared-memory offer
    /// a consumer cannot mistake for a DMA-BUF one, in the byte order the
    /// loopback door writes.
    ///
    /// Mental revert: offer `RGBA` and a consumer reads every YUY2 frame as a
    /// half-width colour smear.
    #[test]
    fn a_pipewire_camera_node_offers_yuy2_in_shared_memory() {
        let offered_format = video_shim::OfferedFormat {
            width: 1280,
            height: 720,
            framerate_numerator: 30,
            framerate_denominator: 1,
        };
        let report =
            unsafe { video_shim::streamlib_pipewire_video_source_describe_offer(&offered_format) };
        assert!(
            report.was_built,
            "the pod builds and parses back: {report:?}"
        );
        assert_eq!((report.width, report.height), (1280, 720));
        assert_eq!(
            report.video_format,
            unsafe { video_shim::streamlib_pipewire_video_source_offered_video_format() },
            "{report:?}"
        );
        assert!(!report.carries_a_modifier, "{report:?}");
    }
}
