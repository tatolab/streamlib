// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! One VideoToolbox compression session coding NV12 pixel buffers from its own
//! pool into Annex-B access units.

use std::ffi::{c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMSampleAttachmentKey_NotSync,
    kCMTimeInvalid,
};
use objc2_core_video::{
    CVColorPrimariesGetStringForIntegerCodePoint, CVPixelBuffer, CVPixelBufferPool,
    CVTransferFunctionGetStringForIntegerCodePoint, CVYCbCrMatrixGetStringForIntegerCodePoint,
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_Quality, kVTCompressionPropertyKey_RealTime,
    kVTCompressionPropertyKey_TransferFunction, kVTCompressionPropertyKey_YCbCrMatrix,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_High_AutoLevel,
    kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};
use streamlib_consumer_rhi::H273ColorVui;
use streamlib_video_bitstream::VideoCodecElementaryStream;
use streamlib_video_bitstream::annex_b_access_unit::{
    NalUnitLengthPrefixWidth, annex_b_access_unit_from_length_prefixed_sample,
};

use super::{
    CORE_VIDEO_NV12_VIDEO_RANGE, adopt_created_core_foundation_object, cf_dictionary_of_booleans,
    core_media_codec_type_of, ensure_core_video_pixel_format_dictionary_is_initialised,
    videotoolbox_call_failure,
};
use crate::encoded_video_publication::EncodedVideoAccessUnit;

/// `kVTCompressionPropertyKey_Quality` when no bitrate is set, on
/// VideoToolbox's `0.0`–`1.0` scale: a balanced constant-quality point.
const BALANCED_CONSTANT_QUALITY: f64 = 0.75;

/// The pixel format the session's pool hands out: NV12 at video range, the
/// range every codec node signals.
pub const COMPRESSION_SESSION_POOL_PIXEL_FORMAT_WIRE_NAME: &str = "nv12_video_range";

/// What one compression session is opened for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoToolboxCompressionSessionRequest {
    pub elementary_stream: VideoCodecElementaryStream,
    pub width: u32,
    pub height: u32,
    pub frames_per_second: u32,
    pub keyframe_interval_seconds: u32,
    /// Average bitrate; `None` codes at a balanced constant quality.
    pub bitrate_bps: Option<u32>,
    /// The colour the parameter sets signal and the pool's pixels are written
    /// in.
    pub color_vui: H273ColorVui,
}

/// What the output callback hands the session: every access unit completed
/// since the last collection, or the failure a frame answered.
struct CompressedAccessUnitsAwaitingCollection {
    elementary_stream: VideoCodecElementaryStream,
    completed: Mutex<Vec<Result<EncodedVideoAccessUnit, String>>>,
}

/// A VideoToolbox compression session, driven from the thread that holds it,
/// coding one pool pixel buffer at a time with no reordering.
pub struct VideoToolboxCompressionSession {
    compression_session: CFRetained<VTCompressionSession>,
    /// The output callback's refcon, held until the session is invalidated.
    awaiting_collection: Arc<CompressedAccessUnitsAwaitingCollection>,
    coded_extent: (u32, u32),
    /// The session's own frame clock: each frame's presentation stamp is its
    /// index on a timescale of the requested rate, so rate control sees a
    /// steady cadence whatever stamps upstream publishes.
    frames_submitted: i64,
    frames_per_second_timescale: i32,
}

// SAFETY: a compression session may be driven from any thread, one call at a
// time, which the `&mut` receiver of every encode call guarantees.
unsafe impl Send for VideoToolboxCompressionSession {}

impl VideoToolboxCompressionSession {
    /// Open a hardware session for `request`, refusing by name a property the
    /// hardware encoder does not take.
    pub fn open(request: &VideoToolboxCompressionSessionRequest) -> Result<Self, String> {
        ensure_core_video_pixel_format_dictionary_is_initialised();
        if request.keyframe_interval_seconds == 0 {
            return Err(format!(
                "{:?} encode on VideoToolbox refuses keyframe_interval_seconds = 0: the \
                 streaming shape puts a sync point on a cadence, so the interval must be at \
                 least one second",
                request.elementary_stream
            ));
        }
        let awaiting_collection = Arc::new(CompressedAccessUnitsAwaitingCollection {
            elementary_stream: request.elementary_stream,
            completed: Mutex::new(Vec::new()),
        });
        let encoder_specification = cf_dictionary_of_booleans(&[(
            // SAFETY: a VideoToolbox-exported constant.
            unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder },
            true,
        )]);
        let frames_per_second_timescale =
            i32::try_from(request.frames_per_second.max(1)).map_err(|_| {
                format!(
                    "{} frames per second is past any rate VideoToolbox times",
                    request.frames_per_second
                )
            })?;
        let width = i32::try_from(request.width)
            .map_err(|_| format!("{} is too wide to encode", request.width))?;
        let height = i32::try_from(request.height)
            .map_err(|_| format!("{} is too tall to encode", request.height))?;
        let source_image_buffer_attributes = nv12_pool_pixel_buffer_attributes(width, height);

