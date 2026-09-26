// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One VideoToolbox compression session behind the seam's encode shape.

use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::ptr::NonNull;

use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMSampleAttachmentKey_NotSync,
    kCMTimeInvalid,
};
use objc2_core_video::{
    CVColorPrimariesGetStringForIntegerCodePoint, CVPixelBuffer, CVPixelBufferGetIOSurface,
    CVPixelBufferPool, CVPixelBufferPoolCreatePixelBuffer,
    CVTransferFunctionGetStringForIntegerCodePoint, CVYCbCrMatrixGetStringForIntegerCodePoint,
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVReturnSuccess,
};
use objc2_io_surface::IOSurfaceID;
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_Quality, kVTCompressionPropertyKey_RealTime,
    kVTCompressionPropertyKey_TransferFunction, kVTCompressionPropertyKey_YCbCrMatrix,
    kVTProfileLevel_H264_High_AutoLevel, kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};
use parking_lot::Mutex;

use super::annex_b_and_length_prefixed_nal_units::annex_b_access_unit_from_length_prefixed_sample;
use super::{cf_dictionary_of_booleans, core_media_codec_type_of, videotoolbox_call_failure};
use crate::apple::core_video_pixel_format_dictionary::ensure_core_video_pixel_format_dictionary_is_initialised;
use crate::core::color::{ColorSpaceKind, H273ColorVui, RangeId, ResolvedColorInfo};
use crate::core::context::{
    EncodedVideoAccessUnitFromSession, GpuContextFullAccess, GpuContextLimitedAccess,
    VideoCodecElementaryStream, VideoEncodeSession, VideoEncodeSessionRequest,
    VideoEncodeSourceSurface,
};
use crate::core::rhi::{PixelFormat, RhiColorConverter, VulkanLayout};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{
    COLOR_CONVERTER_WORKGROUP_SIZE, ImportedIOSurfaceStorageBuffer, RhiCommandRecorder,
    VulkanAccess, VulkanStage,
};

/// `kVTCompressionPropertyKey_Quality` when no bitrate is set: the middle of
/// VideoToolbox's `0.0`–`1.0` scale, the arm's balanced constant-quality
/// point.
const BALANCED_CONSTANT_QUALITY: f64 = 0.75;

/// Nanoseconds per second: the timescale every presentation stamp this arm
/// hands VideoToolbox is in, so a stamp comes back as the same integer.
const NANOSECONDS_TIMESCALE: i32 = 1_000_000_000;

/// The layout a source texture is barriered into before the conversion
/// samples it — the one every sampled descriptor the compute kernel binds
/// declares.
const LAYOUT_THE_CONVERSION_SAMPLES_IN: VulkanLayout = VulkanLayout::SHADER_READ_ONLY_OPTIMAL;

/// Pixels per conversion thread along each axis: the kernel writes 4×2 blocks.
const CONVERSION_BLOCK_WIDTH: u32 = 4;
const CONVERSION_BLOCK_HEIGHT: u32 = 2;

/// What the output callback hands the session: every access unit completed
/// since the last collection, or the first failure among them.
struct CompressedAccessUnitsAwaitingCollection {
    elementary_stream: VideoCodecElementaryStream,
    completed: Mutex<Vec<Result<EncodedVideoAccessUnitFromSession>>>,
}

/// A VideoToolbox compression session, coding one published surface at a
/// time and handing back what each completed.
///
/// CoreVideo wraps no `'RGBA'` IOSurface as a pixel buffer, so a source frame
/// is never handed to VideoToolbox as it is: the RHI's colour converter
/// writes it, on the GPU, into an NV12 surface from the session's own pool —
/// in the matrix and range the parameter sets signal — and that is encoded.
pub(super) struct VideoToolboxEncodeSession {
    compression_session: CFRetained<VTCompressionSession>,
    /// The output callback's refcon. Boxed so its address outlives every
    /// callback; the session is invalidated before it drops.
    awaiting_collection: Box<CompressedAccessUnitsAwaitingCollection>,
    coded_extent: (u32, u32),
    nv12_conversion: RgbaTextureToNv12PoolSurfaceConversion,
    /// Resolves each source frame's surface id to the texture it names.
    gpu_context: GpuContextLimitedAccess,
}

