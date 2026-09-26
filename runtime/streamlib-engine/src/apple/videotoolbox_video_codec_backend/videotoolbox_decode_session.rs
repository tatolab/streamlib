// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One VideoToolbox decompression session behind the seam's decode shape.
//!
//! The session is built from the stream's parameter sets, so it opens at the
//! first sync point and rebuilds when they change. Pictures come back on
//! IOSurfaces and land in the pool through
//! [`Biplanar420IOSurfaceToPooledRgbaConversion`], read from the top-left of
//! the clean aperture — the parameter sets' conformance window.

use std::ffi::{c_int, c_void};
use std::ptr::NonNull;

use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, CMVideoFormatDescriptionGetDimensions,
    kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_core_video::{
    CVImageBuffer, CVImageBufferGetCleanRect, CVPixelBuffer, CVPixelBufferGetIOSurface,
    CVPixelBufferGetPixelFormatType, kCVPixelBufferIOSurfacePropertiesKey,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
};
use parking_lot::Mutex;

use super::annex_b_and_length_prefixed_nal_units::{
    LENGTH_PREFIX_BYTES_OF_SAMPLES_HANDED_TO_VIDEOTOOLBOX, parameter_sets_are_complete,
    sort_annex_b_access_unit_for_videotoolbox,
};
use super::{cf_dictionary_of_booleans, videotoolbox_call_failure};
use crate::apple::biplanar_420_iosurface_to_pooled_rgba_conversion::Biplanar420IOSurfaceToPooledRgbaConversion;
use crate::apple::core_video_pixel_buffer_color::core_video_pixel_buffer_color_to_h273_color_vui;
use crate::apple::core_video_pixel_format_dictionary::ensure_core_video_pixel_format_dictionary_is_initialised;
use crate::core::color::{ColorSpaceKind, H273ColorVui};
use crate::core::context::{
    DecodedVideoPictureInPooledPixelBuffer, GpuContextLimitedAccess, VideoCodecElementaryStream,
    VideoDecodeMaximumCodedExtent, VideoDecodeSession, VideoDecodeSessionRequest,
};
use crate::core::rhi::PixelFormat;
use crate::core::{Error, Result};

/// What the output callback hands the session: every image buffer decoded
/// since the last collection, or the failure a frame answered.
#[derive(Default)]
struct DecodedImageBuffersAwaitingCollection {
    decoded: Mutex<Vec<Result<CFRetained<CVPixelBuffer>>>>,
}

/// A decompression session and the format description it was built from.
struct DecompressionSessionForOneFormat {
    decompression_session: CFRetained<VTDecompressionSession>,
    format_description: CFRetained<CMFormatDescription>,
}

impl Drop for DecompressionSessionForOneFormat {
    fn drop(&mut self) {
        // SAFETY: a live session. Waiting first lets every frame in flight
        // reach the callback before the refcon's owner can drop; after the
        // invalidate no callback runs.
        unsafe {
            self.decompression_session.wait_for_asynchronous_frames();
            self.decompression_session.invalidate();
        }
    }
}

/// The conversion into the pool and the shape it was built for.
struct PooledRgbaConversionForOneShape {
    conversion: Biplanar420IOSurfaceToPooledRgbaConversion,
    source_pixel_format: PixelFormat,
    picture_extent: (u32, u32),
}

/// A VideoToolbox decompression session fed one Annex-B access unit at a
/// time.
pub(super) struct VideoToolboxDecodeSession {
    elementary_stream: VideoCodecElementaryStream,
    maximum_coded_extent: Option<VideoDecodeMaximumCodedExtent>,
    /// The stream's parameter sets, one per `nal_unit_type`, as last carried.
    parameter_sets: Vec<Vec<u8>>,
    /// Declared before `awaiting_collection` so it drops — and stops calling
    /// back — first.
    session: Option<DecompressionSessionForOneFormat>,
    /// The output callback's refcon, boxed so its address is stable.
    awaiting_collection: Box<DecodedImageBuffersAwaitingCollection>,
    pooled_rgba_conversion: Option<PooledRgbaConversionForOneShape>,
    last_decoded_color_vui: Option<H273ColorVui>,
    /// Acquires the pooled pixel buffers pictures land in.
    gpu_context: GpuContextLimitedAccess,
}