        let mut created_compression_session: *mut VTCompressionSession = std::ptr::null_mut();
        // SAFETY: the dictionaries are live and hold documented keys; the
        // refcon is the `Arc`'d state the callback reads, which the session
        // holds until after it is invalidated (see `Drop`); the out-pointer
        // is a stack slot.
        let create_os_status = unsafe {
            VTCompressionSession::create(
                None,
                width,
                height,
                core_media_codec_type_of(request.elementary_stream),
                Some(encoder_specification.as_opaque()),
                Some(source_image_buffer_attributes.as_opaque()),
                None,
                Some(compression_output_callback),
                Arc::as_ptr(&awaiting_collection).cast_mut().cast(),
                NonNull::from(&mut created_compression_session),
            )
        };
        // SAFETY: what `VTCompressionSessionCreate` wrote, beside its status.
        let compression_session = unsafe {
            adopt_created_core_foundation_object(
                created_compression_session,
                create_os_status,
                |create_os_status| {
                    format!(
                        "VideoToolbox has no hardware {:?} encoder for {}x{} \
                         (VTCompressionSessionCreate answered OSStatus {create_os_status})",
                        request.elementary_stream, request.width, request.height
                    )
                },
            )?
        };
        let session = Self {
            compression_session,
            awaiting_collection,
            coded_extent: coded_extent_videotoolbox_codes(request.width, request.height),
            frames_submitted: 0,
            frames_per_second_timescale,
        };
        session.set_the_streaming_shape_and_rate(request)?;
        session.set_the_colour_the_parameter_sets_signal(request.color_vui)?;
        // SAFETY: a live session.
        let prepare_os_status = unsafe { session.compression_session.prepare_to_encode_frames() };
        if prepare_os_status != 0 {
            return Err(videotoolbox_call_failure(
                "VTCompressionSessionPrepareToEncodeFrames",
                prepare_os_status,
            ));
        }
        Ok(session)
    }

    /// The extent the session codes at, before the parameter sets' crop.
    pub fn coded_extent(&self) -> (u32, u32) {
        self.coded_extent
    }

    /// A pixel buffer from the session's own pool for the next frame to be
    /// written into: IOSurface-backed NV12 at the session's extent.
    pub fn pixel_buffer_from_the_session_pool(&self) -> Result<CFRetained<CVPixelBuffer>, String> {
        // SAFETY: a live session; the pool exists once frames can be encoded.
        let pool = unsafe { self.compression_session.pixel_buffer_pool() }
            .ok_or("the compression session offers no pixel buffer pool")?;
        let mut created_pixel_buffer: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: a live pool and a stack out-pointer.
        let create_cv_return = unsafe {
            CVPixelBufferPool::create_pixel_buffer(
                None,
                &pool,
                NonNull::from(&mut created_pixel_buffer),
            )
        };
        // SAFETY: what `CVPixelBufferPoolCreatePixelBuffer` wrote, beside its
        // status.
        unsafe {
            adopt_created_core_foundation_object(
                created_pixel_buffer,
                create_cv_return,
                |create_cv_return| {
                    format!(
                        "the compression session's pool handed out no pixel buffer (CVReturn \
                         {create_cv_return})"
                    )
                },
            )
        }
    }

    /// Code `pool_pixel_buffer`, a sync point when `force_sync_point`, and
    /// answer the access units it completed — with reordering off, its own.
    pub fn encode_pool_pixel_buffer(
        &mut self,
        pool_pixel_buffer: &CVPixelBuffer,
        force_sync_point: bool,
    ) -> Result<Vec<EncodedVideoAccessUnit>, String> {
        let presentation_time_stamp = CMTime {
            value: self.frames_submitted,
            timescale: self.frames_per_second_timescale,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        self.frames_submitted += 1;
        let frame_properties = force_sync_point.then(frame_properties_forcing_a_key_frame);
        // SAFETY: a live session and pool pixel buffer, frame properties that
        // outlive the call, no frame refcon and no info out-pointer.
        let encode_os_status = unsafe {
            self.compression_session.encode_frame(
                pool_pixel_buffer,
                presentation_time_stamp,
                kCMTimeInvalid,
                frame_properties
                    .as_deref()
                    .map(|frame_properties| frame_properties.as_opaque()),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        // With reordering off this completes the frame just submitted, and
        // the callback has run for it by the time the call returns.
        // SAFETY: a live session.
        let complete_os_status = unsafe {
            self.compression_session
                .complete_frames(presentation_time_stamp)
        };
        // Taken before either status is read, so a failed frame leaves
        // nothing behind for the next call to hand back.
        let completed = std::mem::take(
            &mut *self
                .awaiting_collection
                .completed
                .lock()
                .map_err(|_| "the completed access unit list was poisoned".to_string())?,
        );
        if encode_os_status != 0 {
            return Err(videotoolbox_call_failure(
                "VTCompressionSessionEncodeFrame",
                encode_os_status,
            ));
        }
        if complete_os_status != 0 {
            return Err(videotoolbox_call_failure(
                "VTCompressionSessionCompleteFrames",
                complete_os_status,
            ));
        }
        completed.into_iter().collect()
    }

    /// No reordering, a sync point on the requested cadence, and the rate
    /// control the request asks for.
    fn set_the_streaming_shape_and_rate(
        &self,
        request: &VideoToolboxCompressionSessionRequest,
    ) -> Result<(), String> {
        let frames_between_sync_points = request
            .frames_per_second
            .saturating_mul(request.keyframe_interval_seconds);
        // SAFETY: each key and profile is a VideoToolbox-exported constant.
        unsafe {
            // A real-time session paces each synchronous encode-and-complete to
            // just over one frame period (about 17.5 ms at 1080p60 on Apple
            // silicon), so a 60 fps input falls behind; unpaced, it takes about
            // 6 ms.
            self.set_property(kVTCompressionPropertyKey_RealTime, CFBoolean::new(false))?;
            self.set_property(
                kVTCompressionPropertyKey_AllowFrameReordering,
                CFBoolean::new(false),
            )?;
            self.set_property(
                kVTCompressionPropertyKey_ProfileLevel,
                match request.elementary_stream {
                    VideoCodecElementaryStream::H264 => kVTProfileLevel_H264_High_AutoLevel,
                    VideoCodecElementaryStream::H265 => kVTProfileLevel_HEVC_Main_AutoLevel,
                },
            )?;
            self.set_property(
                kVTCompressionPropertyKey_ExpectedFrameRate,
                &CFNumber::new_i64(i64::from(request.frames_per_second)),
            )?;
            self.set_property(
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                &CFNumber::new_i64(i64::from(frames_between_sync_points)),
            )?;
            self.set_property(
                kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
                &CFNumber::new_f64(f64::from(request.keyframe_interval_seconds)),
            )?;
            match request.bitrate_bps {
                Some(bitrate_bps) => self.set_property(
                    kVTCompressionPropertyKey_AverageBitRate,
                    &CFNumber::new_i64(i64::from(bitrate_bps)),
                ),
                None => self.set_property(
                    kVTCompressionPropertyKey_Quality,
                    &CFNumber::new_f64(BALANCED_CONSTANT_QUALITY),
                ),
            }
        }
    }

    /// The colour VideoToolbox writes into the parameter sets' VUI. The range
    /// is the pool's pixel format's, which VideoToolbox states for itself.
    fn set_the_colour_the_parameter_sets_signal(
        &self,
        color_vui: H273ColorVui,
    ) -> Result<(), String> {
        type CoreVideoStringForCodePoint =
            extern "C-unwind" fn(c_int) -> Option<CFRetained<CFString>>;
        // SAFETY: VideoToolbox-exported constants.
        let axes: [(&str, &CFString, Option<u8>, CoreVideoStringForCodePoint); 3] = unsafe {
            [
                (
                    "primaries",
                    kVTCompressionPropertyKey_ColorPrimaries,
                    color_vui.primaries,
                    CVColorPrimariesGetStringForIntegerCodePoint,
                ),
                (
                    "transfer",
                    kVTCompressionPropertyKey_TransferFunction,
                    color_vui.transfer,
                    CVTransferFunctionGetStringForIntegerCodePoint,
                ),
                (
                    "matrix",
                    kVTCompressionPropertyKey_YCbCrMatrix,
                    color_vui.matrix,
                    CVYCbCrMatrixGetStringForIntegerCodePoint,
                ),
            ]
        };
        for (axis_name, property_key, code_point, core_video_string_for) in axes {
            let Some(code_point) = code_point else {
                continue;
            };
            let core_video_string =
                core_video_string_for(c_int::from(code_point)).ok_or_else(|| {
                    format!(
                        "VideoToolbox cannot signal H.273 {axis_name} {code_point}: CoreVideo \
                         names no colour for it"
                    )
                })?;
            self.set_property(property_key, &core_video_string)?;
        }
        Ok(())
    }

    fn set_property(&self, key: &CFString, value: &CFType) -> Result<(), String> {
        // SAFETY: a live session, a VideoToolbox property key and a value of
        // the type that key documents.
        let set_os_status =
            unsafe { VTSessionSetProperty(&self.compression_session, key, Some(value)) };
        if set_os_status != 0 {
            return Err(format!(
                "VideoToolbox's hardware encoder refused {key} = {value:?} (OSStatus \
                 {set_os_status})"
            ));
        }
        Ok(())
    }
}

impl Drop for VideoToolboxCompressionSession {
    fn drop(&mut self) {
        // SAFETY: a live session. Completing first lets every frame in
        // flight reach the callback; after the invalidate none runs, so the
        // refcon may drop with this struct.
        unsafe {
            self.compression_session.complete_frames(kCMTimeInvalid);
            self.compression_session.invalidate();
        }
    }
}

/// The extent VideoToolbox codes a `width` × `height` source at, before the
/// parameter sets' conformance crop: whole 16-sample blocks for both H.264's
/// macroblocks and, as Apple's encoders write it, H.265's coding blocks.
fn coded_extent_videotoolbox_codes(width: u32, height: u32) -> (u32, u32) {
    const BLOCK_SIZE: u32 = 16;
    (
        width.next_multiple_of(BLOCK_SIZE),
        height.next_multiple_of(BLOCK_SIZE),
    )
}

/// The per-frame properties that make `VTCompressionSessionEncodeFrame` emit
/// a sync sample.
fn frame_properties_forcing_a_key_frame() -> CFRetained<CFDictionary<CFString, CFType>> {
    // SAFETY: a VideoToolbox-exported constant.
    let keys = unsafe { [kVTEncodeFrameOptionKey_ForceKeyFrame] };
    let values: [&CFType; 1] = [CFBoolean::new(true)];
    CFDictionary::from_slices(&keys, &values)
}

/// The attributes of the pool the session hands out source pixel buffers
/// from: IOSurface-backed video-range NV12 at the session's extent.
fn nv12_pool_pixel_buffer_attributes(
    width: i32,
    height: i32,
) -> CFRetained<CFDictionary<CFString, CFType>> {
    let pixel_format_number = CFNumber::new_i64(i64::from(CORE_VIDEO_NV12_VIDEO_RANGE));
    let width_number = CFNumber::new_i64(i64::from(width));
    let height_number = CFNumber::new_i64(i64::from(height));
    let no_iosurface_properties = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
    // SAFETY: CoreVideo-exported constants.
    let keys = unsafe {
        [
            kCVPixelBufferPixelFormatTypeKey,
            kCVPixelBufferWidthKey,
            kCVPixelBufferHeightKey,
            kCVPixelBufferIOSurfacePropertiesKey,
        ]
    };
    let values: [&CFType; 4] = [
        &pixel_format_number,
        &width_number,
        &height_number,
        &no_iosurface_properties,
    ];
    CFDictionary::from_slices(&keys, &values)
}

/// Runs on a VideoToolbox thread, or on the caller's inside
/// `VTCompressionSessionCompleteFrames`, once per frame. A panic is caught
/// and reported as the frame's failure rather than unwound into
/// VideoToolbox.
unsafe extern "C-unwind" fn compression_output_callback(
    output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    os_status: i32,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: *mut CMSampleBuffer,
) {
    // SAFETY: the refcon is the session's `Arc`'d state, alive until the
    // session is invalidated, which happens before the session drops it.
    let awaiting_collection =
        unsafe { &*output_callback_ref_con.cast::<CompressedAccessUnitsAwaitingCollection>() };
    let completed_access_unit = catch_unwind(AssertUnwindSafe(|| {
        if os_status != 0 {
            return Some(Err(videotoolbox_call_failure(
                "the compression output",
                os_status,
            )));
        }
        if info_flags.contains(VTEncodeInfoFlags::FrameDropped) {
            return None;
        }
        // SAFETY: a sample buffer VideoToolbox handed the callback, alive for
        // the callback's duration.
        let sample_buffer = unsafe { sample_buffer.as_ref() }?;
        Some(annex_b_access_unit_from_compressed_sample(
            awaiting_collection.elementary_stream,
            sample_buffer,
        ))
    }))
    .unwrap_or_else(|_| {
        Some(Err(
            "the compression output callback panicked converting an access unit".to_string(),
        ))
    });
    if let Some(completed_access_unit) = completed_access_unit
        && let Ok(mut completed) = awaiting_collection.completed.lock()
    {
        completed.push(completed_access_unit);
    }
}

/// The Annex-B access unit a compressed sample is, with the format
/// description's parameter sets in front when it is a sync point.
fn annex_b_access_unit_from_compressed_sample(
    elementary_stream: VideoCodecElementaryStream,
    sample_buffer: &CMSampleBuffer,
) -> Result<EncodedVideoAccessUnit, String> {
    // SAFETY: a live sample buffer.
    let (block_buffer, format_description) = unsafe {
        (
            sample_buffer.data_buffer(),
            sample_buffer.format_description(),
        )
    };
    let block_buffer = block_buffer.ok_or("a compressed sample came back with no data")?;
    let format_description =
        format_description.ok_or("a compressed sample came back with no format description")?;
    let length_prefixed_sample = bytes_of_block_buffer(&block_buffer)?;
    let parameter_sets_of_this_stream =
        ParameterSetsOfAFormatDescription::of(elementary_stream, &format_description)?;
    let is_sync_point = is_sync_sample(sample_buffer);
    let parameter_sets_in_front = if is_sync_point {
        parameter_sets_of_this_stream.every_parameter_set()?
    } else {
        Vec::new()
    };
    let annex_b_access_unit_bytes = annex_b_access_unit_from_length_prefixed_sample(
        &length_prefixed_sample,
        &parameter_sets_in_front,
        parameter_sets_of_this_stream.length_prefix_width,
    )
    .map_err(|refusal| format!("VideoToolbox's sample: {refusal}"))?;
    Ok(EncodedVideoAccessUnit {
        annex_b_access_unit_bytes,
        is_sync_point,
    })
}

fn bytes_of_block_buffer(block_buffer: &CMBlockBuffer) -> Result<Vec<u8>, String> {
    // SAFETY: a live block buffer.
    let byte_count = unsafe { block_buffer.data_length() };
    let mut bytes = vec![0u8; byte_count];
    if byte_count == 0 {
        return Ok(bytes);
    }
    // SAFETY: the destination is `byte_count` writable bytes of a live Vec.
    let copy_os_status = unsafe {
        block_buffer.copy_data_bytes(0, byte_count, NonNull::from(bytes.as_mut_slice()).cast())
    };
    if copy_os_status != 0 {
        return Err(videotoolbox_call_failure(
            "CMBlockBufferCopyDataBytes",
            copy_os_status,
        ));
    }
    Ok(bytes)
}

/// A format description's parameter sets, read as they are asked for.
struct ParameterSetsOfAFormatDescription<'a> {
    elementary_stream: VideoCodecElementaryStream,
    format_description: &'a CMFormatDescription,
    parameter_set_count: usize,
    /// The width of the length prefix the description's samples carry.
    length_prefix_width: NalUnitLengthPrefixWidth,
}

impl<'a> ParameterSetsOfAFormatDescription<'a> {
    /// Count the description's parameter sets and read its length-prefix
    /// width, copying no set.
    fn of(
        elementary_stream: VideoCodecElementaryStream,
        format_description: &'a CMFormatDescription,
    ) -> Result<Self, String> {
        let (mut parameter_set_count, mut nal_unit_header_length) = (0usize, 0 as c_int);
        let count_os_status = read_format_description_parameter_set(
            elementary_stream,
            format_description,
            0,
            FormatDescriptionParameterSetOutputs {
                parameter_set_count: Some(&mut parameter_set_count),
                nal_unit_header_length: Some(&mut nal_unit_header_length),
                ..FormatDescriptionParameterSetOutputs::default()
            },
        );
        if count_os_status != 0 {
            return Err(videotoolbox_call_failure(
                "reading the format description's parameter sets",
                count_os_status,
            ));
        }
        let length_prefix_width =
            NalUnitLengthPrefixWidth::try_from(i64::from(nal_unit_header_length))
                .map_err(|refusal| format!("VideoToolbox's format description: {refusal}"))?;
        Ok(Self {
            elementary_stream,
            format_description,
            parameter_set_count,
            length_prefix_width,
        })
    }

    /// Every parameter set, in the description's order.
    fn every_parameter_set(&self) -> Result<Vec<&'a [u8]>, String> {
        (0..self.parameter_set_count)
            .map(|index| {
                let (mut pointer, mut size) = (std::ptr::null::<u8>(), 0usize);
                let read_os_status = read_format_description_parameter_set(
                    self.elementary_stream,
                    self.format_description,
                    index,
                    FormatDescriptionParameterSetOutputs {
                        parameter_set_pointer: Some(&mut pointer),
                        parameter_set_size: Some(&mut size),
                        ..FormatDescriptionParameterSetOutputs::default()
                    },
                );
                if read_os_status != 0 {
                    return Err(videotoolbox_call_failure(
                        "reading one of the format description's parameter sets",
                        read_os_status,
                    ));
                }
                if pointer.is_null() {
                    return Err(format!(
                        "the format description's parameter set {index} of {} has no bytes",
                        self.parameter_set_count
                    ));
                }
                // SAFETY: `size` bytes of the format description's own
                // storage, alive while the description is.
                Ok(unsafe { std::slice::from_raw_parts(pointer, size) })
            })
            .collect()
    }
}

