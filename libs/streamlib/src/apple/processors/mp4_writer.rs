// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{
    sync::DEFAULT_SYNC_TOLERANCE_MS, AudioFrame, GpuContext, LinkInput, Result, RuntimeContext,
    StreamError, VideoFrame,
};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, ClassType};
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor,
};
use objc2_core_media::CMTime;
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{NSDictionary, NSNumber};
use objc2_foundation::{NSString, NSURL};
use objc2_io_surface::IOSurface;
use std::path::PathBuf;
use tracing::{debug, error, info, trace};

// FFI bindings for CoreVideo functions
#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    #[allow(dead_code)]
    fn CVPixelBufferGetIOSurface(pixelBuffer: *const CVPixelBuffer) -> *mut IOSurface;
}

/// Configuration for MP4 writer processor.
#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, crate::ConfigDescriptor,
)]
pub struct AppleMp4WriterConfig {
    pub output_path: PathBuf,
    pub sync_tolerance_ms: Option<f64>,
    pub video_codec: Option<String>,
    pub video_bitrate: Option<u32>,
    pub audio_codec: Option<String>,
    pub audio_bitrate: Option<u32>,
}

impl Default for AppleMp4WriterConfig {
    fn default() -> Self {
        Self {
            output_path: PathBuf::from("/tmp/output.mp4"),
            sync_tolerance_ms: None,
            video_codec: Some("avc1".to_string()), // H.264
            video_bitrate: Some(5_000_000),        // 5 Mbps
            audio_codec: Some("aac".to_string()),
            audio_bitrate: Some(128_000), // 128 kbps
        }
    }
}

#[crate::processor(
    name = "Mp4WriterProcessor",
    execution = Reactive,
    description = "Writes stereo audio and video to MP4 file with A/V synchronization",
    unsafe_send
)]
pub struct AppleMp4WriterProcessor {
    #[crate::input(description = "Stereo audio frames to write to MP4")]
    audio: LinkInput<AudioFrame>,

    #[crate::input(description = "Video frames to write to MP4")]
    video: LinkInput<VideoFrame>,

    #[crate::config]
    config: AppleMp4WriterConfig,

    // RuntimeContext for main thread dispatch
    ctx: Option<crate::core::RuntimeContext>,

    // AVFoundation objects (accessed via main thread dispatch)
    writer: Option<Retained<AVAssetWriter>>,
    video_input: Option<Retained<AVAssetWriterInput>>,
    audio_input: Option<Retained<AVAssetWriterInput>>,
    pixel_buffer_adaptor: Option<Retained<AVAssetWriterInputPixelBufferAdaptor>>,

    // Runtime state
    last_video_frame: Option<VideoFrame>,
    #[allow(dead_code)] // Reserved for A/V sync (future implementation)
    last_audio_timestamp_ns: i64,
    #[allow(dead_code)] // Reserved for A/V sync (future implementation)
    last_video_timestamp: f64,
    #[allow(dead_code)] // Reserved for A/V sync (future implementation)
    start_time_set: bool,
    start_time_ns: i64,
    writer_failed: bool,

    sync_tolerance_ms: f64,
    frames_written: u64,
    frames_dropped: u64,
    frames_duplicated: u64,

    // Latest frames for realtime streaming
    #[allow(dead_code)] // Reserved for realtime streaming mode
    latest_video: Option<VideoFrame>,

    video_width: u32,
    video_height: u32,

    // Last written timestamp to ensure monotonic increasing
    #[allow(dead_code)] // Reserved for timestamp validation
    last_written_timestamp_ns: i64,

    // Track last written video frame number to avoid duplicates
    last_written_video_frame_number: Option<u64>,

    // Track total audio samples written for timestamp calculation
    #[allow(dead_code)] // Reserved for audio timestamp calculation
    total_audio_samples_written: u64,

    // GPU context for texture conversion
    gpu_context: Option<GpuContext>,

    // GPU-accelerated RGBA → NV12 conversion
    pixel_transfer: Option<crate::apple::PixelTransferSession>,
}

