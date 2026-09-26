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
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;

use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
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

use super::{cf_dictionary_of_booleans, videotoolbox_call_failure};
use crate::apple::biplanar_420_iosurface_to_pooled_rgba_conversion::Biplanar420IOSurfaceToPooledRgbaConversion;
use crate::apple::core_video_pixel_buffer_color::core_video_pixel_buffer_color_to_h273_color_vui;
use crate::apple::core_video_pixel_format_dictionary::ensure_core_video_pixel_format_dictionary_is_initialised;
use crate::core::annex_b_access_unit::{
    NAL_UNIT_LENGTH_PREFIX_BYTES, ParameterSetsFromAnnexBAccessUnit,
    length_prefix_annex_b_access_unit,
};
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

/// A decompression session and the format description it was built from,
/// driven from whichever thread holds them.
struct VideoToolboxDecompressionSessionForOneFormat {
    decompression_session: CFRetained<VTDecompressionSession>,
    format_description: CFRetained<CMFormatDescription>,
    /// The callback's refcon, held until after the session is invalidated.
    awaiting_collection: Arc<DecodedImageBuffersAwaitingCollection>,
}

// SAFETY: a decompression session may be driven from any thread, one call at
// a time, which the `&mut` receiver of every decode call guarantees; the
// format description is immutable once created.
unsafe impl Send for VideoToolboxDecompressionSessionForOneFormat {}

impl Drop for VideoToolboxDecompressionSessionForOneFormat {
    fn drop(&mut self) {
        // SAFETY: a live session. Waiting first lets every frame in flight
        // reach the callback; after the invalidate none runs, so the refcon
        // may drop with this struct.
        unsafe {
            self.decompression_session.wait_for_asynchronous_frames();
            self.decompression_session.invalidate();
        }
    }
}

/// The conversion into the pool and the shape it was built for.
struct PooledRgbaConversionForOneShape {
    biplanar_420_to_pooled_rgba_conversion: Biplanar420IOSurfaceToPooledRgbaConversion,
    source_pixel_format: PixelFormat,
    picture_extent: (u32, u32),
}

/// A VideoToolbox decompression session fed one Annex-B access unit at a
/// time.
pub(super) struct VideoToolboxDecodeSession {
    elementary_stream: VideoCodecElementaryStream,
    maximum_coded_extent: Option<VideoDecodeMaximumCodedExtent>,
    /// The stream's parameter sets, each kind as the latest access unit that
    /// carried it carried it.
    parameter_sets: ParameterSetsFromAnnexBAccessUnit,
    decompression_session_for_current_parameter_sets:
        Option<VideoToolboxDecompressionSessionForOneFormat>,
    pooled_rgba_conversion: Option<PooledRgbaConversionForOneShape>,
    last_decoded_color_vui: Option<H273ColorVui>,
    /// Acquires the pooled pixel buffers pictures land in.
    gpu_context: GpuContextLimitedAccess,
}

impl VideoToolboxDecodeSession {
    /// A session with nothing decoded yet; the VideoToolbox session itself
    /// opens at the stream's first parameter sets.
    pub(super) fn open(
        gpu_context: GpuContextLimitedAccess,
        request: &VideoDecodeSessionRequest,
    ) -> Self {
        Self {
            elementary_stream: request.elementary_stream,
            maximum_coded_extent: request.maximum_coded_extent,
            parameter_sets: ParameterSetsFromAnnexBAccessUnit::default(),
            decompression_session_for_current_parameter_sets: None,
            pooled_rgba_conversion: None,
            last_decoded_color_vui: None,
            gpu_context,
        }
    }

    /// Take in the access unit's parameter sets, each kind it carried
    /// replacing the held one, and drop a session they no longer describe.
    fn take_in_parameter_sets(&mut self, carried: ParameterSetsFromAnnexBAccessUnit) {
        let mut changed = false;
        for (held, carried) in [
            (
                &mut self.parameter_sets.video_parameter_set_nal_units,
                carried.video_parameter_set_nal_units,
            ),
            (
                &mut self.parameter_sets.sequence_parameter_set_nal_units,
                carried.sequence_parameter_set_nal_units,
            ),
            (
                &mut self.parameter_sets.picture_parameter_set_nal_units,
                carried.picture_parameter_set_nal_units,
            ),
        ] {
            if !carried.is_empty() && *held != carried {
                *held = carried;
                changed = true;
            }
        }
        if changed {
            self.decompression_session_for_current_parameter_sets = None;
        }
    }

