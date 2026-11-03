// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video codec node bodies `tatolab-h264` and `tatolab-h265` run in their
//! helpers: a thread reads its node's ports through the module host table and
//! codes, with no Python attached, on a Vulkan Video device the helper owns on
//! Linux and on Apple's media engine through VideoToolbox on macOS.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use streamlib_video_bitstream::VideoCodecElementaryStream;
use tatolab_module_sdk::module_host_tracing::forward_this_modules_tracing_records_to;
use tatolab_module_sdk::{ModuleHost, ModuleHostInputPort, ModuleHostOutputPort};

pub mod encoded_video_group_entry_gate;
pub mod encoded_video_publication;
pub mod hang_legacy_container_media_frame;
#[cfg(target_os = "linux")]
pub mod owned_device_video_decoder;
#[cfg(target_os = "linux")]
pub mod owned_device_video_encoder;
pub mod video_decoder_settings;
pub mod video_encoder_settings;
pub mod video_frame_message;
#[cfg(target_os = "macos")]
pub mod videotoolbox;

use crate::encoded_video_publication::EncodedVideoOutputPort;
use crate::video_decoder_settings::VideoDecoderSettings;
use crate::video_encoder_settings::VideoEncoderSettings;
use crate::video_frame_message::VideoFrameMessage;

/// The port decoded video frames travel on: an encoder's input, a decoder's
/// output.
pub const VIDEO_PORT: &str = "video";
/// The port media frames travel on: an encoder's output, a decoder's input.
pub const ENCODED_VIDEO_PORT: &str = "encoded_video";

/// How long one wait for a message lasts before the stop flag is read again.
const INPUT_PORT_WAIT: Duration = Duration::from_millis(100);

/// Which node a codec body runs as: the type name its logs and refusals
/// carry, and the elementary stream it codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoCodecNodeIdentity {
    pub node_type_name: &'static str,
    pub elementary_stream: VideoCodecElementaryStream,
}

impl VideoCodecNodeIdentity {
    /// `tatolab.h264:H264Encoder`.
    pub const H264_ENCODER: Self = Self {
        node_type_name: "H264Encoder",
        elementary_stream: VideoCodecElementaryStream::H264,
    };
    /// `tatolab.h265:H265Encoder`.
    pub const H265_ENCODER: Self = Self {
        node_type_name: "H265Encoder",
        elementary_stream: VideoCodecElementaryStream::H265,
    };
    /// `tatolab.h264:H264Decoder`.
    pub const H264_DECODER: Self = Self {
        node_type_name: "H264Decoder",
        elementary_stream: VideoCodecElementaryStream::H264,
    };
    /// `tatolab.h265:H265Decoder`.
    pub const H265_DECODER: Self = Self {
        node_type_name: "H265Decoder",
        elementary_stream: VideoCodecElementaryStream::H265,
    };

    /// The Vulkan Video codec this node's elementary stream is coded with.
    #[cfg(target_os = "linux")]
    pub fn vulkan_video_codec(self) -> streamlib_vulkan_video::Codec {
        match self.elementary_stream {
            VideoCodecElementaryStream::H264 => streamlib_vulkan_video::Codec::H264,
            VideoCodecElementaryStream::H265 => streamlib_vulkan_video::Codec::H265,
        }
    }
}

/// A codec node's thread, started over the module host table its node lent
/// its ports through, and stopped by flag.
pub struct VideoCodecNodeThread {
    stop_requested: Arc<AtomicBool>,
    running: Mutex<Option<VideoCodecNodeThreadEnding>>,
}

struct VideoCodecNodeThreadEnding {
    join_handle: std::thread::JoinHandle<()>,
    ended: mpsc::Receiver<()>,
}

impl VideoCodecNodeThread {
    /// Start an encoder thread coding the frames `video` receives onto
    /// `encoded_video`.
    pub fn start_encoder(
        node: VideoCodecNodeIdentity,
        module_host: ModuleHost,
        settings: VideoEncoderSettings,
    ) -> Result<Self, String> {
        let video_input = module_host
            .open_input_port(VIDEO_PORT)
            .map_err(|refusal| refusal.to_string())?;
        let encoded_video_output = module_host
            .open_output_port(ENCODED_VIDEO_PORT)
            .map_err(|refusal| refusal.to_string())?;
        forward_this_modules_tracing_records_to(&module_host);
        Self::spawn(node, move |stop_requested| {
            encode_until_stopped(
                node,
                &module_host,
                &video_input,
                &encoded_video_output,
                settings,
                stop_requested,
            );
        })
    }