impl crate::core::ReactiveProcessor for AppleMp4WriterProcessor::Processor {
    fn setup(
        &mut self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            info!("Setting up MP4 writer processor");

            // Store RuntimeContext for main thread dispatch
            self.ctx = Some(ctx.clone());

            self.sync_tolerance_ms = self
                .config
                .sync_tolerance_ms
                .unwrap_or(DEFAULT_SYNC_TOLERANCE_MS);
            self.gpu_context = Some(ctx.gpu.clone());

            // Initialize GPU-accelerated pixel transfer (RGBA → NV12) using RHI device
            let pixel_transfer = crate::apple::PixelTransferSession::new(ctx.gpu.device().clone())?;
            self.pixel_transfer = Some(pixel_transfer);

            // AVAssetWriter initialization will happen in process() on first frame
            // This is because setup() runs before main thread event loop starts,
            // so run_on_runtime_thread_blocking() would deadlock
            info!("MP4 writer setup complete, will initialize AVAssetWriter in process()");
            Ok(())
        })();
        std::future::ready(result)
    }

    fn process(&mut self) -> Result<()> {
        debug!("=== MP4Writer process() called ===");

        // Wait for both audio and video connections before initializing
        if self.writer.is_none() {
            let audio_connected = self.audio.is_connected();
            let video_connected = self.video.is_connected();

            debug!(
                "Connections: audio={}, video={}",
                audio_connected, video_connected
            );

            if !audio_connected || !video_connected {
                debug!(
                    "Waiting for both connections (audio={}, video={})",
                    audio_connected, video_connected
                );
                return Ok(());
            }

            // Both connected, now wait for first video frame to get dimensions
            if let Some(first_video) = self.video.peek() {
                info!(
                    "🎬 INITIALIZING AVAssetWriter with video dimensions: {}x{}",
                    first_video.width(),
                    first_video.height()
                );

                // Initialize writer
                self.initialize_writer()?;

                // Configure video input with dimensions from first frame
                self.configure_video_input(first_video.width(), first_video.height())?;
                info!(
                    "✅ VIDEO INPUT CONFIGURED: {}x{}",
                    first_video.width(),
                    first_video.height()
                );

                // Configure audio input
                self.configure_audio_input()?;
                info!("✅ AUDIO INPUT CONFIGURED");

                // Start the writing session (all inputs configured)
                let writer = self.writer.as_ref().ok_or_else(|| {
                    StreamError::Configuration("AVAssetWriter not initialized".into())
                })?;

                info!("🎬 STARTING AVAssetWriter session...");
                let started = unsafe { writer.startWriting() };
                if !started {
                    self.writer_failed = true;
                    let error_msg = unsafe {
                        writer
                            .error()
                            .map(|e| e.localizedDescription().to_string())
                            .unwrap_or_else(|| "Unknown error".to_string())
                    };
                    return Err(StreamError::Configuration(format!(
                        "Failed to start AVAssetWriter: {}",
                        error_msg
                    )));
                }

                // Get timestamps from both audio and video
                let first_audio_ts = self.audio.peek().map(|a| a.timestamp_ns).unwrap_or(0);

                // Video timestamp is already in nanoseconds
                let first_video_ts = first_video.timestamp_ns;

                // Use the FIRST VIDEO frame timestamp as the session start time
                // This ensures audio and video start at the same point
                // (we'll skip audio frames that came before the first video frame)
                let session_start_ts = first_video_ts;

                let start_time = unsafe { CMTime::new(0, 1_000_000_000) };
                unsafe {
                    writer.startSessionAtSourceTime(start_time);
                }

                // Set start_time_ns to the first video timestamp
                self.start_time_ns = session_start_ts;
                info!("✅ AVAssetWriter session started at time 0");
                info!(
                    "   Audio first frame: {}ns ({:.6}s)",
                    first_audio_ts,
                    first_audio_ts as f64 / 1_000_000_000.0
                );
                info!(
                    "   Video first frame: {}ns ({:.6}s)",
                    first_video_ts,
                    first_video_ts as f64 / 1_000_000_000.0
                );
                info!(
                    "   Session start:     {}ns ({:.6}s) [synced to first video frame]",
                    session_start_ts,
                    session_start_ts as f64 / 1_000_000_000.0
                );
            } else {
                debug!("Waiting for first video frame to get dimensions");
                return Ok(());
            }
        }

        // Check if we have any audio frames to process
        let has_audio = self.audio.peek().is_some();
        debug!("has_audio = {}", has_audio);

        if !has_audio {
            debug!("No audio frames available, skipping process()");
            return Ok(());
        }

        // Process every audio frame immediately
        while let Some(audio) = self.audio.read() {
            trace!(
                "Processing audio frame: timestamp_ns={}, sample_count={}, channels={}",
                audio.timestamp_ns,
                audio.sample_count(),
                audio.channels()
            );

            // IMPORTANT: Check for new video frame for EACH audio frame
            if let Some(video) = self.video.read() {
                debug!(
                    "New video frame received: timestamp_ns={}, frame_number={}, size={}x{}",
                    video.timestamp_ns,
                    video.frame_number,
                    video.width(),
                    video.height()
                );
                self.last_video_frame = Some(video);
            }

            // Use capture timestamp (same clock as video) and make it relative to start
            // This keeps audio and video on the same time base
            let audio_relative_ns = audio.timestamp_ns - self.start_time_ns;

            trace!(
                "Audio timestamps: original={}ns, start={}ns, relative={}ns ({:.6}s)",
                audio.timestamp_ns,
                self.start_time_ns,
                audio_relative_ns,
                audio_relative_ns as f64 / 1_000_000_000.0
            );

            // Skip audio frames that came BEFORE the first video frame (negative timestamps)
            if audio_relative_ns < 0 {
                debug!(
                    "Skipping audio frame before first video (relative_ts={:.6}s)",
                    audio_relative_ns as f64 / 1_000_000_000.0
                );
                continue;
            }

            // Write audio frame independently
            let mut audio_to_write = audio.clone();
            audio_to_write.timestamp_ns = audio_relative_ns;
            trace!(
                "Writing audio: timestamp={}ns ({:.6}s), samples={}, channels={}",
                audio_to_write.timestamp_ns,
                audio_to_write.timestamp_ns as f64 / 1_000_000_000.0,
                audio_to_write.sample_count(),
                audio_to_write.channels()
            );

            self.write_audio_frame(&audio_to_write)?;

            // Write video frame independently if available (only write each frame once)
            if let Some(last_video) = self.last_video_frame.clone() {
                // Check if this is a new video frame (not already written)
                let should_write = match self.last_written_video_frame_number {
                    None => true,                                          // First video frame
                    Some(last_num) => last_video.frame_number != last_num, // New frame
                };

                if should_write {
                    // Calculate relative timestamp from video frame
                    // Both timestamps are now in nanoseconds
                    let video_relative_ns = last_video.timestamp_ns - self.start_time_ns;
                    let video_timestamp_s = video_relative_ns as f64 / 1_000_000_000.0;

                    let mut video_to_write = last_video.clone();
                    video_to_write.timestamp_ns = video_relative_ns;

                    debug!(
                        "Writing video: frame_number={}, relative_ts={:.6}s",
                        video_to_write.frame_number, video_timestamp_s
                    );

                    self.write_video_frame(&video_to_write)?;
                    self.last_written_video_frame_number = Some(last_video.frame_number);
                }
            }
        }

        debug!("=== MP4Writer process() finished ===");
        Ok(())
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        let result = (|| {
            info!("Tearing down MP4 writer processor");

            // No buffering, so nothing to flush - just finalize
            self.finalize_writer()?;

            // Cleanup: AVFoundation objects will be dropped on main thread
            // when self.asset_writer is dropped (happens automatically)

            Ok(())
        })();
        std::future::ready(result)
    }
}