/// The GPU pass from a source texture into one of the session pool's NV12
/// surfaces, and the pool surfaces it has imported so far.
struct RgbaTextureToNv12PoolSurfaceConversion {
    color_converter: RhiColorConverter,
    recorder: RhiCommandRecorder,
    /// The colour the pass converts into — what the parameter sets signal,
    /// with any axis they leave absent resolved the way a decoder resolves it.
    resolved_color: ResolvedColorInfo,
    imported_pool_surfaces_by_iosurface_id: HashMap<IOSurfaceID, ImportedIOSurfaceStorageBuffer>,
}

// SAFETY: a compression session may be driven from any thread, one call at a
// time — which `&mut self` on every call guarantees — and the callback state
// is behind a mutex.
unsafe impl Send for VideoToolboxEncodeSession {}

impl VideoToolboxEncodeSession {
    /// Open a session for `request`, refusing by name a property the hardware
    /// encoder does not take.
    pub(super) fn open(
        gpu_context: &GpuContextFullAccess,
        request: &VideoEncodeSessionRequest,
    ) -> Result<Self> {
        ensure_core_video_pixel_format_dictionary_is_initialised();
        let resolved_color = request
            .color_vui
            .unwrap_or_default()
            .resolve_defaults(ColorSpaceKind::Yuv);
        let pool_pixel_format = match resolved_color.range {
            RangeId::Full => PixelFormat::Nv12FullRange,
            RangeId::Limited => PixelFormat::Nv12VideoRange,
        };
        let nv12_conversion = RgbaTextureToNv12PoolSurfaceConversion {
            color_converter: gpu_context
                .create_color_converter(PixelFormat::Rgba32, pool_pixel_format)?,
            recorder: gpu_context.create_command_recorder("videotoolbox_encode_nv12_conversion")?,
            resolved_color,
            imported_pool_surfaces_by_iosurface_id: HashMap::new(),
        };
        let awaiting_collection = Box::new(CompressedAccessUnitsAwaitingCollection {
            elementary_stream: request.elementary_stream,
            completed: Mutex::new(Vec::new()),
        });
        let encoder_specification = cf_dictionary_of_booleans(&[(
            // SAFETY: a VideoToolbox-exported constant.
            unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder },
            true,
        )]);
        let (width, height) = (
            i32::try_from(request.width).map_err(|_| {
                Error::Configuration(format!("{} is too wide to encode", request.width))
            })?,
            i32::try_from(request.height).map_err(|_| {
                Error::Configuration(format!("{} is too tall to encode", request.height))
            })?,
        );
        let source_image_buffer_attributes =
            nv12_pool_surface_attributes(pool_pixel_format, width, height);

