// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The video codec node bodies `tatolab-h264` and `tatolab-h265` run in their
//! helpers: a thread reads its node's ports through the module host table and
//! codes, with no Python attached, with the sessions its node's builds open on
//! it: with Vulkan Video on Linux, each session in the machine's video codec
//! host, and on Apple's media engine through VideoToolbox on macOS.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use streamlib_video_bitstream::VideoCodecElementaryStream;
use tatolab_module_sdk::module_host_tracing::forward_this_modules_tracing_records_to;
use tatolab_module_sdk::{
    ModuleHost, ModuleHostInputPort, ModuleHostOutputPort, ModuleHostTablePayloadDescriptor,
    codec_of_a_packets_unit, packets_unit_payload_descriptor,
};

pub mod encoded_video_group_entry_gate;
pub mod encoded_video_publication;
pub mod image_frame_on_the_video_port;
#[cfg(target_os = "linux")]
pub mod video_codec_host;
#[cfg(target_os = "linux")]
pub mod video_codec_host_protocol;
#[cfg(all(test, target_os = "linux"))]
mod video_codec_host_protocol_tests;
#[cfg(target_os = "linux")]
pub mod video_codec_host_session_of_the_node;
#[cfg(all(test, target_os = "linux"))]
mod video_codec_host_session_of_the_node_tests;
#[cfg(all(test, target_os = "linux"))]
mod video_codec_host_tests;
pub mod video_codec_retry_gate;
#[cfg(test)]
mod video_codec_retry_gate_tests;
pub mod video_coding_sessions;
#[cfg(test)]
mod video_coding_sessions_tests;
#[cfg(target_os = "linux")]
pub mod video_decode_on_a_device;
#[cfg(all(test, target_os = "linux"))]
mod video_decode_on_a_device_tests;
pub mod video_decoder_opening;
#[cfg(test)]
mod video_decoder_opening_tests;
pub mod video_decoder_settings;
#[cfg(target_os = "linux")]
pub mod video_encode_on_a_device;
#[cfg(all(test, target_os = "linux"))]
mod video_encode_on_a_device_tests;
pub mod video_encoder_settings;
#[cfg(target_os = "macos")]
pub mod videotoolbox;
#[cfg(target_os = "linux")]
pub mod vulkan_video_decoder;
#[cfg(target_os = "linux")]
pub mod vulkan_video_encoder;

use crate::encoded_video_publication::EncodedVideoOutputPort;
use crate::image_frame_on_the_video_port::ImageFrameReadByTheEncoder;
use crate::video_codec_retry_gate::VideoCodecRefusal;
use crate::video_coding_sessions::{
    VideoCodingSessionCommand, VideoCodingSessionNotOpened, VideoCodingSessionsOfTheThread,
};
use crate::video_decoder_opening::VideoDecoderOpening;
use crate::video_decoder_settings::VideoDecoderSettings;
use crate::video_encoder_settings::VideoEncoderSettings;

/// The Image port decoded pictures travel on: an encoder's input, a
/// decoder's output.
pub const VIDEO_PORT: &str = "video";
/// The Packets port access units travel on: an encoder's output, a decoder's
/// input.
pub const ENCODED_VIDEO_PORT: &str = "encoded_video";

/// How long one wait for a message lasts before the stop flag is read again.
const INPUT_PORT_WAIT: Duration = Duration::from_millis(100);

/// Why an encoder did not encode a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum VideoFrameNotEncoded {
    /// This frame only; the next may encode.
    ThisFrame(String),
    /// The frame is of a format the encoder never reads, so the node fails,
    /// naming it; a producer that states no format may send it on every frame.
    EveryFrameOfItsProducer(String),
}

/// A refusal written as a plain message is for this frame only; a refusal that fails the node
/// names [`VideoFrameNotEncoded::EveryFrameOfItsProducer`] where it is made.
impl From<String> for VideoFrameNotEncoded {
    fn from(refusal: String) -> Self {
        Self::ThisFrame(refusal)
    }
}

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

    /// The codec a Packets unit of this node's elementary stream names.
    pub fn packets_unit_codec_name(self) -> &'static str {
        match self.elementary_stream {
            VideoCodecElementaryStream::H264 => "h264",
            VideoCodecElementaryStream::H265 => "h265",
        }
    }

    /// The Vulkan Video codec this node's elementary stream is coded with.
    #[cfg(target_os = "linux")]
    pub fn vulkan_video_codec(self) -> streamlib_vulkan_video::Codec {
        match self.elementary_stream {
            VideoCodecElementaryStream::H264 => streamlib_vulkan_video::Codec::H264,
            VideoCodecElementaryStream::H265 => streamlib_vulkan_video::Codec::H265,
        }
    }
}