// SAFETY: a decompression session may be driven from any thread, one call at
// a time — which `&mut self` on every call guarantees — and the callback
// state is behind a mutex.
unsafe impl Send for VideoToolboxDecodeSession {}

impl VideoToolboxDecodeSession {
    /// A session with nothing decoded yet; the VideoToolbox session itself
    /// opens at the stream's first parameter sets.
    pub(super) fn open(
        gpu_context: GpuContextLimitedAccess,
        request: &VideoDecodeSessionRequest,
    ) -> Result<Self> {
        Ok(Self {
            elementary_stream: request.elementary_stream,
            maximum_coded_extent: request.maximum_coded_extent,
            parameter_sets: Vec::new(),
            session: None,
            awaiting_collection: Box::default(),
            pooled_rgba_conversion: None,
            last_decoded_color_vui: None,
            gpu_context,
        })
    }

    /// Take in the access unit's parameter sets, replacing any of the same
    /// type, and drop a session they no longer describe.
    fn take_in_parameter_sets(&mut self, carried_parameter_sets: Vec<Vec<u8>>) {
        let mut changed = false;
        for carried in carried_parameter_sets {
            let carried_type = carried.first().copied();
            match self.parameter_sets.iter_mut().find(|held| {
                nal_unit_type_byte_matches(
                    self.elementary_stream,
                    held.first().copied(),
                    carried_type,
                )
            }) {
                Some(held) if *held == carried => {}
                Some(held) => {
                    *held = carried;
                    changed = true;
                }
                None => {
                    self.parameter_sets.push(carried);
                    changed = true;
                }
            }
        }
        if changed {
            self.session = None;
        }
    }

    /// The session for the current parameter sets, opened if it is not yet;
    /// `None` until a complete set has arrived.
    fn session_for_the_current_parameter_sets(
        &mut self,
    ) -> Result<Option<&DecompressionSessionForOneFormat>> {
        if self.session.is_none() {
            if !parameter_sets_are_complete(self.elementary_stream, &self.parameter_sets) {
                return Ok(None);
            }
            let format_description = format_description_from_parameter_sets(
                self.elementary_stream,
                &self.parameter_sets,
            )?;
            self.refuse_a_stream_past_the_maximum_coded_extent(&format_description)?;
            self.session = Some(open_decompression_session(
                self.elementary_stream,
                format_description,
                &self.awaiting_collection,
            )?);
        }
        Ok(self.session.as_ref())
    }

    fn refuse_a_stream_past_the_maximum_coded_extent(
        &self,
        format_description: &CMFormatDescription,
    ) -> Result<()> {
        let Some(maximum) = self.maximum_coded_extent else {
            return Ok(());
        };
        // SAFETY: a live video format description.
        let dimensions = unsafe { CMVideoFormatDescriptionGetDimensions(format_description) };
        let (width, height) = (dimensions.width as u32, dimensions.height as u32);
        if width > maximum.max_coded_width || height > maximum.max_coded_height {
            return Err(Error::Configuration(format!(
                "the {:?} stream codes {width}x{height}, past the decoder's {}x{} maximum",
                self.elementary_stream, maximum.max_coded_width, maximum.max_coded_height
            )));
        }
        Ok(())
    }