impl AppleMp4WriterProcessor::Processor {
    fn initialize_writer(&mut self) -> Result<()> {
        info!("Initializing MP4 writer for: {:?}", self.config.output_path);

        let ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("RuntimeContext not initialized".into()))?;

        let path_str = self.config.output_path.to_string_lossy().to_string();
        let video_codec = self
            .config
            .video_codec
            .clone()
            .unwrap_or_else(|| "avc1".to_string());
        let audio_codec = self
            .config
            .audio_codec
            .clone()
            .unwrap_or_else(|| "aac".to_string());
        let sync_tolerance_ms = self.sync_tolerance_ms;

        // Dispatch to main thread (which has active run loop) to create AVAssetWriter
        // Returns raw pointer since Retained<AVAssetWriter> is not Send
        info!("Dispatching AVAssetWriter initialization to main thread...");
        let writer_ptr = ctx.run_on_runtime_thread_blocking(move || {
            match Self::initialize_writer_on_main_thread(
                &path_str,
                &video_codec,
                &audio_codec,
                sync_tolerance_ms,
            ) {
                Ok(writer) => {
                    // Convert to raw pointer to send back across thread boundary
                    Ok(Retained::into_raw(writer) as usize)
                }
                Err(e) => Err(e),
            }
        })?;

        // SAFETY: We just created this pointer on the main thread
        let writer = unsafe {
            Retained::retain(writer_ptr as *mut AVAssetWriter).ok_or_else(|| {
                StreamError::Configuration("Failed to retain AVAssetWriter".into())
            })?
        };

        self.writer = Some(writer);

