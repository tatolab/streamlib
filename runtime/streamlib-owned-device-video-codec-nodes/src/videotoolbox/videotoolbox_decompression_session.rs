// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One VideoToolbox decompression session fed Annex-B access units.
//!
//! The session is built from the stream's parameter sets, so it opens at the
//! first sync point and rebuilds when they change. Pictures come back on
//! IOSurface-backed pixel buffers, read from the top-left of the clean
//! aperture — the parameter sets' conformance window.

use std::ffi::{c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_core_video::{
    CVImageBuffer, CVImageBufferGetCleanRect, CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, VTIsHardwareDecodeSupported,
    kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
};
use streamlib_video_bitstream::VideoCodecElementaryStream;
use streamlib_video_bitstream::annex_b_access_unit::{
    NAL_UNIT_LENGTH_PREFIX_BYTES, ParameterSetsFromAnnexBAccessUnit,
    length_prefix_annex_b_access_unit,
};

use super::{
    adopt_created_core_foundation_object, cf_dictionary_of_booleans, core_media_codec_type_of,
    ensure_core_video_pixel_format_dictionary_is_initialised, videotoolbox_call_failure,
};

/// A decoded picture: the pixel buffer VideoToolbox decoded it into, and the
/// extent of its clean aperture.
pub struct DecodedVideoToolboxPicture {
    pub pixel_buffer: CFRetained<CVPixelBuffer>,
    pub width: u32,
    pub height: u32,
}

/// A decoded picture's pixel buffer, handed from the VideoToolbox thread that
/// decoded it to the one that publishes it.
struct DecodedPixelBufferHandedAcrossThreads(CFRetained<CVPixelBuffer>);

// SAFETY: CoreVideo's retain and release are thread-safe, and the buffer is
// read only after the decode that wrote it has completed.
unsafe impl Send for DecodedPixelBufferHandedAcrossThreads {}

/// What the output callback hands the session: every image buffer decoded
/// since the last collection, or the failure a frame answered.
#[derive(Default)]
struct DecodedImageBuffersAwaitingCollection {
    decoded: Mutex<Vec<Result<DecodedPixelBufferHandedAcrossThreads, String>>>,
}

/// A decompression session and the format description it was built from.
struct VideoToolboxDecompressionSessionForOneFormat {
    decompression_session: CFRetained<VTDecompressionSession>,
    format_description: CFRetained<CMFormatDescription>,
    /// The callback's refcon, held until after the session is invalidated.
    awaiting_collection: Arc<DecodedImageBuffersAwaitingCollection>,
}

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

/// A VideoToolbox decompression session fed one Annex-B access unit at a
/// time, driven from the thread that holds it.
pub struct VideoToolboxDecompressionSession {
    elementary_stream: VideoCodecElementaryStream,
    /// The largest picture accepted, when the node caps it.
    maximum_picture_extent: Option<(u32, u32)>,
    /// The stream's parameter sets, each kind as the latest access unit to
    /// carry that kind carried it.
    parameter_sets: ParameterSetsFromAnnexBAccessUnit,
    decompression_session_for_current_parameter_sets:
        Option<VideoToolboxDecompressionSessionForOneFormat>,
}

// SAFETY: a decompression session may be driven from any thread, one call at a
// time, which the `&mut` receiver of every decode call guarantees.
unsafe impl Send for VideoToolboxDecompressionSession {}

impl VideoToolboxDecompressionSession {
    /// A session with nothing decoded yet, refused by name on a machine with
    /// no hardware decoder for `elementary_stream`; the VideoToolbox session
    /// itself opens at the stream's first parameter sets.
    pub fn open(
        elementary_stream: VideoCodecElementaryStream,
        maximum_picture_extent: Option<(u32, u32)>,
    ) -> Result<Self, String> {
        // SAFETY: a CoreMedia codec type.
        if !unsafe { VTIsHardwareDecodeSupported(core_media_codec_type_of(elementary_stream)) } {
            return Err(format!(
                "VideoToolbox has no hardware {elementary_stream:?} decoder on this machine"
            ));
        }
        Ok(Self {
            elementary_stream,
            maximum_picture_extent,
            parameter_sets: ParameterSetsFromAnnexBAccessUnit::default(),
            decompression_session_for_current_parameter_sets: None,
        })
    }

    /// Decode one Annex-B access unit, answering the pictures it completed.
    /// An access unit before the stream's first complete parameter sets
    /// decodes nothing.
    pub fn decode_annex_b_access_unit(
        &mut self,
        annex_b_access_unit_bytes: &[u8],
    ) -> Result<Vec<DecodedVideoToolboxPicture>, String> {
        let split =
            length_prefix_annex_b_access_unit(annex_b_access_unit_bytes, self.elementary_stream);
        self.take_in_parameter_sets(split.parameter_sets);
        if split.length_prefixed_sample_bytes.is_empty() {
            return Ok(Vec::new());
        }
        let Some(session) =
            self.open_or_reuse_decompression_session_for_current_parameter_sets()?
        else {
            tracing::debug!(
                elementary_stream = ?self.elementary_stream,
                "an access unit arrived before the stream's parameter sets; skipping it"
            );
            return Ok(Vec::new());
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
        let collected = std::mem::take(
            &mut *session
                .awaiting_collection
                .decoded
                .lock()
                .map_err(|_| "the decoded picture list was poisoned".to_string())?,
        );
        if decode_os_status != 0 {
            return Err(videotoolbox_call_failure(
                "VTDecompressionSessionDecodeFrame",
                decode_os_status,
            ));
        }
        collected
            .into_iter()
            .map(|decoded| {
                let DecodedPixelBufferHandedAcrossThreads(pixel_buffer) = decoded?;
                let (width, height) = clean_aperture_extent_of(&pixel_buffer)?;
                self.refuse_a_picture_past_the_maximum_extent(width, height)?;
                Ok(DecodedVideoToolboxPicture {
                    pixel_buffer,
                    width,
                    height,
                })
            })
            .collect()
    }

    /// Drop the session after a gap in the stream; the next group start
    /// reopens it from the parameter sets held.
    pub fn discard_in_flight_state_after_a_gap(&mut self) {
        self.decompression_session_for_current_parameter_sets = None;
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
    fn open_or_reuse_decompression_session_for_current_parameter_sets(
        &mut self,
    ) -> Result<Option<&VideoToolboxDecompressionSessionForOneFormat>, String> {
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

    /// Refuse a picture past the node's cap. CoreMedia reports every extent
    /// of a decoded stream already cropped to the conformance window, so the
    /// cap is held against the picture.
    fn refuse_a_picture_past_the_maximum_extent(
        &self,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        match self.maximum_picture_extent {
            Some((maximum_width, maximum_height))
                if width > maximum_width || height > maximum_height =>
            {
                Err(format!(
                    "the {:?} stream's pictures are {width}x{height}, past the decoder's \
                     {maximum_width}x{maximum_height} maximum",
                    self.elementary_stream
                ))
            }
            _ => Ok(()),
        }
    }
}

/// A format description built from the stream's parameter sets, stating the
/// length prefix [`length_prefix_annex_b_access_unit`] writes.
fn format_description_from_parameter_sets(
    elementary_stream: VideoCodecElementaryStream,
    parameter_sets: &ParameterSetsFromAnnexBAccessUnit,
) -> Result<CFRetained<CMFormatDescription>, String> {
    let (pointers, sizes): (Vec<NonNull<u8>>, Vec<usize>) = parameter_sets
        .in_configuration_record_order()
        .map(|parameter_set| {
            (
                NonNull::from(parameter_set).cast::<u8>(),
                parameter_set.len(),
            )
        })
        .unzip();
    let nal_unit_header_length = c_int::from(NAL_UNIT_LENGTH_PREFIX_BYTES);
    let mut created_format_description: *const CMFormatDescription = std::ptr::null();
    // SAFETY: `pointers` and `sizes` describe `parameter_sets`, which outlive
    // the call and hold at least the SPS and PPS a complete set carries, so
    // neither vector is empty; the out-pointer is a stack slot.
    let create_os_status = unsafe {
        let pointers_pointer = NonNull::new_unchecked(pointers.as_ptr().cast_mut());
        let sizes_pointer = NonNull::new_unchecked(sizes.as_ptr().cast_mut());
        match elementary_stream {
            VideoCodecElementaryStream::H264 => {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    sizes.len(),
                    pointers_pointer,
                    sizes_pointer,
                    nal_unit_header_length,
                    NonNull::from(&mut created_format_description),
                )
            }
            VideoCodecElementaryStream::H265 => {
                CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    None,
                    sizes.len(),
                    pointers_pointer,
                    sizes_pointer,
                    nal_unit_header_length,
                    None,
                    NonNull::from(&mut created_format_description),
                )
            }
        }
    };
    // SAFETY: what the create wrote, beside its status.
    unsafe {
        adopt_created_core_foundation_object(
            created_format_description.cast_mut(),
            create_os_status,
            |create_os_status| {
                format!(
                    "CoreMedia refused the {elementary_stream:?} stream's parameter sets \
                     (OSStatus {create_os_status})"
                )
            },
        )
    }
}

/// Open a hardware decompression session over `format_description` whose
/// pictures land on IOSurfaces.
fn open_decompression_session(
    elementary_stream: VideoCodecElementaryStream,
    format_description: CFRetained<CMFormatDescription>,
) -> Result<VideoToolboxDecompressionSessionForOneFormat, String> {
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
    // SAFETY: what `VTDecompressionSessionCreate` wrote, beside its status.
    let decompression_session = unsafe {
        adopt_created_core_foundation_object(
            created_decompression_session,
            create_os_status,
            |create_os_status| {
                format!(
                    "VideoToolbox has no hardware {elementary_stream:?} decoder for this stream \
                     (VTDecompressionSessionCreate answered OSStatus {create_os_status})"
                )
            },
        )?
    };
    Ok(VideoToolboxDecompressionSessionForOneFormat {
        decompression_session,
        format_description,
        awaiting_collection,
    })
}

/// A one-sample buffer over a copy of `length_prefixed_sample`.
fn sample_buffer_of(
    length_prefixed_sample: &[u8],
    format_description: &CMFormatDescription,
) -> Result<CFRetained<CMSampleBuffer>, String> {
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
    // SAFETY: what `CMBlockBufferCreateWithMemoryBlock` wrote, beside its
    // status.
    let block_buffer = unsafe {
        adopt_created_core_foundation_object(created_block_buffer, allocate_os_status, |status| {
            videotoolbox_call_failure("CMBlockBufferCreateWithMemoryBlock", status)
        })?
    };
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
    // SAFETY: what `CMSampleBufferCreateReady` wrote, beside its status.
    unsafe {
        adopt_created_core_foundation_object(created_sample_buffer, create_os_status, |status| {
            videotoolbox_call_failure("CMSampleBufferCreateReady", status)
        })
    }
}

/// The extent of `decoded`'s clean aperture, which VideoToolbox sets from the
/// parameter sets' conformance window. Refused unless it opens at the
/// picture's top-left, where every conformance window this session reads
/// does.
fn clean_aperture_extent_of(decoded: &CVImageBuffer) -> Result<(u32, u32), String> {
    let clean_rect = CVImageBufferGetCleanRect(decoded);
    if clean_rect.origin.x != 0.0 || clean_rect.origin.y != 0.0 {
        return Err(format!(
            "the decoded picture's clean aperture opens at ({}, {}), not its top-left",
            clean_rect.origin.x, clean_rect.origin.y
        ));
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
        NonNull::new(image_buffer).map(|image_buffer| {
            Ok(DecodedPixelBufferHandedAcrossThreads(unsafe {
                CFRetained::retain(image_buffer)
            }))
        })
    }))
    .unwrap_or_else(|_| Some(Err("the decompression output callback panicked".to_string())));
    if let Some(decoded) = decoded
        && let Ok(mut awaiting) = awaiting_collection.decoded.lock()
    {
        awaiting.push(decoded);
    }
}