    /// Land one decoded image buffer in a pooled `Rgba32` pixel buffer at its
    /// clean aperture's extent.
    fn land_in_the_pool(
        &mut self,
        decoded: &CVPixelBuffer,
    ) -> Result<DecodedVideoPictureInPooledPixelBuffer> {
        let source_pixel_format =
            PixelFormat::from_cv_pixel_format_type(CVPixelBufferGetPixelFormatType(decoded));
        if !matches!(
            source_pixel_format,
            PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange
        ) {
            return Err(Error::NotSupported(format!(
                "VideoToolbox decoded the {:?} stream into {source_pixel_format:?}; only 8-bit \
                 biplanar 4:2:0 lands in the pool",
                self.elementary_stream
            )));
        }
        let iosurface = CVPixelBufferGetIOSurface(Some(decoded)).ok_or_else(|| {
            Error::GpuError("VideoToolbox decoded a picture with no IOSurface".into())
        })?;
        let picture_extent = clean_aperture_extent_of(decoded)?;
        let color_vui = core_video_pixel_buffer_color_to_h273_color_vui(decoded);
        self.last_decoded_color_vui = Some(color_vui);

        if self.pooled_rgba_conversion.as_ref().is_some_and(|built| {
            built.source_pixel_format != source_pixel_format
                || built.picture_extent != picture_extent
        }) {
            self.pooled_rgba_conversion = None;
        }
        let conversion = match &mut self.pooled_rgba_conversion {
            Some(built) => built,
            None => self
                .pooled_rgba_conversion
                .insert(PooledRgbaConversionForOneShape {
                    conversion: Biplanar420IOSurfaceToPooledRgbaConversion::create(
                        &self.gpu_context,
                        &format!("VideoToolbox {:?} decoder", self.elementary_stream),
                        source_pixel_format,
                        picture_extent.0,
                        picture_extent.1,
                    )?,
                    source_pixel_format,
                    picture_extent,
                }),
        };
        let (published_pixel_buffer_frame_id, pixel_buffer) =
            conversion.conversion.convert_into_pooled_pixel_buffer(
                &self.gpu_context,
                &iosurface,
                &color_vui.resolve_defaults(ColorSpaceKind::Yuv),
            )?;
        Ok(DecodedVideoPictureInPooledPixelBuffer {
            published_pixel_buffer_frame_id,
            pixel_buffer,
            width: picture_extent.0,
            height: picture_extent.1,
        })
    }
}

impl VideoDecodeSession for VideoToolboxDecodeSession {
    fn decode_annex_b_access_unit(
        &mut self,
        annex_b_access_unit_bytes: &[u8],
        decoded_pictures_in_completion_order: &mut Vec<DecodedVideoPictureInPooledPixelBuffer>,
    ) -> Result<()> {
        let sorted = sort_annex_b_access_unit_for_videotoolbox(
            self.elementary_stream,
            annex_b_access_unit_bytes,
        )?;
        self.take_in_parameter_sets(sorted.parameter_sets);
        if sorted.length_prefixed_sample.is_empty() {
            return Ok(());
        }
        let Some(session) = self.session_for_the_current_parameter_sets()? else {
            tracing::debug!(
                elementary_stream = ?self.elementary_stream,
                "an access unit arrived before the stream's parameter sets; skipping it"
            );
            return Ok(());
        };
        let sample_buffer =
            sample_buffer_of(&sorted.length_prefixed_sample, &session.format_description)?;
        let mut info_flags = VTDecodeInfoFlags::empty();
        // SAFETY: a live session and sample buffer; decode flags ask for a
        // synchronous decode, so the callback has run for this frame by the
        // wait below.
        let decoded = unsafe {
            session.decompression_session.decode_frame(
                &sample_buffer,
                VTDecodeFrameFlags::empty(),
                std::ptr::null_mut(),
                &mut info_flags,
            )
        };
        // SAFETY: a live session.
        unsafe { session.decompression_session.wait_for_asynchronous_frames() };
        let collected = std::mem::take(&mut *self.awaiting_collection.decoded.lock());
        if decoded != 0 {
            return Err(videotoolbox_call_failure(
                "VTDecompressionSessionDecodeFrame",
                decoded,
            ));
        }
        for decoded_image_buffer in collected {
            let decoded_image_buffer = decoded_image_buffer?;
            decoded_pictures_in_completion_order
                .push(self.land_in_the_pool(&decoded_image_buffer)?);
        }
        Ok(())
    }

    fn parsed_parameter_set_color_vui(&self) -> Option<H273ColorVui> {
        self.last_decoded_color_vui
    }

    fn discard_in_flight_state_after_a_gap(&mut self) {
        self.session = None;
        self.awaiting_collection.decoded.lock().clear();
    }

    fn reset_for_new_parameter_sets(&mut self) {
        self.discard_in_flight_state_after_a_gap();
        self.parameter_sets.clear();
        self.last_decoded_color_vui = None;
    }
}

/// Whether two parameter sets' first header bytes name the same
/// `nal_unit_type`.
fn nal_unit_type_byte_matches(
    elementary_stream: VideoCodecElementaryStream,
    held_first_byte: Option<u8>,
    carried_first_byte: Option<u8>,
) -> bool {
    let nal_unit_type = |first_byte: u8| match elementary_stream {
        VideoCodecElementaryStream::H264 => first_byte & 0x1F,
        VideoCodecElementaryStream::H265 => (first_byte >> 1) & 0x3F,
    };
    matches!(
        (held_first_byte, carried_first_byte),
        (Some(held), Some(carried)) if nal_unit_type(held) == nal_unit_type(carried)
    )
}