/// How long a build waits for the coding thread to open the session it asked for: the video
/// codec host answers an opening within its longest answer, and the thread takes a command
/// between frames.
const SESSION_OPENING_ANSWER_WAIT: Duration = Duration::from_secs(15);

/// How long dropping the thread waits for it to end: its own wait for a message is bounded at a
/// tenth of a second, and a coding in flight finishes first.
const CODING_THREAD_END_WAIT: Duration = Duration::from_secs(2);

/// A codec node's thread, started over the module host table its node lent its ports through,
/// and coding with the sessions opened on it. It takes no settings: each session carries its
/// own, so a new setting opens a session beside the old one and the stream never stops.
/// Dropping it stops the thread.
pub struct VideoCodecNodeThread<Settings> {
    node: VideoCodecNodeIdentity,
    commands: mpsc::Sender<VideoCodingSessionCommand<Settings>>,
    sessions_opened: AtomicU64,
    stop_requested: Arc<AtomicBool>,
    /// In a mutex so the thread may be shared: the receiver alone is not `Sync`.
    running: Mutex<Option<VideoCodecNodeThreadEnding>>,
}

struct VideoCodecNodeThreadEnding {
    join_handle: std::thread::JoinHandle<()>,
    ended: mpsc::Receiver<()>,
}

/// A session open on a codec node's thread, closed when dropped.
#[must_use = "dropping the session closes it"]
pub struct VideoCodecNodeSession<Settings> {
    number: u64,
    commands: mpsc::Sender<VideoCodingSessionCommand<Settings>>,
}

impl<Settings> VideoCodecNodeSession<Settings> {
    /// Close the session: the thread drops it before its next frame. A second close does
    /// nothing.
    pub fn close(&self) {
        let _ = self.commands.send(VideoCodingSessionCommand::Close {
            number: self.number,
        });
    }
}

impl<Settings> Drop for VideoCodecNodeSession<Settings> {
    fn drop(&mut self) {
        self.close();
    }
}

impl VideoCodecNodeThread<VideoEncoderSettings> {
    /// Start an encoder thread coding the frames `video` receives onto `encoded_video` with the
    /// newest session opened on it.
    pub fn start_encoder(
        node: VideoCodecNodeIdentity,
        module_host: ModuleHost,
    ) -> Result<Self, String> {
        let video_input = module_host
            .open_input_port(VIDEO_PORT)
            .map_err(|refusal| refusal.to_string())?;
        let encoded_video_output = module_host
            .open_output_port(ENCODED_VIDEO_PORT)
            .map_err(|refusal| refusal.to_string())?;
        forward_this_modules_tracing_records_to(&module_host);
        Self::spawn(node, move |commands, stop_requested| {
            encode_until_stopped(
                node,
                &module_host,
                &video_input,
                &encoded_video_output,
                &commands,
                stop_requested,
            );
        })
    }
}

impl VideoCodecNodeThread<VideoDecoderSettings> {
    /// Start a decoder thread decoding the access units `encoded_video` receives onto `video`,
    /// moving to the newest session opened on it at a group start.
    pub fn start_decoder(
        node: VideoCodecNodeIdentity,
        module_host: ModuleHost,
    ) -> Result<Self, String> {
        let encoded_video_input = module_host
            .open_input_port(ENCODED_VIDEO_PORT)
            .map_err(|refusal| refusal.to_string())?;
        let video_output = module_host
            .open_output_port(VIDEO_PORT)
            .map_err(|refusal| refusal.to_string())?;
        forward_this_modules_tracing_records_to(&module_host);
        Self::spawn(node, move |commands, stop_requested| {
            decode_until_stopped(
                node,
                &module_host,
                &encoded_video_input,
                &video_output,
                &commands,
                stop_requested,
            );
        })
    }
}

