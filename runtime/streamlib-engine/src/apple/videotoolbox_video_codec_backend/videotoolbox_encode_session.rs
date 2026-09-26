// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! One VideoToolbox compression session behind the seam's encode shape.

use std::ffi::{c_int, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;

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
    CVColorPrimariesGetStringForIntegerCodePoint, CVPixelBuffer, CVPixelBufferGetIOSurface,
    CVPixelBufferPool, CVTransferFunctionGetStringForIntegerCodePoint,
    CVYCbCrMatrixGetStringForIntegerCodePoint, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVReturnSuccess,
};
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

use super::{cf_dictionary_of_booleans, core_media_codec_type_of, videotoolbox_call_failure};
use crate::apple::core_video_pixel_format_dictionary::ensure_core_video_pixel_format_dictionary_is_initialised;
use crate::apple::imported_iosurface_storage_buffers_kept_for_recycling::ImportedIOSurfaceStorageBuffersKeptForRecycling;
use crate::core::annex_b_access_unit::annex_b_access_unit_from_length_prefixed_sample;
use crate::core::color::{ColorSpaceKind, H273ColorVui, RangeId, ResolvedColorInfo};
use crate::core::context::{
    EncodedVideoAccessUnitFromSession, GpuContextFullAccess, GpuContextLimitedAccess,
    VideoCodecElementaryStream, VideoEncodeSession, VideoEncodeSessionRequest,
    VideoEncodeSourceSurface,
};
use crate::core::rhi::{PixelFormat, RhiColorConverter, VulkanLayout};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{
    RhiCommandRecorder, VulkanAccess, VulkanStage, image_to_nv12_buffer_dispatch_group_counts,
};

/// `kVTCompressionPropertyKey_Quality` when no bitrate is set, on
/// VideoToolbox's `0.0`–`1.0` scale: the arm's balanced constant-quality
/// point.
const BALANCED_CONSTANT_QUALITY: f64 = 0.75;

/// Nanoseconds per second: the timescale of every presentation stamp this arm
/// hands VideoToolbox.
const NANOSECONDS_TIMESCALE: i32 = 1_000_000_000;

/// The layout a source texture is barriered into before the conversion
/// samples it — the one every sampled descriptor the compute kernel binds
/// declares.
const LAYOUT_THE_CONVERSION_SAMPLES_IN: VulkanLayout = VulkanLayout::SHADER_READ_ONLY_OPTIMAL;

/// What the output callback hands the session: every access unit completed
/// since the last collection, or the failure a frame answered.
struct CompressedAccessUnitsAwaitingCollection {
    elementary_stream: VideoCodecElementaryStream,
    completed: Mutex<Vec<Result<EncodedVideoAccessUnitFromSession>>>,
}

/// A compression session, driven from whichever thread holds it.
struct VideoToolboxCompressionSessionDrivenFromOneThreadAtATime(CFRetained<VTCompressionSession>);

// SAFETY: a compression session may be driven from any thread, one call at a
// time, which the `&mut` receiver of every encode call guarantees.
unsafe impl Send for VideoToolboxCompressionSessionDrivenFromOneThreadAtATime {}

/// A VideoToolbox compression session, coding one published surface at a
/// time and handing back what each completed.
///
/// CoreVideo wraps no `'RGBA'` IOSurface as a pixel buffer, so a source frame
/// is never handed to VideoToolbox as it is: the RHI's colour converter
/// writes it, on the GPU, into an NV12 surface from the session's own pool —
/// in the matrix and range the parameter sets signal — and that is encoded.
pub(super) struct VideoToolboxEncodeSession {
    compression_session: VideoToolboxCompressionSessionDrivenFromOneThreadAtATime,
    /// The output callback's refcon, held until the session is invalidated.
    awaiting_collection: Arc<CompressedAccessUnitsAwaitingCollection>,
    coded_extent: (u32, u32),
    nv12_conversion: RgbaTextureToNv12PoolSurfaceConversion,
    /// The session's own frame clock: each frame's presentation stamp is its
    /// index at the requested rate, so rate control sees a steady cadence
    /// whatever stamps upstream publishes. The source's own stamp rides the
    /// frame's refcon to the access unit.
    frames_submitted: i64,
    nanoseconds_per_frame: i64,
    /// Resolves each source frame's surface id to the texture it names.
    gpu_context: GpuContextLimitedAccess,
}