/// What one read of a format description's parameter sets writes back; an
/// output left `None` is not asked for.
#[derive(Default)]
struct FormatDescriptionParameterSetOutputs<'a> {
    parameter_set_pointer: Option<&'a mut *const u8>,
    parameter_set_size: Option<&'a mut usize>,
    parameter_set_count: Option<&'a mut usize>,
    nal_unit_header_length: Option<&'a mut c_int>,
}

/// Read parameter set `index` of `format_description` into whichever of
/// `outputs` are asked for, answering the call's `OSStatus`.
fn read_format_description_parameter_set(
    elementary_stream: VideoCodecElementaryStream,
    format_description: &CMFormatDescription,
    index: usize,
    outputs: FormatDescriptionParameterSetOutputs<'_>,
) -> i32 {
    fn slot_or_null<Slot>(slot: Option<&mut Slot>) -> *mut Slot {
        slot.map_or(std::ptr::null_mut(), std::ptr::from_mut)
    }
    let (pointer_out, size_out, count_out, header_length_out) = (
        slot_or_null(outputs.parameter_set_pointer),
        slot_or_null(outputs.parameter_set_size),
        slot_or_null(outputs.parameter_set_count),
        slot_or_null(outputs.nal_unit_header_length),
    );
    // SAFETY: a live format description; every out-pointer is a live
    // exclusive borrow or null, which the call documents as "not wanted".
    unsafe {
        match elementary_stream {
            VideoCodecElementaryStream::H264 => CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                format_description,
                index,
                pointer_out,
                size_out,
                count_out,
                header_length_out,
            ),
            VideoCodecElementaryStream::H265 => CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                format_description,
                index,
                pointer_out,
                size_out,
                count_out,
                header_length_out,
            ),
        }
    }
}