impl<Settings: Send + 'static> VideoCodecNodeThread<Settings> {
    fn spawn(
        node: VideoCodecNodeIdentity,
        body: impl FnOnce(mpsc::Receiver<VideoCodingSessionCommand<Settings>>, &AtomicBool)
        + Send
        + 'static,
    ) -> Result<Self, String> {
        let (commands, commands_received) = mpsc::channel();
        let stop_requested = Arc::new(AtomicBool::new(false));
        let stop_requested_of_the_thread = Arc::clone(&stop_requested);
        let (thread_ended, ended) = mpsc::channel();
        let join_handle = std::thread::Builder::new()
            .name(node.node_type_name.to_ascii_lowercase())
            .spawn(move || {
                body(commands_received, &stop_requested_of_the_thread);
                let _ = thread_ended.send(());
            })
            .map_err(|spawn_failure| {
                format!(
                    "{}: the coding thread did not start: {spawn_failure}",
                    node.node_type_name
                )
            })?;
        Ok(Self {
            node,
            commands,
            sessions_opened: AtomicU64::new(0),
            stop_requested,
            running: Mutex::new(Some(VideoCodecNodeThreadEnding { join_handle, ended })),
        })
    }

    /// Open a session of `settings` on the thread, beside the ones open there, and wait for it
    /// to open: the thread codes with it from then on. A refusal leaves the open sessions
    /// coding.
    pub fn open_session(
        &self,
        settings: Settings,
    ) -> Result<VideoCodecNodeSession<Settings>, VideoCodingSessionNotOpened> {
        let node_type_name = self.node.node_type_name;
        let number = self.sessions_opened.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = mpsc::sync_channel(1);
        self.commands
            .send(VideoCodingSessionCommand::Open {
                number,
                settings,
                answer,
            })
            .map_err(|_| VideoCodingSessionNotOpened::CodingThreadEnded { node_type_name })?;
        let session = VideoCodecNodeSession {
            number,
            commands: self.commands.clone(),
        };
        match answered.recv_timeout(SESSION_OPENING_ANSWER_WAIT) {
            Ok(Ok(())) => Ok(session),
            // Dropping `session` closes what the thread may still open.
            Ok(Err(refusal)) => Err(refusal),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(VideoCodingSessionNotOpened::Unanswered {
                node_type_name,
                waited: SESSION_OPENING_ANSWER_WAIT,
            }),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(VideoCodingSessionNotOpened::CodingThreadEnded { node_type_name })
            }
        }
    }
}

impl<Settings> Drop for VideoCodecNodeThread<Settings> {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::Release);
        let Some(ending) = self.running.get_mut().ok().and_then(Option::take) else {
            return;
        };
        match ending.ended.recv_timeout(CODING_THREAD_END_WAIT) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = ending.join_handle.join();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => tracing::warn!(
                "{}: the coding thread is still finishing a coding, and ends after it",
                self.node.node_type_name
            ),
        }
    }
}

/// `encoded_video` as the publication writes it: each access unit one
/// Packets unit of the node's codec.
struct EncodedVideoLentByTheModuleHost<'port> {
    encoded_video_output: &'port ModuleHostOutputPort,
    packets_unit: ModuleHostTablePayloadDescriptor,
}

impl EncodedVideoOutputPort for EncodedVideoLentByTheModuleHost<'_> {
    fn set_stream_facts(&self, family: &str, stream_facts_json: &str) -> Result<(), String> {
        self.encoded_video_output
            .set_stream_facts(family, stream_facts_json)
            .map_err(|refusal| refusal.to_string())
    }

    fn write(
        &self,
        annex_b_access_unit: &[u8],
        timestamp_ns: i64,
        starts_group: bool,
    ) -> Result<(), String> {
        self.encoded_video_output
            .write_described(
                annex_b_access_unit,
                Some(timestamp_ns),
                starts_group,
                &self.packets_unit,
            )
            .map_err(|refusal| refusal.to_string())
    }
}

#[cfg(target_os = "linux")]
type VideoEncoderOnThisPlatform = vulkan_video_encoder::VulkanVideoEncoder;