    /// The session for the current parameter sets, opened if it is not yet;
    /// `None` until a complete set has arrived.
    fn decompression_session_for_current_parameter_sets(
        &mut self,
    ) -> Result<Option<&VideoToolboxDecompressionSessionForOneFormat>> {
        if self
            .decompression_session_for_current_parameter_sets
            .is_none()
        {
            if !self.parameter_sets.is_complete_for(self.elementary_stream) {
                return Ok(None);
            }
            let format_description = format_description_from_parameter_sets(
                self.elementary_stream,
                &self.parameter_sets,
            )?;
            self.decompression_session_for_current_parameter_sets = Some(
                open_decompression_session(self.elementary_stream, format_description)?,
            );
        }
        Ok(self
            .decompression_session_for_current_parameter_sets
            .as_ref())
    }

    /// Refuse a picture past the cap the request set. CoreMedia reports
    /// every extent of a decoded stream already cropped to the conformance
    /// window, and the coded extent is written only in the SPS, so this arm
    /// holds the cap against the picture — at most one coding block short of
    /// the coded extent the Vulkan arm sizes its picture buffer by.
    fn refuse_a_picture_past_the_maximum_extent(&self, picture_extent: (u32, u32)) -> Result<()> {
        let Some(maximum) = self.maximum_coded_extent else {
            return Ok(());
        };
        let (picture_width, picture_height) = picture_extent;
        if picture_width > maximum.max_coded_width || picture_height > maximum.max_coded_height {
            return Err(Error::Configuration(format!(
                "the {:?} stream's pictures are {picture_width}x{picture_height}, past the \
                 decoder's {}x{} maximum",
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
        self.refuse_a_picture_past_the_maximum_extent(picture_extent)?;
        let color_vui = core_video_pixel_buffer_color_to_h273_color_vui(decoded);
        self.last_decoded_color_vui = Some(color_vui);

        if self.pooled_rgba_conversion.as_ref().is_some_and(|built| {
            built.source_pixel_format != source_pixel_format
                || built.picture_extent != picture_extent
        }) {
            self.pooled_rgba_conversion = None;
        }
        let pooled_rgba_conversion = match &mut self.pooled_rgba_conversion {
            Some(built) => built,
            None => self
                .pooled_rgba_conversion
                .insert(PooledRgbaConversionForOneShape {
                    biplanar_420_to_pooled_rgba_conversion:
                        Biplanar420IOSurfaceToPooledRgbaConversion::create(
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
        let (published_pixel_buffer_frame_id, pixel_buffer) = pooled_rgba_conversion
            .biplanar_420_to_pooled_rgba_conversion
            .convert_into_pooled_pixel_buffer(
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
        let split =
            length_prefix_annex_b_access_unit(annex_b_access_unit_bytes, self.elementary_stream);
        self.take_in_parameter_sets(split.parameter_sets);
        if split.length_prefixed_sample_bytes.is_empty() {
            return Ok(());
        }
        let Some(session) = self.decompression_session_for_current_parameter_sets()? else {
            tracing::debug!(
                elementary_stream = ?self.elementary_stream,
                "an access unit arrived before the stream's parameter sets; skipping it"
            );
            return Ok(());
        };
        let sample_buffer = sample_buffer_of(
            &split.length_prefixed_sample_bytes,
            &session.format_description,
        )?;
        let mut info_flags = VTDecodeInfoFlags::empty();
        // SAFETY: a live session and sample buffer. Without the asynchronous
        // decompression flag the decode is synchronous, so the callback has
        // run for this frame by the wait below.
        let decode_os_status = unsafe {
            session.decompression_session.decode_frame(
                &sample_buffer,
                VTDecodeFrameFlags::empty(),
                std::ptr::null_mut(),
                &mut info_flags,
            )
        };
        // SAFETY: a live session.
        unsafe { session.decompression_session.wait_for_asynchronous_frames() };
        let collected = std::mem::take(&mut *session.awaiting_collection.decoded.lock());
        if decode_os_status != 0 {
            return Err(videotoolbox_call_failure(
                "VTDecompressionSessionDecodeFrame",
                decode_os_status,
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
        self.decompression_session_for_current_parameter_sets = None;
    }

    fn reset_for_new_parameter_sets(&mut self) {
        self.discard_in_flight_state_after_a_gap();
        self.parameter_sets = ParameterSetsFromAnnexBAccessUnit::default();
        self.last_decoded_color_vui = None;
    }
}

/// A format description built from the stream's parameter sets, stating the
/// length prefix [`length_prefix_annex_b_access_unit`] writes.
fn format_description_from_parameter_sets(
    elementary_stream: VideoCodecElementaryStream,
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
) -> Result<CFRetained<CMFormatDescription>> {
    let in_order: Vec<&[u8]> = parameter_sets.in_configuration_record_order().collect();
    let pointers: Vec<NonNull<u8>> = in_order
        .iter()
        .map(|parameter_set| NonNull::from(*parameter_set).cast::<u8>())
        .collect();
    let sizes: Vec<usize> = in_order
        .iter()
        .map(|parameter_set| parameter_set.len())
        .collect();
    let nal_unit_header_length = c_int::from(NAL_UNIT_LENGTH_PREFIX_BYTES);
    let mut created_format_description: *const CMFormatDescription = std::ptr::null();
    // SAFETY: `pointers` and `sizes` describe `in_order`, which outlives the
    // call and holds at least the SPS and PPS a complete set carries, so
    // neither vector is empty; the out-pointer is a stack slot.
    let create_os_status = unsafe {
        let pointers = NonNull::new_unchecked(pointers.as_ptr().cast_mut());
        let sizes = NonNull::new_unchecked(sizes.as_ptr().cast_mut());
        match elementary_stream {
            VideoCodecElementaryStream::H264 => {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    in_order.len(),
                    pointers,
                    sizes,
                    nal_unit_header_length,
                    NonNull::from(&mut created_format_description),
                )
            }
            VideoCodecElementaryStream::H265 => {
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    None,
                    in_order.len(),
                    pointers,
                    sizes,
                    nal_unit_header_length,
                    None,
                    NonNull::from(&mut created_format_description),
                )
            }
        }
    };
    match NonNull::new(created_format_description.cast_mut()).filter(|_| create_os_status == 0) {
        // SAFETY: a successful create hands back a +1 description.
        Some(created) => Ok(unsafe { CFRetained::from_raw(created) }),
        None => Err(Error::Configuration(format!(
            "CoreMedia refused the {elementary_stream:?} stream's parameter sets (OSStatus \
             {create_os_status})"
        ))),
    }
}

/// Open a hardware decompression session over `format_description` whose
/// pictures land on IOSurfaces.
fn open_decompression_session(
    elementary_stream: VideoCodecElementaryStream,
    format_description: CFRetained<CMFormatDescription>,
) -> Result<VideoToolboxDecompressionSessionForOneFormat> {
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
    let awaiting_collection = Arc::new(DecodedImageBuffersAwaitingCollection::default());
    let output_callback = VTDecompressionOutputCallbackRecord {
        decompressionOutputCallback: Some(decompression_output_callback),
        decompressionOutputRefCon: Arc::as_ptr(&awaiting_collection).cast_mut().cast(),
    };
    let mut created_decompression_session: *mut VTDecompressionSession = std::ptr::null_mut();
    // SAFETY: live dictionaries of documented keys; the callback record is
    // read during the call; its refcon is `Arc`'d state the returned value
    // holds until after the session is invalidated; the out-pointer is a
    // stack slot.
    let create_os_status = unsafe {
        VTDecompressionSession::create(
            None,
            &format_description,
            Some(decoder_specification.as_opaque()),
            Some(destination_image_buffer_attributes.as_opaque()),
            &output_callback,
            NonNull::from(&mut created_decompression_session),
        )
    };
    let Some(created_decompression_session) =
        NonNull::new(created_decompression_session).filter(|_| create_os_status == 0)
    else {
        return Err(Error::GpuError(format!(
            "VideoToolbox has no hardware {elementary_stream:?} decoder for this stream \
             (VTDecompressionSessionCreate answered OSStatus {create_os_status})"
        )));
    };
    Ok(VideoToolboxDecompressionSessionForOneFormat {
        // SAFETY: a successful create hands back a +1 session.
        decompression_session: unsafe { CFRetained::from_raw(created_decompression_session) },
        format_description,
        awaiting_collection,
    })
}

/// A one-sample buffer over a copy of `length_prefixed_sample`.
fn sample_buffer_of(
    length_prefixed_sample: &[u8],
    format_description: &CMFormatDescription,
) -> Result<CFRetained<CMSampleBuffer>> {
    let byte_count = length_prefixed_sample.len();
    let mut created_block_buffer: *mut CMBlockBuffer = std::ptr::null_mut();
    // SAFETY: a null memory block asks CoreMedia to allocate `byte_count`
    // bytes itself; the out-pointer is a stack slot.
    let allocate_os_status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            std::ptr::null_mut(),
            byte_count,
            None,
            std::ptr::null(),
            0,
            byte_count,
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut created_block_buffer),
        )
    };
    let Some(created_block_buffer) =
        NonNull::new(created_block_buffer).filter(|_| allocate_os_status == 0)
    else {
        return Err(videotoolbox_call_failure(
            "CMBlockBufferCreateWithMemoryBlock",
            allocate_os_status,
        ));
    };
    // SAFETY: a successful create hands back a +1 block buffer.
    let block_buffer = unsafe { CFRetained::from_raw(created_block_buffer) };
    // SAFETY: the source is `byte_count` readable bytes, and the block buffer
    // was allocated `byte_count` bytes.
    let replace_os_status = unsafe {
        CMBlockBuffer::replace_data_bytes(
            NonNull::from(length_prefixed_sample).cast(),
            &block_buffer,
            0,
            byte_count,
        )
    };
    if replace_os_status != 0 {
        return Err(videotoolbox_call_failure(
            "CMBlockBufferReplaceDataBytes",
            replace_os_status,
        ));
    }
    let mut created_sample_buffer: *mut CMSampleBuffer = std::ptr::null_mut();
    // SAFETY: one sample of `byte_count` bytes over a live block buffer and
    // format description; no timing, which a decode does not need.
    let create_os_status = unsafe {
        CMSampleBuffer::create_ready(
            None,
            Some(&block_buffer),
            Some(format_description),
            1,
            0,
            std::ptr::null(),
            1,
            &byte_count,
            NonNull::from(&mut created_sample_buffer),
        )
    };
    match NonNull::new(created_sample_buffer).filter(|_| create_os_status == 0) {
        // SAFETY: a successful create hands back a +1 sample buffer.
        Some(created) => Ok(unsafe { CFRetained::from_raw(created) }),
        None => Err(videotoolbox_call_failure(
            "CMSampleBufferCreateReady",
            create_os_status,
        )),
    }
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
/// `VTDecompressionSessionDecodeFrame`, once per frame. A panic is caught and
/// reported as the frame's failure rather than unwound into VideoToolbox.
unsafe extern "C-unwind" fn decompression_output_callback(
    decompression_output_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    os_status: i32,
    info_flags: VTDecodeInfoFlags,
    image_buffer: *mut CVImageBuffer,
    _presentation_time_stamp: CMTime,
    _presentation_duration: CMTime,
) {
    // SAFETY: the refcon is the session's `Arc`'d state, alive until the
    // decompression session is invalidated, which happens before it drops.
    let awaiting_collection =
        unsafe { &*decompression_output_ref_con.cast::<DecodedImageBuffersAwaitingCollection>() };
    let decoded = catch_unwind(AssertUnwindSafe(|| {
        if os_status != 0 {
            return Some(Err(videotoolbox_call_failure(
                "the decompression output",
                os_status,
            )));
        }
        if info_flags.contains(VTDecodeInfoFlags::FrameDropped) {
            return None;
        }
        // SAFETY: a live image buffer VideoToolbox handed the callback,
        // retained here to outlive it.
        NonNull::new(image_buffer)
            .map(|image_buffer| Ok(unsafe { CFRetained::retain(image_buffer) }))
    }))
    .unwrap_or_else(|_| {
        Some(Err(Error::GpuError(
            "the decompression output callback panicked".into(),
        )))
    });
    if let Some(decoded) = decoded {
        awaiting_collection.decoded.lock().push(decoded);
    }
}