/// A sample is a sync point unless its attachments say `NotSync`.
fn is_sync_sample(sample_buffer: &CMSampleBuffer) -> bool {
    // SAFETY: a live sample buffer; not asking for the array to be created.
    let Some(attachments) = (unsafe { sample_buffer.sample_attachments_array(false) }) else {
        return true;
    };
    let attachments: &CFArray = &attachments;
    if attachments.count() == 0 {
        return true;
    }
    // SAFETY: the array's elements are the per-sample attachment
    // dictionaries, per `CMSampleBufferGetSampleAttachmentsArray`, and the
    // key is a CoreMedia-exported constant.
    let not_sync = unsafe {
        let first_sample_attachments = &*attachments.value_at_index(0).cast::<CFDictionary>();
        first_sample_attachments.value(std::ptr::from_ref(kCMSampleAttachmentKey_NotSync).cast())
    };
    // SAFETY: `NotSync` is documented as a `CFBoolean`.
    not_sync.is_null() || !unsafe { &*not_sync.cast::<CFBoolean>() }.as_bool()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extent_codes_at_whole_blocks_and_an_aligned_one_is_unchanged() {
        assert_eq!(coded_extent_videotoolbox_codes(320, 180), (320, 192));
        assert_eq!(coded_extent_videotoolbox_codes(1920, 1080), (1920, 1088));
        assert_eq!(coded_extent_videotoolbox_codes(1280, 720), (1280, 720));
    }
}