    /// Start a decoder thread decoding the media frames `encoded_video`
    /// receives onto `video`.
    pub fn start_decoder(
        node: VideoCodecNodeIdentity,
        module_host: ModuleHost,
        settings: VideoDecoderSettings,
    ) -> Result<Self, String> {
        let encoded_video_input = module_host
            .open_input_port(ENCODED_VIDEO_PORT)
            .map_err(|refusal| refusal.to_string())?;
        let video_output = module_host
            .open_output_port(VIDEO_PORT)
            .map_err(|refusal| refusal.to_string())?;
        forward_this_modules_tracing_records_to(&module_host);
        Self::spawn(node, move |stop_requested| {
            decode_until_stopped(
                node,
                &module_host,
                &encoded_video_input,
                &video_output,
                settings,
                stop_requested,
            );
        })
    }

    fn spawn(
        node: VideoCodecNodeIdentity,
        body: impl FnOnce(&AtomicBool) + Send + 'static,
    ) -> Result<Self, String> {
        let stop_requested = Arc::new(AtomicBool::new(false));
        let stop_requested_of_the_thread = Arc::clone(&stop_requested);
        let (thread_ended, ended) = mpsc::channel();
        let join_handle = std::thread::Builder::new()
            .name(node.node_type_name.to_ascii_lowercase())
            .spawn(move || {
                body(&stop_requested_of_the_thread);
                let _ = thread_ended.send(());
            })
            .map_err(|spawn_failure| {
                format!(
                    "{}: the coding thread did not start: {spawn_failure}",
                    node.node_type_name
                )
            })?;
        Ok(Self {
            stop_requested,
            running: Mutex::new(Some(VideoCodecNodeThreadEnding { join_handle, ended })),
        })
    }

    /// Ask the thread to stop and wait up to `timeout` for it: `true` when it
    /// ended.
    pub fn stop_and_join(&self, timeout: Duration) -> bool {
        self.stop_requested.store(true, Ordering::Release);
        let Ok(mut running) = self.running.lock() else {
            return false;
        };
        let Some(ending) = running.take() else {
            return true;
        };
        match ending.ended.recv_timeout(timeout) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = ending.join_handle.join();
                true
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                *running = Some(ending);
                false
            }
        }
    }
}

/// `encoded_video` as the publication writes it.
struct EncodedVideoLentByTheModuleHost<'port>(&'port ModuleHostOutputPort);

impl EncodedVideoOutputPort for EncodedVideoLentByTheModuleHost<'_> {
    fn set_catalog_entry(&self, section: &str, entry_json: &str) -> Result<(), String> {
        self.0
            .set_catalog_entry(section, entry_json)
            .map_err(|refusal| refusal.to_string())
    }

    fn write(
        &self,
        media_frame: &[u8],
        timestamp_ns: i64,
        starts_group: bool,
    ) -> Result<(), String> {
        self.0
            .write(media_frame, Some(timestamp_ns), starts_group)
            .map_err(|refusal| refusal.to_string())
    }
}

#[cfg(target_os = "linux")]
type VideoEncoderOnThisPlatform = owned_device_video_encoder::OwnedDeviceVideoEncoder;

#[cfg(target_os = "linux")]
fn open_the_encoder(
    node: VideoCodecNodeIdentity,
    _module_host: &ModuleHost,
    settings: VideoEncoderSettings,
) -> Result<VideoEncoderOnThisPlatform, String> {
    VideoEncoderOnThisPlatform::open(node, settings)
}

#[cfg(target_os = "macos")]
type VideoEncoderOnThisPlatform =
    videotoolbox::videotoolbox_video_encoder::VideoToolboxVideoEncoder;

#[cfg(target_os = "macos")]
fn open_the_encoder(
    node: VideoCodecNodeIdentity,
    module_host: &ModuleHost,
    settings: VideoEncoderSettings,
) -> Result<VideoEncoderOnThisPlatform, String> {
    VideoEncoderOnThisPlatform::open(node, module_host.clone(), settings)
}