/// A format description built from the stream's parameter sets, stating the
/// length prefix this arm's samples carry.
fn format_description_from_parameter_sets(
    elementary_stream: VideoCodecElementaryStream,
    parameter_sets: &[Vec<u8>],
) -> Result<CFRetained<CMFormatDescription>> {
    let pointers: Vec<NonNull<u8>> = parameter_sets
        .iter()
        .map(|parameter_set| NonNull::from(parameter_set.as_slice()).cast::<u8>())
        .collect();
    let sizes: Vec<usize> = parameter_sets.iter().map(Vec::len).collect();
    let nal_unit_header_length = LENGTH_PREFIX_BYTES_OF_SAMPLES_HANDED_TO_VIDEOTOOLBOX as c_int;
    let mut format_description: *const CMFormatDescription = std::ptr::null();
    // SAFETY: `pointers` and `sizes` describe `parameter_sets`, which outlive
    // the call; the out-pointer is a stack slot.
    let created = unsafe {
        let pointers = NonNull::new_unchecked(pointers.as_ptr().cast_mut());
        let sizes_pointer = NonNull::new_unchecked(sizes.as_ptr().cast_mut());
        match elementary_stream {
            VideoCodecElementaryStream::H264 => {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    parameter_sets.len(),
                    pointers,
                    sizes_pointer,
                    nal_unit_header_length,
                    NonNull::from(&mut format_description),
                )
            }
            VideoCodecElementaryStream::H265 => {
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    None,
                    parameter_sets.len(),
                    pointers,
                    sizes_pointer,
                    nal_unit_header_length,
                    None,
                    NonNull::from(&mut format_description),
                )
            }
        }
    };
    if created != 0 || format_description.is_null() {
        return Err(Error::Configuration(format!(
            "CoreMedia refused the {elementary_stream:?} stream's parameter sets (OSStatus \
             {created})"
        )));
    }
    // SAFETY: a successful create hands back a +1 description.
    Ok(unsafe { CFRetained::from_raw(NonNull::new_unchecked(format_description.cast_mut())) })
}

/// Open a hardware decompression session over `format_description` whose
/// pictures land on IOSurfaces and are handed to `awaiting_collection`.
fn open_decompression_session(
    elementary_stream: VideoCodecElementaryStream,
    format_description: CFRetained<CMFormatDescription>,
    awaiting_collection: &DecodedImageBuffersAwaitingCollection,
) -> Result<DecompressionSessionForOneFormat> {
    ensure_core_video_pixel_format_dictionary_is_initialised();
    let decoder_specification = cf_dictionary_of_booleans(&[(
        // SAFETY: a VideoToolbox-exported constant.
        unsafe { kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder },
        true,
    )]);
    let no_iosurface_properties = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
    let destination_image_buffer_attributes = CFDictionary::<CFString, CFType>::from_slices(
        // SAFETY: a CoreVideo-exported constant.
        &[unsafe { kCVPixelBufferIOSurfacePropertiesKey }],
        &[no_iosurface_properties.as_ref()],
    );
    let output_callback = VTDecompressionOutputCallbackRecord {
        decompressionOutputCallback: Some(decompression_output_callback),
        decompressionOutputRefCon: std::ptr::from_ref(awaiting_collection).cast_mut().cast(),
    };
    let mut created: *mut VTDecompressionSession = std::ptr::null_mut();
    // SAFETY: live dictionaries of documented keys; the callback record is
    // read during the call; its refcon outlives the session (see the session
    // struct's field order); the out-pointer is a stack slot.
    let os_status = unsafe {
        VTDecompressionSession::create(
            None,
            &format_description,
            Some(decoder_specification.as_opaque()),
            Some(destination_image_buffer_attributes.as_opaque()),
            &output_callback,
            NonNull::from(&mut created),
        )
    };
    if os_status != 0 || created.is_null() {
        return Err(Error::GpuError(format!(
            "VideoToolbox has no hardware {elementary_stream:?} decoder for this stream \
             (VTDecompressionSessionCreate answered OSStatus {os_status})"
        )));
    }
    Ok(DecompressionSessionForOneFormat {
        // SAFETY: a successful create hands back a +1 session.
        decompression_session: unsafe { CFRetained::from_raw(NonNull::new_unchecked(created)) },
        format_description,
    })
}