/// The GPU pass from a source texture into one of the session pool's NV12
/// surfaces.
struct RgbaTextureToNv12PoolSurfaceConversion {
    color_converter: RhiColorConverter,
    recorder: RhiCommandRecorder,
    /// The colour the pass converts into — what the parameter sets signal,
    /// with any axis they leave absent resolved the way a decoder resolves it.
    resolved_color: ResolvedColorInfo,
    imported_pool_surfaces: ImportedIOSurfaceStorageBuffersKeptForRecycling,
}

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
            imported_pool_surfaces: ImportedIOSurfaceStorageBuffersKeptForRecycling::default(),
        };
        let awaiting_collection = Arc::new(CompressedAccessUnitsAwaitingCollection {
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
        let Some(created_compression_session) =
            NonNull::new(created_compression_session).filter(|_| create_os_status == 0)
        else {
            return Err(Error::GpuError(format!(
                "VideoToolbox has no hardware {:?} encoder for {}x{} (VTCompressionSessionCreate \
                 answered OSStatus {create_os_status})",
                request.elementary_stream, request.width, request.height
            )));
        };
        let session = Self {
            // SAFETY: a successful create hands back a +1 session.
            compression_session: VideoToolboxCompressionSessionDrivenFromOneThreadAtATime(unsafe {
                CFRetained::from_raw(created_compression_session)
            }),
            awaiting_collection,
            coded_extent: coded_extent_videotoolbox_codes(
                request.elementary_stream,
                request.width,
                request.height,
            ),
            nv12_conversion,
            frames_submitted: 0,
            nanoseconds_per_frame: i64::from(NANOSECONDS_TIMESCALE)
                / i64::from(request.frames_per_second.max(1)),
            gpu_context: gpu_context.host_inner().limited_access(),
        };
        session.set_the_streaming_shape_and_rate(request)?;
        if let Some(color_vui) = request.color_vui {
            session.set_the_colour_the_parameter_sets_signal(color_vui)?;
        }
        // SAFETY: a live session.
        let prepare_os_status = unsafe { session.compression_session.0.prepare_to_encode_frames() };
        if prepare_os_status != 0 {
            return Err(videotoolbox_call_failure(
                "VTCompressionSessionPrepareToEncodeFrames",
                prepare_os_status,
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
        let pool = unsafe { self.compression_session.0.pixel_buffer_pool() }.ok_or_else(|| {
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
        // Every conversion is waited for before its frame is encoded, so no
        // GPU work reads an import when the set starts over.
        let destination = conversion
            .imported_pool_surfaces
            .imported_for(&self.gpu_context, &pool_iosurface)?;
        let kernel = conversion.color_converter.prepare_image_to_nv12_buffer(
            source_registration.texture(),
            destination.storage_buffer(),
            destination.nv12_source_layout()?,
            &conversion.resolved_color,
        )?;
        let (dispatch_group_x, dispatch_group_y) =
            image_to_nv12_buffer_dispatch_group_counts(source.width, source.height);
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
            recorder.record_dispatch(&kernel, dispatch_group_x, dispatch_group_y, 1)?;
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
        // SAFETY: each key and profile is a VideoToolbox-exported constant.
        unsafe {
            self.set_property(kVTCompressionPropertyKey_RealTime, CFBoolean::new(true))?;
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
                &CFNumber::new_f64(f64::from(request.knobs.keyframe_interval_seconds)),
            )?;
            match request.knobs.bitrate_bps {
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
    fn set_the_colour_the_parameter_sets_signal(&self, color_vui: H273ColorVui) -> Result<()> {
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
            let core_video_string = core_video_string_for(c_int::from(code_point)).ok_or_else(|| {
                Error::Configuration(format!(
                    "VideoToolbox cannot signal H.273 {axis_name} {code_point}: CoreVideo names \
                     no colour for it"
                ))
            })?;
            self.set_property(property_key, &core_video_string)?;
        }
        Ok(())
    }

    fn set_property(&self, key: &CFString, value: &CFType) -> Result<()> {
        // SAFETY: a live session, a VideoToolbox property key and a value of
        // the type that key documents.
        let set_os_status =
            unsafe { VTSessionSetProperty(&self.compression_session.0, key, Some(value)) };
        if set_os_status != 0 {
            return Err(Error::Configuration(format!(
                "VideoToolbox's hardware encoder refused {key} = {value:?} (OSStatus \
                 {set_os_status})"
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
            value: self.frames_submitted * self.nanoseconds_per_frame,
            timescale: NANOSECONDS_TIMESCALE,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        self.frames_submitted += 1;

        // SAFETY: a live session and pool pixel buffer; no per-frame
        // properties or info out-pointer. The frame refcon is the source's
        // stamp as a pointer-sized integer, never dereferenced.
        let encode_os_status = unsafe {
            self.compression_session.0.encode_frame(
                &pool_pixel_buffer,
                presentation_time_stamp,
                kCMTimeInvalid,
                None,
                source.timestamp_ns as isize as *mut c_void,
                std::ptr::null_mut(),
            )
        };
        // With reordering off this completes the frame just submitted, and
        // the callback has run for it by the time the call returns.
        // SAFETY: a live session.
        let complete_os_status = unsafe {
            self.compression_session
                .0
                .complete_frames(presentation_time_stamp)
        };
        // Taken before either status is read, so a failed frame leaves
        // nothing behind for the next call to hand back.
        let completed = std::mem::take(&mut *self.awaiting_collection.completed.lock());
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
}

impl Drop for VideoToolboxEncodeSession {
    fn drop(&mut self) {
        // SAFETY: a live session. Completing first lets every frame in
        // flight reach the callback; after the invalidate none runs, so the
        // refcon may drop with this struct.
        unsafe {
            self.compression_session.0.complete_frames(kCMTimeInvalid);
            self.compression_session.0.invalidate();
        }
    }
}

/// The extent VideoToolbox codes a `width` × `height` source at, before the
/// parameter sets' conformance crop: whole 16-sample macroblocks for H.264,
/// and — measured on Apple's encoders, whose SPS the rig test reads it back
/// from — whole 16-sample coding blocks for H.265.
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
    let mut created_pixel_buffer: *mut CVPixelBuffer = std::ptr::null_mut();
    // SAFETY: a live pool and a stack out-pointer.
    let create_cv_return = unsafe {
        CVPixelBufferPool::create_pixel_buffer(None, pool, NonNull::from(&mut created_pixel_buffer))
    };
    match NonNull::new(created_pixel_buffer).filter(|_| create_cv_return == kCVReturnSuccess) {
        // SAFETY: a successful create hands back a +1 buffer.
        Some(created_pixel_buffer) => Ok(unsafe { CFRetained::from_raw(created_pixel_buffer) }),
        None => Err(Error::GpuError(format!(
            "the compression session's pool handed out no pixel buffer (CVReturn \
             {create_cv_return})"
        ))),
    }
}

/// Runs on a VideoToolbox thread, or on the caller's inside
/// `VTCompressionSessionCompleteFrames`, once per frame. A panic is caught
/// and reported as the frame's failure rather than unwound into
/// VideoToolbox.
unsafe extern "C-unwind" fn compression_output_callback(
    output_callback_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    os_status: i32,
    info_flags: VTEncodeInfoFlags,
    sample_buffer: *mut CMSampleBuffer,
) {
    // SAFETY: the refcon is the session's `Arc`'d state, alive until the
    // session is invalidated, which happens before the session drops it.
    let awaiting_collection =
        unsafe { &*output_callback_ref_con.cast::<CompressedAccessUnitsAwaitingCollection>() };
    let source_timestamp_ns = source_frame_ref_con as isize as i64;
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
            source_timestamp_ns,
        ))
    }))
    .unwrap_or_else(|_| {
        Some(Err(Error::GpuError(
            "the compression output callback panicked converting an access unit".into(),
        )))
    });
    if let Some(completed_access_unit) = completed_access_unit {
        awaiting_collection
            .completed
            .lock()
            .push(completed_access_unit);
    }
}

/// The Annex-B access unit a compressed sample is, with the format
/// description's parameter sets in front when it is a sync point.
fn annex_b_access_unit_from_compressed_sample(
    elementary_stream: VideoCodecElementaryStream,
    sample_buffer: &CMSampleBuffer,
    source_timestamp_ns: i64,
) -> Result<EncodedVideoAccessUnitFromSession> {
    // SAFETY: a live sample buffer.
    let (block_buffer, format_description) = unsafe {
        (
            sample_buffer.data_buffer(),
            sample_buffer.format_description(),
        )
    };
    let block_buffer = block_buffer
        .ok_or_else(|| Error::GpuError("a compressed sample came back with no data".into()))?;
    let format_description = format_description.ok_or_else(|| {
        Error::GpuError("a compressed sample came back with no format description".into())
    })?;
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
        parameter_sets_of_this_stream.length_prefix_bytes,
    )
    .map_err(|refusal| Error::GpuError(format!("VideoToolbox's sample: {refusal}")))?;
    Ok(EncodedVideoAccessUnitFromSession {
        annex_b_access_unit_bytes,
        is_sync_point,
        timestamp_ns: Some(source_timestamp_ns),
    })
}

fn bytes_of_block_buffer(block_buffer: &CMBlockBuffer) -> Result<Vec<u8>> {
    // SAFETY: a live block buffer.
    let byte_count = unsafe { block_buffer.data_length() };
    let mut bytes = vec![0u8; byte_count];
    let Some(destination) = NonNull::new(bytes.as_mut_ptr()).filter(|_| byte_count > 0) else {
        return Ok(bytes);
    };
    // SAFETY: the destination is `byte_count` writable bytes.
    let copy_os_status = unsafe { block_buffer.copy_data_bytes(0, byte_count, destination.cast()) };
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
    length_prefix_bytes: usize,
}

impl<'a> ParameterSetsOfAFormatDescription<'a> {
    /// Count the description's parameter sets and read its length-prefix
    /// width, copying no set.
    fn of(
        elementary_stream: VideoCodecElementaryStream,
        format_description: &'a CMFormatDescription,
    ) -> Result<Self> {
        let mut described = Self {
            elementary_stream,
            format_description,
            parameter_set_count: 0,
            length_prefix_bytes: 0,
        };
        let (mut parameter_set_count, mut nal_unit_header_length) = (0usize, 0 as c_int);
        let count_os_status = described.parameter_set_at_index(
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut parameter_set_count,
            &mut nal_unit_header_length,
        );
        if count_os_status != 0 {
            return Err(videotoolbox_call_failure(
                "reading the format description's parameter sets",
                count_os_status,
            ));
        }
        described.parameter_set_count = parameter_set_count;
        described.length_prefix_bytes = usize::try_from(nal_unit_header_length).map_err(|_| {
            Error::GpuError(format!(
                "the format description states a {nal_unit_header_length}-byte NAL length prefix"
            ))
        })?;
        Ok(described)
    }

    /// Every parameter set, in the description's order.
    fn every_parameter_set(&self) -> Result<Vec<&'a [u8]>> {
        (0..self.parameter_set_count)
            .map(|index| {
                let (mut pointer, mut size) = (std::ptr::null::<u8>(), 0usize);
                let read_os_status = self.parameter_set_at_index(
                    index,
                    &mut pointer,
                    &mut size,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                );
                if read_os_status != 0 || pointer.is_null() {
                    return Err(videotoolbox_call_failure(
                        "reading one of the format description's parameter sets",
                        read_os_status,
                    ));
                }
                // SAFETY: `size` bytes of the format description's own
                // storage, alive while the description is.
                Ok(unsafe { std::slice::from_raw_parts(pointer, size) })
            })
            .collect()
    }

    fn parameter_set_at_index(
        &self,
        index: usize,
        pointer_out: *mut *const u8,
        size_out: *mut usize,
        count_out: *mut usize,
        header_length_out: *mut c_int,
    ) -> i32 {
        // SAFETY: a live format description; every out-pointer is a valid
        // slot or null, which the call documents as "not wanted".
        unsafe {
            match self.elementary_stream {
                VideoCodecElementaryStream::H264 => {
                    CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                        self.format_description,
                        index,
                        pointer_out,
                        size_out,
                        count_out,
                        header_length_out,
                    )
                }
                VideoCodecElementaryStream::H265 => {
                    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                        self.format_description,
                        index,
                        pointer_out,
                        size_out,
                        count_out,
                        header_length_out,
                    )
                }
            }
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
    fn a_negative_source_stamp_survives_the_frame_refcon() {
        let source_timestamp_ns: i64 = -1_234_567_890_123;
        let frame_ref_con = source_timestamp_ns as isize as *mut c_void;
        assert_eq!(frame_ref_con as isize as i64, source_timestamp_ns);
    }
}