        info!("AVAssetWriter initialized successfully on main thread");
        Ok(())
    }

    fn initialize_writer_on_main_thread(
        path_str: &str,
        video_codec: &str,
        audio_codec: &str,
        sync_tolerance_ms: f64,
    ) -> Result<Retained<AVAssetWriter>> {
        // Delete existing file if it exists (AVAssetWriter requires file to not exist)
        if std::path::Path::new(path_str).exists() {
            info!("Deleting existing file: {}", path_str);
            std::fs::remove_file(path_str).map_err(|e| {
                StreamError::Configuration(format!("Failed to delete existing file: {}", e))
            })?;
        }

        // Create file URL
        let ns_path = NSString::from_str(path_str);
        let url = NSURL::fileURLWithPath(&ns_path);

        // Create AVAssetWriter
        let file_type_str = NSString::from_str("com.apple.quicktime-movie");
        let writer = unsafe {
            match AVAssetWriter::assetWriterWithURL_fileType_error(&url, &file_type_str) {
                Ok(w) => w,
                Err(e) => {
                    error!("Failed to create AVAssetWriter: {:?}", e);
                    return Err(StreamError::GpuError(format!(
                        "Failed to create AVAssetWriter: {:?}",
                        e
                    )));
                }
            }
        };

        info!("AVAssetWriter created successfully");
        info!("Video codec: {:?}", video_codec);
        info!("Audio codec: {:?}", audio_codec);
        info!("Sync tolerance: {:.1}ms", sync_tolerance_ms);

        Ok(writer)
    }

    fn configure_video_input(&mut self, width: u32, height: u32) -> Result<()> {
        if self.video_input.is_some() {
            return Ok(()); // Already configured
        }

        self.video_width = width;
        self.video_height = height;

        let ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("RuntimeContext not initialized".into()))?;

        let writer = self
            .writer
            .take()
            .ok_or_else(|| StreamError::Configuration("AVAssetWriter not initialized".into()))?;
        let writer_ptr = Retained::into_raw(writer) as usize;

        let video_bitrate = self.config.video_bitrate.unwrap_or(5_000_000);
        let video_codec = self
            .config
            .video_codec
            .clone()
            .unwrap_or_else(|| "avc1".to_string());

        // Dispatch to main thread to configure video input
        // Returns raw pointers since Retained types are not Send
        let (video_input_ptr, pixel_buffer_adaptor_ptr) =
            ctx.run_on_runtime_thread_blocking(move || {
                // SAFETY: We just converted this from a Retained
                let writer = unsafe { Retained::retain(writer_ptr as *mut AVAssetWriter).unwrap() };

                match Self::configure_video_input_on_main_thread(
                    writer,
                    width,
                    height,
                    &video_codec,
                    video_bitrate,
                ) {
                    Ok((writer, video_input, pixel_buffer_adaptor)) => {
                        // Convert to raw pointers to send back across thread boundary
                        let _ = Retained::into_raw(writer); // Already leaked, ignore
                        Ok((
                            Retained::into_raw(video_input) as usize,
                            Retained::into_raw(pixel_buffer_adaptor) as usize,
                        ))
                    }
                    Err(e) => Err(e),
                }
            })?;

        // SAFETY: We just created these pointers on the main thread
        let writer = unsafe { Retained::retain(writer_ptr as *mut AVAssetWriter).unwrap() };
        let video_input =
            unsafe { Retained::retain(video_input_ptr as *mut AVAssetWriterInput).unwrap() };
        let pixel_buffer_adaptor = unsafe {
            Retained::retain(pixel_buffer_adaptor_ptr as *mut AVAssetWriterInputPixelBufferAdaptor)
                .unwrap()
        };

        self.writer = Some(writer);
        self.video_input = Some(video_input);
        self.pixel_buffer_adaptor = Some(pixel_buffer_adaptor);

        Ok(())
    }

    fn configure_video_input_on_main_thread(
        writer: Retained<AVAssetWriter>,
        width: u32,
        height: u32,
        video_codec: &str,
        _video_bitrate: u32,
    ) -> Result<(
        Retained<AVAssetWriter>,
        Retained<AVAssetWriterInput>,
        Retained<AVAssetWriterInputPixelBufferAdaptor>,
    )> {
        use objc2::runtime::AnyClass;

        let video_settings_ptr: *mut AnyObject = unsafe {
            let dict_cls: &AnyClass = NSDictionary::<AnyObject, AnyObject>::class();

            // Create codec key/value
            let codec_key = NSString::from_str("AVVideoCodecKey");
            let codec_value = NSString::from_str(video_codec);

            // Create width key/value
            let width_key = NSString::from_str("AVVideoWidthKey");
            let width_value = NSNumber::new_u32(width);

            // Create height key/value
            let height_key = NSString::from_str("AVVideoHeightKey");
            let height_value = NSNumber::new_u32(height);

            // Build main settings dictionary with 3 key-value pairs (codec, width, height)
            // Note: Bitrate is not supported for H.264 in AVAssetWriterInput outputSettings
            let keys = [
                &*codec_key as *const _ as *const AnyObject,
                &*width_key as *const _ as *const AnyObject,
                &*height_key as *const _ as *const AnyObject,
            ];
            let values = [
                &*codec_value as *const _ as *const AnyObject,
                &*width_value as *const _ as *const AnyObject,
                &*height_value as *const _ as *const AnyObject,
            ];

            msg_send![
                dict_cls,
                dictionaryWithObjects: values.as_ptr(),
                forKeys: keys.as_ptr(),
                count: 3usize
            ]
        };

        // Create AVAssetWriterInput with settings
        let media_type = NSString::from_str("vide"); // AVMediaTypeVideo
        let video_settings = unsafe {
            std::mem::transmute::<*mut AnyObject, *const NSDictionary<NSString, AnyObject>>(
                video_settings_ptr,
            )
        };

        let video_input = unsafe {
            AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                &media_type,
                video_settings.as_ref(),
            )
        };

        // Configure for real-time encoding
        unsafe {
            video_input.setExpectsMediaDataInRealTime(true);
        }

        // Add input to writer
        let can_add = unsafe { writer.canAddInput(&video_input) };
        if !can_add {
            return Err(StreamError::Configuration(
                "Cannot add video input to AVAssetWriter".into(),
            ));
        }

        unsafe {
            writer.addInput(&video_input);
        }

        // Create pixel buffer adaptor with nil attributes to let AVAssetWriter choose optimal format
        // Per Apple TN3121, passing nil allows AVAssetWriter to select YUV format for H.264 encoding
        // which is more efficient than BGRA. We'll use VTPixelTransferSession to convert RGBA→YUV.
        let pixel_buffer_adaptor = unsafe {
            AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(
                &video_input,
                None,
            )
        };

        Ok((writer, video_input, pixel_buffer_adaptor))
    }

    fn configure_audio_input(&mut self) -> Result<()> {
        if self.audio_input.is_some() {
            return Ok(()); // Already configured
        }

        let ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("RuntimeContext not initialized".into()))?;

        let writer = self
            .writer
            .take()
            .ok_or_else(|| StreamError::Configuration("AVAssetWriter not initialized".into()))?;
        let writer_ptr = Retained::into_raw(writer) as usize;

        // Dispatch to main thread to configure audio input
        // Returns raw pointer since Retained types are not Send
        let audio_input_ptr = ctx.run_on_runtime_thread_blocking(move || {
            // SAFETY: We just converted this from a Retained
            let writer = unsafe { Retained::retain(writer_ptr as *mut AVAssetWriter).unwrap() };

            match Self::configure_audio_input_on_main_thread(writer) {
                Ok((writer, audio_input)) => {
                    // Convert to raw pointers to send back across thread boundary
                    let _ = Retained::into_raw(writer); // Already leaked, ignore
                    Ok(Retained::into_raw(audio_input) as usize)
                }
                Err(e) => Err(e),
            }
        })?;

        // SAFETY: We just created these pointers on the main thread
        let writer = unsafe { Retained::retain(writer_ptr as *mut AVAssetWriter).unwrap() };
        let audio_input =
            unsafe { Retained::retain(audio_input_ptr as *mut AVAssetWriterInput).unwrap() };

        self.writer = Some(writer);
        self.audio_input = Some(audio_input);

        info!("Audio input configured: stereo 48kHz 16-bit LPCM");
        Ok(())
    }

    fn configure_audio_input_on_main_thread(
        writer: Retained<AVAssetWriter>,
    ) -> Result<(Retained<AVAssetWriter>, Retained<AVAssetWriterInput>)> {
        use objc2::runtime::AnyClass;

        let audio_settings_ptr: *mut AnyObject = unsafe {
            let dict_cls: &AnyClass = NSDictionary::<AnyObject, AnyObject>::class();

            // AVFormatIDKey: kAudioFormatLinearPCM (1819304813 = 'lpcm')
            let format_key = NSString::from_str("AVFormatIDKey");
            let format_value = NSNumber::new_u32(1819304813);

            // AVSampleRateKey: 48000.0
            let sample_rate_key = NSString::from_str("AVSampleRateKey");
            let sample_rate_value = NSNumber::new_f64(48000.0);

            // AVNumberOfChannelsKey: 2 (stereo)
            let channels_key = NSString::from_str("AVNumberOfChannelsKey");
            let channels_value = NSNumber::new_u32(2);

            // AVLinearPCMBitDepthKey: 16 bits per sample
            let bit_depth_key = NSString::from_str("AVLinearPCMBitDepthKey");
            let bit_depth_value = NSNumber::new_u32(16);

            // AVLinearPCMIsFloatKey: NO (integer samples)
            let is_float_key = NSString::from_str("AVLinearPCMIsFloatKey");
            let is_float_value = NSNumber::new_bool(false);

            // AVLinearPCMIsBigEndianKey: NO (little endian)
            let is_big_endian_key = NSString::from_str("AVLinearPCMIsBigEndianKey");
            let is_big_endian_value = NSNumber::new_bool(false);

            // AVLinearPCMIsNonInterleaved: NO (interleaved)
            let is_non_interleaved_key = NSString::from_str("AVLinearPCMIsNonInterleaved");
            let is_non_interleaved_value = NSNumber::new_bool(false);

            // Build settings dictionary with 7 key-value pairs
            let keys = [
                &*format_key as *const _ as *const AnyObject,
                &*sample_rate_key as *const _ as *const AnyObject,
                &*channels_key as *const _ as *const AnyObject,
                &*bit_depth_key as *const _ as *const AnyObject,
                &*is_float_key as *const _ as *const AnyObject,
                &*is_big_endian_key as *const _ as *const AnyObject,
                &*is_non_interleaved_key as *const _ as *const AnyObject,
            ];
            let values = [
                &*format_value as *const _ as *const AnyObject,
                &*sample_rate_value as *const _ as *const AnyObject,
                &*channels_value as *const _ as *const AnyObject,
                &*bit_depth_value as *const _ as *const AnyObject,
                &*is_float_value as *const _ as *const AnyObject,
                &*is_big_endian_value as *const _ as *const AnyObject,
                &*is_non_interleaved_value as *const _ as *const AnyObject,
            ];

            msg_send![
                dict_cls,
                dictionaryWithObjects: values.as_ptr(),
                forKeys: keys.as_ptr(),
                count: 7usize
            ]
        };

        // Create AVAssetWriterInput with settings
        let media_type = NSString::from_str("soun"); // AVMediaTypeAudio
        let audio_settings = unsafe {
            std::mem::transmute::<*mut AnyObject, *const NSDictionary<NSString, AnyObject>>(
                audio_settings_ptr,
            )
        };

        let audio_input = unsafe {
            AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                &media_type,
                audio_settings.as_ref(),
            )
        };

        // Configure for real-time encoding
        unsafe {
            audio_input.setExpectsMediaDataInRealTime(true);
        }

        // Add input to writer
        let can_add = unsafe { writer.canAddInput(&audio_input) };
        if !can_add {
            return Err(StreamError::Configuration(
                "Cannot add audio input to AVAssetWriter".into(),
            ));
        }

        unsafe {
            writer.addInput(&audio_input);
        }

        Ok((writer, audio_input))
    }

    #[allow(dead_code)]
    fn write_synced_frame(&mut self, audio: AudioFrame, video: VideoFrame) -> Result<()> {
        // Initialize AVAssetWriter on first frame (lazy initialization)
        if self.writer.is_none() {
            self.initialize_writer()?;
        }

        // Configure video input on first video frame
        if self.video_input.is_none() {
            self.configure_video_input(video.width(), video.height())?;
        }

        // Configure audio input on first audio frame
        if self.audio_input.is_none() {
            self.configure_audio_input()?;
        }

        // Start writing session only when BOTH inputs are configured
        if self.video_input.is_some()
            && self.audio_input.is_some()
            && !self.start_time_set
            && !self.writer_failed
        {
            let writer = self.writer.as_ref().ok_or_else(|| {
                StreamError::Configuration("AVAssetWriter not initialized".into())
            })?;

            info!("Both audio and video inputs configured, starting AVAssetWriter session...");

            let started = unsafe { writer.startWriting() };
            if !started {
                self.writer_failed = true;
                let error_msg = unsafe {
                    writer
                        .error()
                        .map(|e| e.localizedDescription().to_string())
                        .unwrap_or_else(|| "Unknown error".to_string())
                };
                return Err(StreamError::Configuration(format!(
                    "Failed to start AVAssetWriter: {}",
                    error_msg
                )));
            }

            // Start session at source time (use audio timestamp as reference)
            let start_time = unsafe { CMTime::new(audio.timestamp_ns, 1_000_000_000) };

            unsafe {
                writer.startSessionAtSourceTime(start_time);
            }

            // Set start time
            self.start_time_ns = audio.timestamp_ns;
            self.start_time_set = true;

            info!(
                "AVAssetWriter session started at timestamp {}ns",
                audio.timestamp_ns
            );
        }

        // If not both inputs are ready yet, skip this frame
        if self.video_input.is_none() || self.audio_input.is_none() {
            return Ok(());
        }

        // Check if pixel buffer pool is ready (becomes available after startWriting())
        if let Some(adaptor) = self.pixel_buffer_adaptor.as_ref() {
            let pool_ready = unsafe { adaptor.pixelBufferPool().is_some() };
            if !pool_ready {
                // Pool not ready yet, wait for next frame
                debug!("Pixel buffer pool not ready, skipping frame");
                return Ok(());
            }
        }

        // No sync logic - just write every audio frame with paired video frame
        debug!(
            "Writing frames: video={:.3}s audio={:.3}s",
            video.timestamp_ns as f64 / 1_000_000_000.0,
            audio.timestamp_ns as f64 / 1_000_000_000.0
        );

        self.write_video_frame(&video)?;
        self.write_audio_frame(&audio)?;

        self.last_video_frame = Some(video.clone());
        self.last_video_timestamp = video.timestamp_ns as f64 / 1_000_000_000.0;
        self.last_audio_timestamp_ns = audio.timestamp_ns;
        self.frames_written += 1;

        Ok(())
    }

    fn write_video_frame(&self, frame: &VideoFrame) -> Result<()> {
        let pixel_buffer_adaptor = self.pixel_buffer_adaptor.as_ref().ok_or_else(|| {
            StreamError::Configuration("Pixel buffer adaptor not initialized".into())
        })?;

        let video_input = self
            .video_input
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Video input not initialized".into()))?;

        // Check if video input is ready for more data
        let is_ready = unsafe { video_input.isReadyForMoreMediaData() };
        if !is_ready {
            debug!("Video input not ready for more data, skipping frame");
            return Ok(());
        }

        let pixel_transfer = self.pixel_transfer.as_ref().ok_or_else(|| {
            StreamError::Configuration("PixelTransferSession not initialized".into())
        })?;

        // Step 1: GPU-accelerated conversion to NV12 using VTPixelTransferSession
        // This creates a new NV12 CVPixelBuffer from the buffer-backed VideoFrame
        let nv12_pixel_buffer_ptr = pixel_transfer.convert_buffer_to_nv12(frame.buffer())?;

        // Wrap in Retained for automatic memory management
        let pixel_buffer = unsafe { objc2::rc::Retained::from_raw(nv12_pixel_buffer_ptr).unwrap() };

        // Step 2: Create CMTime for presentation timestamp
        let timestamp_ns = frame.timestamp_ns;
        let presentation_time = unsafe { CMTime::new(timestamp_ns, 1_000_000_000) };

        trace!(
            "Appending video to AVAssetWriter: timestamp={:.6}s ({}ns), size={}x{}",
            timestamp_ns as f64 / 1_000_000_000.0,
            timestamp_ns,
            frame.width(),
            frame.height()
        );

        // Step 3: Append pixel buffer to adaptor
        let success = unsafe {
            pixel_buffer_adaptor
                .appendPixelBuffer_withPresentationTime(&pixel_buffer, presentation_time)
        };

        if !success {
            return Err(StreamError::GpuError(
                "Failed to append pixel buffer to adaptor".into(),
            ));
        }

        trace!("Video appended successfully");
        Ok(())
    }

    fn write_audio_frame(&self, frame: &AudioFrame) -> Result<()> {
        let audio_input = self
            .audio_input
            .as_ref()
            .ok_or_else(|| StreamError::Configuration("Audio input not initialized".into()))?;

        // Check if audio input is ready for more data
        let is_ready = unsafe { audio_input.isReadyForMoreMediaData() };
        if !is_ready {
            debug!("Audio input not ready for more data, skipping frame");
            return Ok(());
        }

        // AudioFrame stores samples as Vec<f32> with interleaved channels
        // We need to convert f32 → i16 PCM for LPCM format
        let total_samples = frame.samples.len();
        let num_channels = frame.channels();
        let num_samples_per_channel = frame.sample_count(); // This is samples.len() / CHANNELS

        info!("Writing audio frame: sample_rate={}, channels={}, total_samples={}, num_frames={}, duration_ms={:.2}",
            frame.sample_rate, num_channels, total_samples, num_samples_per_channel,
            (num_samples_per_channel as f64 / frame.sample_rate as f64) * 1000.0);
        let mut pcm_data: Vec<i16> = Vec::with_capacity(total_samples);

        for sample in frame.samples.iter() {
            // Clamp to [-1.0, 1.0] and convert to i16
            let clamped = sample.max(-1.0).min(1.0);
            let pcm_sample = (clamped * 32767.0) as i16;
            pcm_data.push(pcm_sample);
        }

        let byte_count = pcm_data.len() * 2;

        // Create CMBlockBuffer - we need to use raw FFI since objc2 doesn't fully wrap this
        use objc2_core_media::CMBlockBuffer;
        use std::ptr::NonNull;

        let mut block_buffer_ptr: *mut CMBlockBuffer = std::ptr::null_mut();
        let block_buffer_out = NonNull::new(&mut block_buffer_ptr as *mut _).ok_or_else(|| {
            StreamError::GpuError("Failed to create NonNull for block buffer".into())
        })?;

        // Create CMBlockBuffer that allocates its own memory and copies our data
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,                 // structure_allocator (use default allocator)
                std::ptr::null_mut(), // memoryBlock = NULL means CMBlockBuffer allocates memory
                byte_count,
                None,             // block_allocator (use default)
                std::ptr::null(), // custom_block_source
                0,                // offset_to_data
                byte_count,       // data_length
                0,                // flags
                block_buffer_out,
            )
        };

        if status != 0 {
            return Err(StreamError::GpuError(format!(
                "Failed to create CMBlockBuffer: status {}",
                status
            )));
        }

        let block_buffer = unsafe {
            objc2::rc::Retained::retain(block_buffer_ptr)
                .ok_or_else(|| StreamError::GpuError("CMBlockBuffer is null".into()))?
        };

        // Copy PCM data into the CMBlockBuffer
        #[allow(non_snake_case, clashing_extern_declarations)]
        extern "C" {
            fn CMBlockBufferReplaceDataBytes(
                sourceBytes: *const std::ffi::c_void,
                destinationBuffer: *const CMBlockBuffer,
                offsetIntoDestination: usize,
                dataLength: usize,
            ) -> i32;
        }

        let copy_status = unsafe {
            CMBlockBufferReplaceDataBytes(
                pcm_data.as_ptr() as *const std::ffi::c_void,
                &*block_buffer as *const _,
                0,
                byte_count,
            )
        };

        if copy_status != 0 {
            return Err(StreamError::GpuError(format!(
                "Failed to copy PCM data to CMBlockBuffer: status {}",
                copy_status
            )));
        }

        // Create timing info
        use objc2_core_media::CMTime;
        let presentation_time = unsafe { CMTime::new(frame.timestamp_ns, 1_000_000_000) };

        let _duration = unsafe {
            CMTime::new(
                num_samples_per_channel as i64 * 1_000_000_000 / frame.sample_rate as i64,
                1_000_000_000,
            )
        };

        // Create CMSampleBuffer using raw FFI
        use objc2_core_media::CMSampleBuffer;

        let mut sample_buffer_ptr: *mut CMSampleBuffer = std::ptr::null_mut();
        let _sample_buffer_out =
            NonNull::new(&mut sample_buffer_ptr as *mut _).ok_or_else(|| {
                StreamError::GpuError("Failed to create NonNull for sample buffer".into())
            })?;

        // We need to use CMAudioSampleBufferCreateWithPacketDescriptions
        // But first we need the format description which is already set in the audio input
        // Actually, we can get it from the audio input's outputSettings
        // For now, let's create a simple one inline using raw FFI

        // Create AudioStreamBasicDescription manually
        #[repr(C)]
        #[allow(non_snake_case)]
        struct AudioStreamBasicDescription {
            mSampleRate: f64,
            mFormatID: u32,
            mFormatFlags: u32,
            mBytesPerPacket: u32,
            mFramesPerPacket: u32,
            mBytesPerFrame: u32,
            mChannelsPerFrame: u32,
            mBitsPerChannel: u32,
            mReserved: u32,
        }

        // Create ASBD from frame metadata (sample_rate, channels)
        // NOTE: This must match what we told AVAssetWriterInput in configure_audio_input
        // Currently AVAssetWriterInput is hardcoded to 48kHz stereo
        let bytes_per_frame = (num_channels * 2) as u32; // 2 bytes per sample (16-bit)
        let asbd = AudioStreamBasicDescription {
            mSampleRate: frame.sample_rate as f64,
            mFormatID: 0x6c70636d, // 'lpcm'
            mFormatFlags: 0xC, // kLinearPCMFormatFlagIsSignedInteger | kLinearPCMFormatFlagIsPacked
            mBytesPerPacket: bytes_per_frame,
            mFramesPerPacket: 1,
            mBytesPerFrame: bytes_per_frame,
            mChannelsPerFrame: num_channels as u32,
            mBitsPerChannel: 16,
            mReserved: 0,
        };

        // Create CMAudioFormatDescription using the ASBD
        extern "C" {
            fn CMAudioFormatDescriptionCreate(
                allocator: *const std::ffi::c_void,
                asbd: *const AudioStreamBasicDescription,
                layoutSize: usize,
                layout: *const std::ffi::c_void,
                magicCookieSize: usize,
                magicCookie: *const std::ffi::c_void,
                extensions: *const std::ffi::c_void,
                formatDescriptionOut: *mut *const std::ffi::c_void,
            ) -> i32;
        }

        use objc2_core_media::CMFormatDescription;
        let mut format_desc_ptr: *const std::ffi::c_void = std::ptr::null();

        let status = unsafe {
            CMAudioFormatDescriptionCreate(
                std::ptr::null(), // allocator
                &asbd as *const _,
                0,                // layoutSize
                std::ptr::null(), // layout
                0,                // magicCookieSize
                std::ptr::null(), // magicCookie
                std::ptr::null(), // extensions
                &mut format_desc_ptr as *mut _,
            )
        };

        if status != 0 {
            return Err(StreamError::GpuError(format!(
                "Failed to create CMAudioFormatDescription: status {}",
                status
            )));
        }

        let _format_desc = unsafe {
            objc2::rc::Retained::retain(format_desc_ptr as *mut CMFormatDescription)
                .ok_or_else(|| StreamError::GpuError("Format description is null".into()))?
        };

        // Use CMAudioSampleBufferCreateWithPacketDescriptions which is designed for audio
        extern "C" {
            fn CMAudioSampleBufferCreateWithPacketDescriptions(
                allocator: *const std::ffi::c_void,
                dataBuffer: *const CMBlockBuffer,
                dataReady: bool,
                makeDataReadyCallback: *const std::ffi::c_void,
                makeDataReadyRefcon: *const std::ffi::c_void,
                formatDescription: *const CMFormatDescription,
                numSamples: i64,
                presentationTimeStamp: CMTime,
                packetDescriptions: *const AudioStreamPacketDescription,
                sampleBufferOut: *mut *mut CMSampleBuffer,
            ) -> i32;
        }

        #[repr(C)]
        #[allow(non_snake_case)] // Apple FFI struct - matches CoreAudio naming
        struct AudioStreamPacketDescription {
            mStartOffset: i64,
            mVariableFramesInPacket: u32,
            mDataByteSize: u32,
        }

        let status = unsafe {
            CMAudioSampleBufferCreateWithPacketDescriptions(
                std::ptr::null(),
                &*block_buffer as *const _,
                true,             // dataReady
                std::ptr::null(), // makeDataReadyCallback
                std::ptr::null(), // makeDataReadyRefcon
                format_desc_ptr as *const CMFormatDescription,
                num_samples_per_channel as i64,
                presentation_time,
                std::ptr::null(), // packetDescriptions (NULL for CBR formats like LPCM)
                &mut sample_buffer_ptr as *mut _,
            )
        };

        if status != 0 {
            return Err(StreamError::GpuError(format!(
                "Failed to create audio CMSampleBuffer: status {}",
                status
            )));
        }

        let sample_buffer = unsafe {
            objc2::rc::Retained::retain(sample_buffer_ptr)
                .ok_or_else(|| StreamError::GpuError("Sample buffer is null".into()))?
        };

        trace!("Appending audio to AVAssetWriter: timestamp={}ns ({:.6}s), samples={}, channels={}, rate={}Hz",
            frame.timestamp_ns, frame.timestamp_ns as f64 / 1_000_000_000.0,
            num_samples_per_channel, num_channels, frame.sample_rate);

        // Append to audio input
        let success = unsafe { audio_input.appendSampleBuffer(&sample_buffer) };

        if !success {
            return Err(StreamError::GpuError(
                "Failed to append audio sample buffer".into(),
            ));
        }

        trace!("Audio appended successfully");

        debug!(
            "Wrote audio frame: {} samples per channel at timestamp {}ns",
            num_samples_per_channel, frame.timestamp_ns
        );

        // Keep data alive until sample buffer is appended
        drop(pcm_data);

        Ok(())
    }

    fn finalize_writer(&mut self) -> Result<()> {
        // Only finalize if writer was initialized
        if self.writer.is_none() {
            return Ok(());
        }

        info!("Finalizing MP4 file: {:?}", self.config.output_path);
        info!("Statistics:");
        info!("  Frames written: {}", self.frames_written);
        info!("  Frames dropped: {}", self.frames_dropped);
        info!("  Frames duplicated: {}", self.frames_duplicated);

        // Mark inputs as finished
        if let Some(ref video_input) = self.video_input {
            unsafe {
                video_input.markAsFinished();
            }
        }

        if let Some(ref audio_input) = self.audio_input {
            unsafe {
                audio_input.markAsFinished();
            }
        }

        // Finish writing
        if let Some(ref writer) = self.writer {
            #[allow(deprecated)]
            unsafe {
                writer.finishWriting();
            }
            info!("AVAssetWriter finishWriting() called");
        }

        info!("MP4 file finalized successfully");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mp4_writer_config_default() {
        let config = AppleMp4WriterConfig::default();
        assert_eq!(config.output_path, PathBuf::from("/tmp/output.mp4"));
        assert_eq!(config.sync_tolerance_ms, None);
        assert_eq!(config.video_codec, Some("avc1".to_string()));
        assert_eq!(config.video_bitrate, Some(5_000_000));
    }

    #[test]
    fn test_mp4_writer_config_custom() {
        let config = AppleMp4WriterConfig {
            output_path: PathBuf::from("/tmp/test.mp4"),
            sync_tolerance_ms: Some(33.3),
            video_bitrate: Some(10_000_000),
            ..Default::default()
        };

        assert_eq!(config.output_path, PathBuf::from("/tmp/test.mp4"));
        assert_eq!(config.sync_tolerance_ms, Some(33.3));
        assert_eq!(config.video_bitrate, Some(10_000_000));
    }
}