#[cfg(target_os = "linux")]
fn open_the_encoder(
    node: VideoCodecNodeIdentity,
    module_host: &ModuleHost,
    settings: VideoEncoderSettings,
) -> Result<VideoEncoderOnThisPlatform, String> {
    VideoEncoderOnThisPlatform::open(node, module_host, settings)
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
        _frame: &ImageFrameReadByTheEncoder,
        _encoded_video: &dyn EncodedVideoOutputPort,
    ) -> Result<(), VideoFrameNotEncoded> {
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

/// Read, encode and write until the node stops, with the newest session open. While none is
/// open, once the first build has asked for one, `video` is still drained, so upstream sees a
/// live consumer.
fn encode_until_stopped(
    node: VideoCodecNodeIdentity,
    module_host: &ModuleHost,
    video_input: &ModuleHostInputPort,
    encoded_video_output: &ModuleHostOutputPort,
    commands: &mpsc::Receiver<VideoCodingSessionCommand<VideoEncoderSettings>>,
    stop_requested: &AtomicBool,
) {
    let node_type_name = node.node_type_name;
    let packets_unit = match packets_unit_payload_descriptor(node.packets_unit_codec_name()) {
        Ok(packets_unit) => packets_unit,
        Err(refusal) => {
            tracing::error!("{node_type_name}: {refusal}");
            return;
        }
    };
    let encoded_video = EncodedVideoLentByTheModuleHost {
        encoded_video_output,
        packets_unit,
    };
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let take_the_commands = |sessions: &mut VideoCodingSessionsOfTheThread<_>| {
        sessions.take_the_commands(
            commands,
            INPUT_PORT_WAIT,
            |_| true,
            |settings, _| {
                open_the_encoder(node, module_host, settings)
                    .map_err(VideoCodingSessionNotOpened::Refused)
            },
        );
    };
    let mut its_producer_was_refused = false;
    let mut frames_refused: u64 = 0;
    while !stop_requested.load(Ordering::Acquire) && !module_host.stop_requested() {
        take_the_commands(&mut sessions);
        // Frames wait on `video` for the node's first build, so the stream's first frame
        // is encoded rather than read with no session to take it.
        if !sessions.a_session_was_asked_for() {
            continue;
        }
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
            take_the_commands(&mut sessions);
            if its_producer_was_refused {
                continue;
            }
            let Some(encoder) = sessions.newest_mut() else {
                continue;
            };
            let encoded = ImageFrameReadByTheEncoder::of_the_video_message(&message)
                .map_err(VideoFrameNotEncoded::ThisFrame)
                .and_then(|frame| encoder.encode_and_publish(&frame, &encoded_video));
            match encoded {
                Ok(()) => {}
                Err(VideoFrameNotEncoded::ThisFrame(refusal)) => {
                    frames_refused += 1;
                    if frames_refused.is_power_of_two() {
                        tracing::error!(frames_refused, "{refusal}");
                    }
                }
                Err(VideoFrameNotEncoded::EveryFrameOfItsProducer(refusal)) => {
                    tracing::error!("{refusal}; the node fails and every frame is discarded");
                    if let Err(failure) = module_host.fail_the_node_unrecoverably(&refusal) {
                        tracing::error!(
                            "{node_type_name}: the node could not be failed: {failure}"
                        );
                    }
                    // Draining on keeps upstream's consumer live until the node is stopped.
                    its_producer_was_refused = true;
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
type VideoDecoderOnThisPlatform = vulkan_video_decoder::VulkanVideoDecoder;

#[cfg(target_os = "macos")]
type VideoDecoderOnThisPlatform =
    videotoolbox::videotoolbox_video_decoder::VideoToolboxVideoDecoder;

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_the_decoder(
    node: VideoCodecNodeIdentity,
    module_host: &ModuleHost,
    settings: VideoDecoderSettings,
) -> Result<VideoDecoderOnThisPlatform, VideoCodecRefusal> {
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
) -> Result<VideoDecoderOnThisPlatform, VideoCodecRefusal> {
    Err(VideoCodecRefusal::ForGood(format!(
        "{} decodes on Linux and macOS only",
        node.node_type_name
    )))
}

/// One decode session on a decoder node's thread: its settings and its decoder's opening. It
/// says what it decoded as it closes.
struct VideoDecodeSession {
    settings: VideoDecoderSettings,
    opening: VideoDecoderOpening<VideoDecoderOnThisPlatform>,
}

impl VideoDecodeSession {
    /// Open a session of `settings` at `now`. Refused for now with no other session decoding,
    /// it is opened again a retry interval after each refusal, as a decoder brought up before
    /// the GPU frees what it needs; refused beside another that decodes, it is refused, and the
    /// other keeps decoding.
    fn open(
        node: VideoCodecNodeIdentity,
        module_host: &ModuleHost,
        settings: VideoDecoderSettings,
        another_decodes: bool,
        now: Instant,
    ) -> Result<Self, VideoCodingSessionNotOpened> {
        let opening = match open_the_decoder(node, module_host, settings) {
            Ok(decoder) => VideoDecoderOpening::opened(node.node_type_name, decoder),
            Err(VideoCodecRefusal::ForNow(refusal)) if !another_decodes => {
                VideoDecoderOpening::refused_for_now_at(node.node_type_name, now, &refusal)
            }
            Err(VideoCodecRefusal::ForNow(refusal)) => {
                return Err(VideoCodingSessionNotOpened::RefusedForNow(refusal));
            }
            Err(VideoCodecRefusal::ForGood(refusal)) => {
                return Err(VideoCodingSessionNotOpened::Refused(refusal));
            }
        };
        Ok(Self { settings, opening })
    }

    /// The session's decoder, opened again at `now` when it was refused for now and the retry
    /// interval has passed; `None` while it is not open.
    fn decoder_at(
        &mut self,
        node: VideoCodecNodeIdentity,
        module_host: &ModuleHost,
        now: Instant,
    ) -> Option<&mut VideoDecoderOnThisPlatform> {
        let settings = self.settings;
        self.opening
            .the_decoder_at(now, || open_the_decoder(node, module_host, settings))
    }
}

impl Drop for VideoDecodeSession {
    fn drop(&mut self) {
        if let Some(decoder) = self.opening.opened_decoder() {
            decoder.report_at_teardown();
        }
    }
}

/// Read, decode and publish until the node stops, each media frame with the session
/// [`VideoCodingSessionsOfTheThread::the_session_decoding_a_media_frame`] names. Until the
/// node's first build asks for a session, `encoded_video` is left unread; after, while no
/// session decodes, it is still drained, so upstream sees a live consumer.
fn decode_until_stopped(
    node: VideoCodecNodeIdentity,
    module_host: &ModuleHost,
    encoded_video_input: &ModuleHostInputPort,
    video_output: &ModuleHostOutputPort,
    commands: &mpsc::Receiver<VideoCodingSessionCommand<VideoDecoderSettings>>,
    stop_requested: &AtomicBool,
) {
    let node_type_name = node.node_type_name;
    let mut sessions = VideoCodingSessionsOfTheThread::default();
    let take_the_commands = |sessions: &mut VideoCodingSessionsOfTheThread<VideoDecodeSession>| {
        sessions.take_the_commands(
            commands,
            INPUT_PORT_WAIT,
            |session| session.opening.opened_decoder().is_some(),
            |settings, another_decodes| {
                VideoDecodeSession::open(
                    node,
                    module_host,
                    settings,
                    another_decodes,
                    Instant::now(),
                )
            },
        );
    };
    let mut media_frames_refused: u64 = 0;
    while !stop_requested.load(Ordering::Acquire) && !module_host.stop_requested() {
        take_the_commands(&mut sessions);
        // Media frames wait on `encoded_video` for the node's first build, so the group
        // the stream opens with reaches the first session rather than being drained.
        if !sessions.a_session_was_asked_for() {
            continue;
        }
        if let Err(refusal) = module_host.wait_for_any_input_port(INPUT_PORT_WAIT) {
            tracing::error!("{node_type_name}: waiting for a media frame was refused: {refusal}");
            return;
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
            take_the_commands(&mut sessions);
            let now = Instant::now();
            let Some(session) = sessions
                .the_session_decoding_a_media_frame(media_frame.starts_group(), |session| {
                    session.decoder_at(node, module_host, now).is_some()
                })
            else {
                continue;
            };
            let Some(decoder) = session.decoder_at(node, module_host, now) else {
                continue;
            };
            let decoded = refuse_unless_a_packets_unit_of_this_codec(node, &media_frame)
                .and_then(|()| decoder.decode_and_publish(&media_frame, video_output));
            if let Err(refusal) = decoded {
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
}

/// Refuse by name a media frame that is not a Packets unit of `node`'s codec.
fn refuse_unless_a_packets_unit_of_this_codec(
    node: VideoCodecNodeIdentity,
    media_frame: &tatolab_module_sdk::ModuleHostReadMessage,
) -> Result<(), String> {
    let node_type_name = node.node_type_name;
    let payload_descriptor = media_frame
        .payload_descriptor()
        .map_err(|refusal| format!("{node_type_name}: {refusal}"))?;
    let codec = codec_of_a_packets_unit(&payload_descriptor).map_err(|refusal| {
        format!("{node_type_name}: an `encoded_video` message is not a Packets unit: {refusal}")
    })?;
    if codec != node.packets_unit_codec_name() {
        return Err(format!(
            "{node_type_name}: an `encoded_video` Packets unit is coded {codec:?}, and this \
             decoder reads {:?}",
            node.packets_unit_codec_name()
        ));
    }
    Ok(())
}