/// A one-sample buffer over a copy of `length_prefixed_sample`.
fn sample_buffer_of(
    length_prefixed_sample: &[u8],
    format_description: &CMFormatDescription,
) -> Result<CFRetained<CMSampleBuffer>> {
    let byte_count = length_prefixed_sample.len();
    let mut block_buffer: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: a null memory block asks CoreMedia to allocate `byte_count`
    // bytes itself; the out-pointer is a stack slot.
    let allocated = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            std::ptr::null_mut(),
            byte_count,
            None,
            std::ptr::null(),
            0,
            byte_count,
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut block_buffer),
        )
    };
    if allocated != 0 || block_buffer.is_null() {
        return Err(videotoolbox_call_failure(
            "CMBlockBufferCreateWithMemoryBlock",
            allocated,
        ));
    }
    // SAFETY: a successful create hands back a +1 block buffer.
    let block_buffer = unsafe { CFRetained::from_raw(NonNull::new_unchecked(block_buffer)) };
    // SAFETY: the source is `byte_count` readable bytes, and the block buffer
    // was allocated `byte_count` bytes.
    let replaced = unsafe {
        CMBlockBuffer::replace_data_bytes(
            NonNull::from(length_prefixed_sample).cast(),
            &block_buffer,
            0,
            byte_count,
        )
    };
    if replaced != 0 {
        return Err(videotoolbox_call_failure(
            "CMBlockBufferReplaceDataBytes",
            replaced,
        ));
    }
    let mut sample_buffer: *mut CMSampleBuffer = std::ptr::null_mut();
    // SAFETY: one sample of `byte_count` bytes over a live block buffer and
    // format description; no timing, which a decode does not need.
    let created = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(&block_buffer),
            Some(format_description),
            1,
            0,
            std::ptr::null(),
            1,
            &byte_count,
            NonNull::from(&mut sample_buffer),
        )
    };
    if created != 0 || sample_buffer.is_null() {
        return Err(videotoolbox_call_failure(
            "CMSampleBufferCreateReady",
            created,
        ));
    }
    // SAFETY: a successful create hands back a +1 sample buffer.
    Ok(unsafe { CFRetained::from_raw(NonNull::new_unchecked(sample_buffer)) })
}

/// The extent of `decoded`'s clean aperture, which VideoToolbox sets from the
/// parameter sets' conformance window. Refused unless it opens at the
/// picture's top-left, where every conformance window this arm reads does.
fn clean_aperture_extent_of(decoded: &CVImageBuffer) -> Result<(u32, u32)> {
    let clean_rect = CVImageBufferGetCleanRect(decoded);
    if clean_rect.origin.x != 0.0 || clean_rect.origin.y != 0.0 {
        return Err(Error::NotSupported(format!(
            "the decoded picture's clean aperture opens at ({}, {}), not its top-left",
            clean_rect.origin.x, clean_rect.origin.y
        )));
    }
    Ok((
        clean_rect.size.width.round() as u32,
        clean_rect.size.height.round() as u32,
    ))
}

/// Runs on a VideoToolbox thread, or on the caller's inside
/// `VTDecompressionSessionDecodeFrame`, once per frame.
unsafe extern "C-unwind" fn decompression_output_callback(
    decompression_output_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    os_status: i32,
    info_flags: VTDecodeInfoFlags,
    image_buffer: *mut CVImageBuffer,
    _presentation_time_stamp: CMTime,
    _presentation_duration: CMTime,
) {
    // SAFETY: the refcon is the session's boxed state, alive until the
    // decompression session is invalidated, which happens before it drops.
    let awaiting_collection =
        unsafe { &*decompression_output_ref_con.cast::<DecodedImageBuffersAwaitingCollection>() };
    let decoded = if os_status != 0 {
        Err(videotoolbox_call_failure(
            "the decompression output",
            os_status,
        ))
    } else if info_flags.contains(VTDecodeInfoFlags::FrameDropped) {
        return;
    } else {
        match NonNull::new(image_buffer) {
            // SAFETY: a live image buffer VideoToolbox handed the callback,
            // retained here to outlive it.
            Some(image_buffer) => Ok(unsafe { CFRetained::retain(image_buffer) }),
            None => return,
        }
    };
    awaiting_collection.decoded.lock().push(decoded);
}