/// No encoder opens here, so none exists to encode with.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
enum VideoEncoderOnThisPlatform {}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl VideoEncoderOnThisPlatform {
    fn encode_and_publish(
        &mut self,
        _frame: &VideoFrameMessage,
        _encoded_video: &dyn EncodedVideoOutputPort,
    ) -> Result<(), String> {
        match *self {}
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_the_encoder(
    node: VideoCodecNodeIdentity,
    _module_host: &ModuleHost,
    _settings: VideoEncoderSettings,
) -> Result<VideoEncoderOnThisPlatform, String> {
    Err(format!(
        "{} encodes on Linux and macOS only",
        node.node_type_name
    ))
}

/// Read, encode and write until the node stops. An encoder that cannot open
/// keeps draining `video`, so upstream still sees a live consumer.
fn encode_until_stopped(
    node: VideoCodecNodeIdentity,
    module_host: &ModuleHost,
    video_input: &ModuleHostInputPort,
    encoded_video_output: &ModuleHostOutputPort,
    settings: VideoEncoderSettings,
    stop_requested: &AtomicBool,
) {
    let node_type_name = node.node_type_name;
    let mut encoder = match open_the_encoder(node, module_host, settings) {
        Ok(encoder) => Some(encoder),
        Err(refusal) => {
            tracing::error!("{refusal}; every frame is discarded");
            None
        }
    };
    let encoded_video = EncodedVideoLentByTheModuleHost(encoded_video_output);
    let mut frames_refused: u64 = 0;
    while !stop_requested.load(Ordering::Acquire) && !module_host.stop_requested() {
        if let Err(refusal) = module_host.wait_for_any_input_port(INPUT_PORT_WAIT) {
            tracing::error!("{node_type_name}: waiting for a frame was refused: {refusal}");
            return;
        }
        loop {
            let message = match video_input.read() {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(refusal) => {
                    tracing::error!("{node_type_name}: reading `video` was refused: {refusal}");
                    return;
                }
            };
            let Some(encoder) = encoder.as_mut() else {
                continue;
            };
            let encoded = VideoFrameMessage::from_msgpack(message.payload())
                .and_then(|frame| encoder.encode_and_publish(&frame, &encoded_video));
            if let Err(refusal) = encoded {
                frames_refused += 1;
                if frames_refused.is_power_of_two() {
                    tracing::error!(frames_refused, "{refusal}");
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
type VideoDecoderOnThisPlatform = owned_device_video_decoder::OwnedDeviceVideoDecoder;

#[cfg(target_os = "macos")]
type VideoDecoderOnThisPlatform =
    videotoolbox::videotoolbox_video_decoder::VideoToolboxVideoDecoder;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_the_decoder(
    node: VideoCodecNodeIdentity,
    module_host: &ModuleHost,
    settings: VideoDecoderSettings,
) -> Result<VideoDecoderOnThisPlatform, String> {
    VideoDecoderOnThisPlatform::open(node, module_host.clone(), settings)
}

/// No decoder opens here, so none exists to decode with.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
enum VideoDecoderOnThisPlatform {}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl VideoDecoderOnThisPlatform {
    fn decode_and_publish(
        &mut self,
        _media_frame: &tatolab_module_sdk::ModuleHostReadMessage,
        _video_output: &ModuleHostOutputPort,
    ) -> Result<(), String> {
        match *self {}
    }

    fn report_at_teardown(&self) {
        match *self {}
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_the_decoder(
    node: VideoCodecNodeIdentity,
    _module_host: &ModuleHost,
    _settings: VideoDecoderSettings,
) -> Result<VideoDecoderOnThisPlatform, String> {
    Err(format!(
        "{} decodes on Linux and macOS only",
        node.node_type_name
    ))
}

/// Read, decode and publish until the node stops. A decoder that cannot open
/// keeps draining `encoded_video`, so upstream still sees a live consumer.
fn decode_until_stopped(
    node: VideoCodecNodeIdentity,
    module_host: &ModuleHost,
    encoded_video_input: &ModuleHostInputPort,
    video_output: &ModuleHostOutputPort,
    settings: VideoDecoderSettings,
    stop_requested: &AtomicBool,
) {
    let node_type_name = node.node_type_name;
    let mut decoder = match open_the_decoder(node, module_host, settings) {
        Ok(decoder) => Some(decoder),
        Err(refusal) => {
            tracing::error!("{refusal}; every media frame is discarded");
            None
        }
    };
    let mut media_frames_refused: u64 = 0;
    while !stop_requested.load(Ordering::Acquire) && !module_host.stop_requested() {
        if let Err(refusal) = module_host.wait_for_any_input_port(INPUT_PORT_WAIT) {
            tracing::error!("{node_type_name}: waiting for a media frame was refused: {refusal}");
            break;
        }
        loop {
            let media_frame = match encoded_video_input.read() {
                Ok(Some(media_frame)) => media_frame,
                Ok(None) => break,
                Err(refusal) => {
                    tracing::error!(
                        "{node_type_name}: reading `encoded_video` was refused: {refusal}"
                    );
                    return;
                }
            };
            let Some(decoder) = decoder.as_mut() else {
                continue;
            };
            if let Err(refusal) = decoder.decode_and_publish(&media_frame, video_output) {
                media_frames_refused += 1;
                if media_frames_refused.is_power_of_two() {
                    tracing::error!(
                        media_frames_refused,
                        "{refusal}; discarding to the next group start"
                    );
                }
            }
        }
    }
    if let Some(decoder) = decoder.as_ref() {
        decoder.report_at_teardown();
    }
}