        let mut created: *mut VTCompressionSession = std::ptr::null_mut();
        // SAFETY: the dictionaries are live and hold documented keys; the
        // refcon is the boxed state the callback reads, which outlives the
        // session (see `Drop`); the out-pointer is a stack slot.
        let os_status = unsafe {
            VTCompressionSession::create(
                None,
                width,
                height,
                core_media_codec_type_of(request.elementary_stream),
                Some(encoder_specification.as_opaque()),
                Some(source_image_buffer_attributes.as_opaque()),
                None,
                Some(compression_output_callback),
                std::ptr::from_ref(&*awaiting_collection).cast_mut().cast(),
                NonNull::from(&mut created),
            )
        };
        if os_status != 0 || created.is_null() {
            return Err(Error::GpuError(format!(
                "VideoToolbox has no hardware {:?} encoder for {}x{} (VTCompressionSessionCreate \
                 answered OSStatus {os_status})",
                request.elementary_stream, request.width, request.height
            )));
        }
        // SAFETY: a successful create hands back a +1 session.
        let compression_session = unsafe { CFRetained::from_raw(NonNull::new_unchecked(created)) };
        let session = Self {
            compression_session,
            awaiting_collection,
            coded_extent: coded_extent_videotoolbox_codes(
                request.elementary_stream,
                request.width,
                request.height,
            ),
            nv12_conversion,
            gpu_context: gpu_context.host_inner().limited_access(),
        };
        session.set_the_streaming_shape_and_rate(request)?;
        if let Some(color_vui) = request.color_vui {
            session.set_the_colour_the_parameter_sets_signal(color_vui)?;
        }
        // SAFETY: a live session.
        let prepared = unsafe { session.compression_session.prepare_to_encode_frames() };
        if prepared != 0 {
            return Err(videotoolbox_call_failure(
                "VTCompressionSessionPrepareToEncodeFrames",
                prepared,
            ));
        }
        Ok(session)
    }

    /// A fresh NV12 surface from the session's pool holding `source`'s
    /// frame, converted on the GPU and complete by the time this returns.
    fn pool_surface_holding(
        &mut self,
        source: &VideoEncodeSourceSurface<'_>,
    ) -> Result<CFRetained<CVPixelBuffer>> {
        // SAFETY: a live session; the pool exists once frames can be encoded.
        let pool = unsafe { self.compression_session.pixel_buffer_pool() }.ok_or_else(|| {
            Error::GpuError("the compression session offers no pixel buffer pool".into())
        })?;
        let pool_pixel_buffer = pixel_buffer_from_pool(&pool)?;
        let pool_iosurface =
            CVPixelBufferGetIOSurface(Some(&pool_pixel_buffer)).ok_or_else(|| {
                Error::GpuError("the compression session's pool handed out no IOSurface".into())
            })?;
        let source_registration = self
            .gpu_context
            .resolve_texture_registration_by_surface_id(
                source.surface_id,
                source.texture_layout,
                source.width,
                source.height,
            )?;
        let conversion = &mut self.nv12_conversion;
        if !conversion
            .imported_pool_surfaces_by_iosurface_id
            .contains_key(&pool_iosurface.id())
        {
            let imported = self
                .gpu_context
                .escalate(|full| full.import_iosurface_as_storage_buffer(&pool_iosurface))?;
            conversion
                .imported_pool_surfaces_by_iosurface_id
                .insert(pool_iosurface.id(), imported);
        }
        let destination = &conversion.imported_pool_surfaces_by_iosurface_id[&pool_iosurface.id()];
        let kernel = conversion.color_converter.prepare_image_to_nv12_buffer(
            source_registration.texture(),
            destination.storage_buffer(),
            destination.nv12_source_layout()?,
            &conversion.resolved_color,
        )?;
        let recorder = &mut conversion.recorder;
        recorder.begin()?;
        // A failure between `begin()` and the submit leaves the recorder mid-
        // recording; abandoning it is what lets the next frame begin again.
        let recorded = (|| -> Result<()> {
            recorder.record_image_barrier(
                source_registration.texture(),
                source_registration.current_layout(),
                LAYOUT_THE_CONVERSION_SAMPLES_IN,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::SHADER_SAMPLED_READ,
            )?;
            recorder.record_dispatch(
                &kernel,
                source
                    .width
                    .div_ceil(CONVERSION_BLOCK_WIDTH)
                    .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                source
                    .height
                    .div_ceil(CONVERSION_BLOCK_HEIGHT)
                    .div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                1,
            )?;
            recorder.record_buffer_barrier(
                destination.storage_buffer(),
                VulkanStage::COMPUTE_SHADER,
                VulkanStage::HOST,
                VulkanAccess::MEMORY_WRITE,
                VulkanAccess::HOST_READ,
            )?;
            recorder.submit_and_wait()
        })();
        if let Err(failure) = recorded {
            recorder.abort_recording();
            return Err(failure);
        }
        source_registration.update_layout(LAYOUT_THE_CONVERSION_SAMPLES_IN);
        Ok(pool_pixel_buffer)
    }

    /// No reordering, a sync point on the requested cadence, and the rate
    /// control the knobs ask for.
    fn set_the_streaming_shape_and_rate(&self, request: &VideoEncodeSessionRequest) -> Result<()> {
        let frames_between_sync_points = request
            .frames_per_second
            .saturating_mul(request.knobs.keyframe_interval_seconds);
        // SAFETY: VideoToolbox-exported constants.
        let profile_level = unsafe {
            match request.elementary_stream {
                VideoCodecElementaryStream::H264 => kVTProfileLevel_H264_High_AutoLevel,
                VideoCodecElementaryStream::H265 => kVTProfileLevel_HEVC_Main_AutoLevel,
            }
        };
        // SAFETY: each key is a VideoToolbox-exported constant.
        let (
            real_time,
            allow_frame_reordering,
            profile_level_key,
            expected_frame_rate,
            max_key_frame_interval,
            max_key_frame_interval_duration,
        ) = unsafe {
            (
                kVTCompressionPropertyKey_RealTime,
                kVTCompressionPropertyKey_AllowFrameReordering,
                kVTCompressionPropertyKey_ProfileLevel,
                kVTCompressionPropertyKey_ExpectedFrameRate,
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration,
            )
        };
        self.set_property(real_time, objc2_core_foundation::CFBoolean::new(true))?;
        self.set_property(
            allow_frame_reordering,
            objc2_core_foundation::CFBoolean::new(false),
        )?;
        self.set_property(profile_level_key, profile_level)?;
        self.set_property(
            expected_frame_rate,
            &CFNumber::new_i64(i64::from(request.frames_per_second)),
        )?;
        self.set_property(
            max_key_frame_interval,
            &CFNumber::new_i64(i64::from(frames_between_sync_points)),
        )?;
        self.set_property(
            max_key_frame_interval_duration,
            &CFNumber::new_f64(f64::from(request.knobs.keyframe_interval_seconds)),
        )?;
        match request.knobs.bitrate_bps {
            // SAFETY: a VideoToolbox-exported constant.
            Some(bitrate_bps) => self.set_property(
                unsafe { kVTCompressionPropertyKey_AverageBitRate },
                &CFNumber::new_i64(i64::from(bitrate_bps)),
            ),
            // SAFETY: as above.
            None => self.set_property(
                unsafe { kVTCompressionPropertyKey_Quality },
                &CFNumber::new_f64(BALANCED_CONSTANT_QUALITY),
            ),
        }
    }

    /// The colour VideoToolbox writes into the parameter sets' VUI. The range
    /// is the one VideoToolbox converts the source into, which the parameter
    /// sets state for themselves.
    fn set_the_colour_the_parameter_sets_signal(&self, color_vui: H273ColorVui) -> Result<()> {
        // SAFETY: VideoToolbox-exported constants.
        let axes = unsafe {
            [
                (
                    "primaries",
                    kVTCompressionPropertyKey_ColorPrimaries,
                    color_vui.primaries,
                    CVColorPrimariesGetStringForIntegerCodePoint
                        as extern "C-unwind" fn(c_int) -> Option<CFRetained<CFString>>,
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
            let core_video_string: CFRetained<CFString> =
                core_video_string_for(c_int::from(code_point)).ok_or_else(|| {
                    Error::Configuration(format!(
                        "VideoToolbox cannot signal H.273 {axis_name} {code_point}: CoreVideo \
                         names no colour for it"
                    ))
                })?;
            self.set_property(property_key, &core_video_string)?;
        }
        Ok(())
    }

    fn set_property(&self, key: &CFString, value: &CFType) -> Result<()> {
        // SAFETY: a live session, a VideoToolbox property key and a value of
        // the type that key documents.
        let os_status =
            unsafe { VTSessionSetProperty(&self.compression_session, key, Some(value)) };
        if os_status != 0 {
            return Err(Error::Configuration(format!(
                "VideoToolbox's hardware encoder refused {key} = {value:?} (OSStatus {os_status})"
            )));
        }
        Ok(())
    }
}

impl VideoEncodeSession for VideoToolboxEncodeSession {
    fn coded_extent(&self) -> (u32, u32) {
        self.coded_extent
    }

    fn encode_published_surface(
        &mut self,
        source: &VideoEncodeSourceSurface<'_>,
    ) -> Result<Vec<EncodedVideoAccessUnitFromSession>> {
        let pool_pixel_buffer = self.pool_surface_holding(source)?;
        let presentation_time_stamp = CMTime {
            value: source.timestamp_ns,
            timescale: NANOSECONDS_TIMESCALE,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };

        // SAFETY: a live session and pool pixel buffer; no per-frame
        // properties, refcon or info out-pointer.
        let submitted = unsafe {
            self.compression_session.encode_frame(
                &pool_pixel_buffer,
                presentation_time_stamp,
                kCMTimeInvalid,
                None,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if submitted != 0 {
            return Err(videotoolbox_call_failure(
                "VTCompressionSessionEncodeFrame",
                submitted,
            ));
        }
        // With reordering off this completes the frame just submitted, and
        // the callback has run for it by the time the call returns.
        // SAFETY: a live session.
        let completed = unsafe {
            self.compression_session
                .complete_frames(presentation_time_stamp)
        };
        if completed != 0 {
            return Err(videotoolbox_call_failure(
                "VTCompressionSessionCompleteFrames",
                completed,
            ));
        }
        std::mem::take(&mut *self.awaiting_collection.completed.lock())
            .into_iter()
            .collect()
    }
}

impl Drop for VideoToolboxEncodeSession {
    fn drop(&mut self) {
        // SAFETY: a live session; after this no callback reads the refcon.
        unsafe { self.compression_session.invalidate() };
    }
}

/// The extent VideoToolbox codes a `width` × `height` source at, before the
/// parameter sets' conformance crop: whole 16-sample macroblocks for H.264,
/// whole 16-sample minimum coding blocks for H.265 on Apple's encoders.
fn coded_extent_videotoolbox_codes(
    elementary_stream: VideoCodecElementaryStream,
    width: u32,
    height: u32,
) -> (u32, u32) {
    let block_size = match elementary_stream {
        VideoCodecElementaryStream::H264 | VideoCodecElementaryStream::H265 => 16,
    };
    (
        width.next_multiple_of(block_size),
        height.next_multiple_of(block_size),
    )
}

/// The attributes of the pool the session hands out source surfaces from:
/// IOSurface-backed NV12 at the session's extent.
fn nv12_pool_surface_attributes(
    pool_pixel_format: PixelFormat,
    width: i32,
    height: i32,
) -> CFRetained<CFDictionary<CFString, CFType>> {
    let pixel_format_number =
        CFNumber::new_i64(i64::from(pool_pixel_format.as_cv_pixel_format_type()));
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

fn pixel_buffer_from_pool(pool: &CVPixelBufferPool) -> Result<CFRetained<CVPixelBuffer>> {
    let mut pixel_buffer: *mut CVPixelBuffer = std::ptr::null_mut();
    // SAFETY: a live pool and a stack out-pointer.
    let created =
        unsafe { CVPixelBufferPoolCreatePixelBuffer(None, pool, NonNull::from(&mut pixel_buffer)) };
    if created != kCVReturnSuccess || pixel_buffer.is_null() {
        return Err(Error::GpuError(format!(
            "the compression session's pool handed out no pixel buffer (CVReturn {created})"
        )));
    }
    // SAFETY: a successful create hands back a +1 buffer.
    Ok(unsafe { CFRetained::from_raw(NonNull::new_unchecked(pixel_buffer)) })
}

/// Runs on a VideoToolbox thread, or on the caller's inside
/// `VTCompressionSessionCompleteFrames`, once per frame.
unsafe extern "C-unwind" fn compression_output_callback(
    output_callback_ref_con: *mut c_void,
    _source_frame_ref_con: *mut c_void,
    os_status: i32,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: *mut CMSampleBuffer,
) {
    // SAFETY: the refcon is the session's boxed state, alive until the
    // session is invalidated, which happens before it drops.
    let awaiting_collection =
        unsafe { &*output_callback_ref_con.cast::<CompressedAccessUnitsAwaitingCollection>() };
    let completed_access_unit = if os_status != 0 {
        Err(videotoolbox_call_failure(
            "the compression output",
            os_status,
        ))
    } else if info_flags.contains(VTEncodeInfoFlags::FrameDropped) || sample_buffer.is_null() {
        return;
    } else {
        // SAFETY: a non-null sample buffer VideoToolbox handed the callback,
        // alive for the callback's duration.
        let sample_buffer = unsafe { &*sample_buffer };
        annex_b_access_unit_from_compressed_sample(
            awaiting_collection.elementary_stream,
            sample_buffer,
        )
    };
    awaiting_collection
        .completed
        .lock()
        .push(completed_access_unit);
}

/// The Annex-B access unit a compressed sample is, with the format
/// description's parameter sets in front when it is a sync point.
fn annex_b_access_unit_from_compressed_sample(
    elementary_stream: VideoCodecElementaryStream,
    sample_buffer: &CMSampleBuffer,
) -> Result<EncodedVideoAccessUnitFromSession> {
    // SAFETY: a live sample buffer.
    let (block_buffer, format_description, presentation_time_stamp) = unsafe {
        (
            sample_buffer.data_buffer(),
            sample_buffer.format_description(),
            sample_buffer.presentation_time_stamp(),
        )
    };
    let block_buffer = block_buffer
        .ok_or_else(|| Error::GpuError("a compressed sample came back with no data".into()))?;
    let format_description = format_description.ok_or_else(|| {
        Error::GpuError("a compressed sample came back with no format description".into())
    })?;
    let length_prefixed_sample = bytes_of_block_buffer(&block_buffer)?;
    let (parameter_sets, length_prefix_bytes) =
        parameter_sets_of_format_description(elementary_stream, &format_description)?;
    let is_sync_point = is_sync_sample(sample_buffer);
    let parameter_sets_in_front: Vec<&[u8]> = if is_sync_point {
        parameter_sets
            .iter()
            .map(|parameter_set| &**parameter_set)
            .collect()
    } else {
        Vec::new()
    };
    Ok(EncodedVideoAccessUnitFromSession {
        annex_b_access_unit_bytes: annex_b_access_unit_from_length_prefixed_sample(
            &parameter_sets_in_front,
            &length_prefixed_sample,
            length_prefix_bytes,
        )?,
        is_sync_point,
        timestamp_ns: nanoseconds_of(presentation_time_stamp),
    })
}

fn bytes_of_block_buffer(block_buffer: &CMBlockBuffer) -> Result<Vec<u8>> {
    // SAFETY: a live block buffer.
    let byte_count = unsafe { block_buffer.data_length() };
    let mut bytes = vec![0u8; byte_count];
    if byte_count == 0 {
        return Ok(bytes);
    }
    // SAFETY: the destination is `byte_count` writable bytes.
    let copied = unsafe {
        block_buffer.copy_data_bytes(
            0,
            byte_count,
            NonNull::new_unchecked(bytes.as_mut_ptr()).cast(),
        )
    };
    if copied != 0 {
        return Err(videotoolbox_call_failure(
            "CMBlockBufferCopyDataBytes",
            copied,
        ));
    }
    Ok(bytes)
}

/// Every parameter set a format description holds, in its order, and the
/// width of the length prefix its samples carry.
pub(super) fn parameter_sets_of_format_description(
    elementary_stream: VideoCodecElementaryStream,
    format_description: &CMFormatDescription,
) -> Result<(Vec<Vec<u8>>, usize)> {
    let parameter_set_at_index = |index: usize,
                                  pointer_out: *mut *const u8,
                                  size_out: *mut usize,
                                  count_out: *mut usize,
                                  header_length_out: *mut c_int| {
        // SAFETY: a live format description; every out-pointer is a valid
        // stack slot or null, which the call documents as "not wanted".
        unsafe {
            match elementary_stream {
                VideoCodecElementaryStream::H264 => {
                    CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                        format_description,
                        index,
                        pointer_out,
                        size_out,
                        count_out,
                        header_length_out,
                    )
                }
                VideoCodecElementaryStream::H265 => {
                    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                        format_description,
                        index,
                        pointer_out,
                        size_out,
                        count_out,
                        header_length_out,
                    )
                }
            }
        }
    };
    let (mut parameter_set_count, mut nal_unit_header_length) = (0usize, 0 as c_int);
    let counted = parameter_set_at_index(
        0,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut parameter_set_count,
        &mut nal_unit_header_length,
    );
    if counted != 0 {
        return Err(videotoolbox_call_failure(
            "reading the format description's parameter sets",
            counted,
        ));
    }
    let mut parameter_sets = Vec::with_capacity(parameter_set_count);
    for index in 0..parameter_set_count {
        let (mut pointer, mut size) = (std::ptr::null::<u8>(), 0usize);
        let read = parameter_set_at_index(
            index,
            &mut pointer,
            &mut size,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if read != 0 || pointer.is_null() {
            return Err(videotoolbox_call_failure(
                "reading one of the format description's parameter sets",
                read,
            ));
        }
        // SAFETY: the pointer is `size` bytes of the format description's own
        // storage, alive while the description is.
        parameter_sets.push(unsafe { std::slice::from_raw_parts(pointer, size) }.to_vec());
    }
    let length_prefix_bytes = usize::try_from(nal_unit_header_length).map_err(|_| {
        Error::GpuError(format!(
            "the format description states a {nal_unit_header_length}-byte NAL length prefix"
        ))
    })?;
    Ok((parameter_sets, length_prefix_bytes))
}

/// A sample is a sync point unless its attachments say `NotSync`.
fn is_sync_sample(sample_buffer: &CMSampleBuffer) -> bool {
    // SAFETY: a live sample buffer; not asking for the array to be created.
    let Some(attachments) = (unsafe { sample_buffer.sample_attachments_array(false) }) else {
        return true;
    };
    // SAFETY: the array's elements are the per-sample attachment
    // dictionaries, per `CMSampleBufferGetSampleAttachmentsArray`.
    let first_sample_attachments = unsafe {
        let attachments: &objc2_core_foundation::CFArray = &attachments;
        if attachments.count() == 0 {
            return true;
        }
        &*attachments.value_at_index(0).cast::<CFDictionary>()
    };
    // SAFETY: a CoreMedia-exported key, read out of a live dictionary.
    let not_sync = unsafe {
        first_sample_attachments.value(std::ptr::from_ref(kCMSampleAttachmentKey_NotSync).cast())
    };
    if not_sync.is_null() {
        return true;
    }
    // SAFETY: `NotSync` is documented as a `CFBoolean`.
    !unsafe { &*not_sync.cast::<objc2_core_foundation::CFBoolean>() }.as_bool()
}

/// A presentation stamp in nanoseconds, when it is a valid one.
fn nanoseconds_of(time: CMTime) -> Option<i64> {
    if !time.flags.contains(CMTimeFlags::Valid) || time.timescale <= 0 {
        return None;
    }
    if time.timescale == NANOSECONDS_TIMESCALE {
        return Some(time.value);
    }
    i64::try_from(
        i128::from(time.value) * i128::from(NANOSECONDS_TIMESCALE) / i128::from(time.timescale),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_extent_codes_at_whole_blocks_and_an_aligned_one_is_unchanged() {
        for elementary_stream in [
            VideoCodecElementaryStream::H264,
            VideoCodecElementaryStream::H265,
        ] {
            assert_eq!(
                coded_extent_videotoolbox_codes(elementary_stream, 320, 180),
                (320, 192)
            );
            assert_eq!(
                coded_extent_videotoolbox_codes(elementary_stream, 1920, 1080),
                (1920, 1088)
            );
            assert_eq!(
                coded_extent_videotoolbox_codes(elementary_stream, 1280, 720),
                (1280, 720)
            );
        }
    }

    #[test]
    fn a_nanosecond_stamp_comes_back_as_the_same_integer() {
        let stamp = CMTime {
            value: 1_234_567_890_123,
            timescale: NANOSECONDS_TIMESCALE,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        assert_eq!(nanoseconds_of(stamp), Some(1_234_567_890_123));
    }

    #[test]
    fn a_stamp_on_another_timescale_converts_to_nanoseconds() {
        let stamp = CMTime {
            value: 90_000,
            timescale: 90_000,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        assert_eq!(nanoseconds_of(stamp), Some(1_000_000_000));
    }

    #[test]
    fn an_invalid_stamp_carries_no_timestamp() {
        // SAFETY: a CoreMedia-exported constant.
        assert_eq!(nanoseconds_of(unsafe { kCMTimeInvalid }), None);
    }
}
