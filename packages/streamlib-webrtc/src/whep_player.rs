// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `WhepPlayer`'s reading thread: drain each access unit and Opus packet the
//! newest endpoint opened on it receives onto the node's output ports, and
//! reconnect for as long as the node runs — all of it native, writing through
//! the module host table with no Python on the path.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::error::WebRtcExtensionError;
use crate::opus_identification_header::hex_encoded_opus_identification_header_for_rtp_stream;
use crate::whep_session::ReceivedMedia;

/// The output port `WhepPlayer` writes H.264 access units on.
pub(crate) const ENCODED_VIDEO_OUTPUT_PORT: &str = "encoded_video";

/// The output port `WhepPlayer` writes Opus packets on.
pub(crate) const ENCODED_AUDIO_OUTPUT_PORT: &str = "encoded_audio";

/// How long one wait for media lasts before the thread looks at whether it has
/// been asked to stop; short enough that a stop lands well inside the helper's
/// five-second teardown budget.
pub(crate) const PLAYER_POLL_TIMEOUT: Duration = Duration::from_millis(200);

/// The reconnect backoff. A WHEP endpoint answers `409 Conflict` while the
/// live input it fronts has not started publishing, which is the ordinary
/// state of a player brought up beside its publisher, so one failed connect is
/// never the end of the stream.
pub(crate) const FIRST_RECONNECT_DELAY: Duration = Duration::from_millis(500);
pub(crate) const LONGEST_RECONNECT_DELAY: Duration = Duration::from_secs(10);

/// How often the player says it is still working, per port.
pub(crate) const MEDIA_FRAMES_BETWEEN_PROGRESS_REPORTS: u64 = 300;

/// A helper-placed link's per-message ceiling (`streamlib-ipc-types`'
/// untrusted-session ceiling). A message past it is dropped by the engine
/// rather than refused, which would look from here like a stream that stopped,
/// so the player says so itself, once.
pub(crate) const HELPER_LINK_PAYLOAD_CEILING_BYTES: usize = 16 * 1024 * 1024;

/// The only rate an Opus RTP stream is clocked at (RFC 7587 §4.1).
pub(crate) const OPUS_RTP_SAMPLE_RATE_HZ: u32 = 48_000;

/// One of the player's two output ports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum WhepPlayerOutputPort {
    EncodedVideo,
    EncodedAudio,
}

impl WhepPlayerOutputPort {
    /// The port's declared name.
    pub(crate) fn port_name(self) -> &'static str {
        match self {
            Self::EncodedVideo => ENCODED_VIDEO_OUTPUT_PORT,
            Self::EncodedAudio => ENCODED_AUDIO_OUTPUT_PORT,
        }
    }

    /// The codec every Packets unit the port writes names.
    pub(crate) fn packets_unit_codec_name(self) -> &'static str {
        match self {
            Self::EncodedVideo => "h264",
            Self::EncodedAudio => "opus",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::EncodedVideo => 0,
            Self::EncodedAudio => 1,
        }
    }
}

/// The Packets stream facts one port states, taken from the stream itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WhepPlayerPacketsStreamFacts {
    /// The codec string and coded extent the SPS carries; no codec
    /// configuration, because the parameter sets ride inline in the access
    /// units.
    Video {
        codec: String,
        coded_width: u32,
        coded_height: u32,
    },
    /// The channel count and an `OpusHead` with a pre-skip of 0, because RTP
    /// states no encoder lookahead.
    Audio {
        number_of_channels: u8,
        opus_identification_header_hex: String,
    },
}

impl WhepPlayerPacketsStreamFacts {
    /// The facts' fields as one JSON object, in `PacketsStreamFacts`' names.
    pub(crate) fn stream_facts_json(&self) -> String {
        match self {
            Self::Video {
                codec,
                coded_width,
                coded_height,
            } => format!(
                "{{\"rfc6381_codec\":{},\"coded_width\":{coded_width},\"coded_height\":{coded_height}}}",
                json_string(codec)
            ),
            Self::Audio {
                number_of_channels,
                opus_identification_header_hex,
            } => format!(
                "{{\"rfc6381_codec\":\"opus\",\"sample_rate\":{OPUS_RTP_SAMPLE_RATE_HZ},\
                 \"channel_count\":{number_of_channels},\"codec_configuration_hex\":{}}}",
                json_string(opus_identification_header_hex)
            ),
        }
    }
}

fn json_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            control if (control as u32) < 0x20 => {
                quoted.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// One received item as the player publishes it, with the entry its port
/// states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageTheWhepPlayerWrites {
    pub port: WhepPlayerOutputPort,
    pub packets_unit_bytes: Vec<u8>,
    pub timestamp_ns: i64,
    /// Only a sync point opens a group on the video port; every Opus packet
    /// opens one.
    pub starts_group: bool,
    pub stream_facts: WhepPlayerPacketsStreamFacts,
}

impl MessageTheWhepPlayerWrites {
    pub(crate) fn of_received_media(
        received_media: ReceivedMedia,
    ) -> Result<Self, WebRtcExtensionError> {
        Ok(match received_media {
            ReceivedMedia::Video(access_unit) => Self {
                port: WhepPlayerOutputPort::EncodedVideo,
                packets_unit_bytes: access_unit.annex_b_access_unit,
                timestamp_ns: access_unit.timestamp_ns,
                starts_group: access_unit.is_sync_point,
                stream_facts: WhepPlayerPacketsStreamFacts::Video {
                    codec: access_unit.sequence_parameter_set.avc1_codec_string(),
                    coded_width: access_unit.sequence_parameter_set.coded_width,
                    coded_height: access_unit.sequence_parameter_set.coded_height,
                },
            },
            ReceivedMedia::Audio(packet) => Self {
                port: WhepPlayerOutputPort::EncodedAudio,
                packets_unit_bytes: packet.opus_packet.to_vec(),
                timestamp_ns: packet.timestamp_ns,
                starts_group: true,
                stream_facts: WhepPlayerPacketsStreamFacts::Audio {
                    number_of_channels: packet.channel_layout.channel_count(),
                    opus_identification_header_hex:
                        hex_encoded_opus_identification_header_for_rtp_stream(packet.channel_layout),
                },
            },
        })
    }
}

/// A record's severity on the node's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WhepPlayerLogLevel {
    Error,
    Warn,
    Info,
}

/// Where the player writes: its node's two output ports and its log.
pub(crate) trait WhepPlayerOutputPorts {
    fn write(
        &self,
        port: WhepPlayerOutputPort,
        packets_unit_bytes: &[u8],
        timestamp_ns: i64,
        starts_group: bool,
    ) -> Result<(), String>;

    fn set_stream_facts(
        &self,
        port: WhepPlayerOutputPort,
        stream_facts: &WhepPlayerPacketsStreamFacts,
    ) -> Result<(), String>;

    fn log(&self, level: WhepPlayerLogLevel, message: &str);
}

/// One connected WHEP session, drained until it ends.
pub(crate) trait WhepPlayerConnectedSession {
    /// The next item, or `None` when none arrived within `timeout`.
    fn next_message(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<MessageTheWhepPlayerWrites>, WebRtcExtensionError>;

    /// Close the peer connection and DELETE the session.
    fn close(&mut self);
}

impl<Session: WhepPlayerConnectedSession + ?Sized> WhepPlayerConnectedSession for Box<Session> {
    fn next_message(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<MessageTheWhepPlayerWrites>, WebRtcExtensionError> {
        (**self).next_message(timeout)
    }

    fn close(&mut self) {
        (**self).close();
    }
}

/// One endpoint, dialled once per attempt: a closed peer connection cannot be
/// dialled again.
pub(crate) trait WhepPlayerSessionDialer {
    type Session: WhepPlayerConnectedSession;

    fn dial(&self) -> Result<Self::Session, WebRtcExtensionError>;
}

impl<Dialer: WhepPlayerSessionDialer + ?Sized> WhepPlayerSessionDialer for &Dialer {
    type Session = Dialer::Session;

    fn dial(&self) -> Result<Self::Session, WebRtcExtensionError> {
        (**self).dial()
    }
}

/// What the reading thread is told by the endpoints opened and closed on it.
pub(crate) enum WhepPlayerCommand<Dialer: WhepPlayerSessionDialer> {
    /// Play this endpoint from now on, with the session its opening already
    /// connected, or dialling it on the thread when its opening could not.
    Play {
        number: u64,
        dialer: Dialer,
        connected: Option<Dialer::Session>,
    },
    /// The endpoint's value was let go: drop it, and close its session.
    Close { number: u64 },
}

/// Dial `dialer` where its endpoint is opened, so a refusal is the opening's
/// to raise and the session already playing keeps playing. With no other
/// endpoint open a refusal is handed to the thread instead, which redials it
/// with backoff: a WHEP endpoint refuses until its publisher starts, and a
/// player brought up first still plays once it does.
pub(crate) fn open_an_endpoint<Dialer: WhepPlayerSessionDialer>(
    number: u64,
    dialer: Dialer,
    another_is_open: bool,
) -> Result<WhepPlayerCommand<Dialer>, WebRtcExtensionError> {
    match dialer.dial() {
        Ok(session) => Ok(WhepPlayerCommand::Play {
            number,
            dialer,
            connected: Some(session),
        }),
        Err(refusal) if another_is_open => Err(refusal),
        Err(_) => Ok(WhepPlayerCommand::Play {
            number,
            dialer,
            connected: None,
        }),
    }
}

/// Whether the player has been asked to stop.
pub(crate) struct WhepPlayerStopSignal {
    stopped: AtomicBool,
    stopped_from_elsewhere: Box<dyn Fn() -> bool + Send + Sync>,
}

impl WhepPlayerStopSignal {
    /// A signal that is also set whenever `stopped_from_elsewhere` says so —
    /// the node host's stop flag.
    pub(crate) fn also_set_when(
        stopped_from_elsewhere: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            stopped: AtomicBool::new(false),
            stopped_from_elsewhere,
        }
    }

    pub(crate) fn is_set(&self) -> bool {
        self.stopped.load(Ordering::Acquire) || (self.stopped_from_elsewhere)()
    }

    pub(crate) fn set(&self) {
        self.stopped.store(true, Ordering::Release);
    }
}

/// Wait out `delay` for the next command, unless a stop arrives first: the
/// backoff the reading thread spends, which a newly opened endpoint cuts short.
pub(crate) fn wait_for_a_command_unless_stopped<Dialer: WhepPlayerSessionDialer>(
    commands: &Receiver<WhepPlayerCommand<Dialer>>,
    stop_signal: &WhepPlayerStopSignal,
    delay: Duration,
) -> Option<WhepPlayerCommand<Dialer>> {
    let deadline = Instant::now() + delay;
    while !stop_signal.is_set() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        match commands.recv_timeout(left.min(PLAYER_POLL_TIMEOUT)) {
            Ok(command) => return Some(command),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

/// What a player has said about its ports so far, so it says each thing once.
#[derive(Default)]
struct WhatThePlayerStated {
    stream_facts_by_port: [Option<WhepPlayerPacketsStreamFacts>; 2],
    media_frames_written_by_port: [u64; 2],
    reported_an_oversized_packets_unit: bool,
}

/// One endpoint open on the thread, and its session while it has one.
struct EndpointOfTheThread<Dialer: WhepPlayerSessionDialer> {
    number: u64,
    dialer: Dialer,
    session: Option<Dialer::Session>,
    delay: Duration,
    backoff_owed: bool,
}

impl<Dialer: WhepPlayerSessionDialer> EndpointOfTheThread<Dialer> {
    fn close_its_session(&mut self) {
        if let Some(mut session) = self.session.take() {
            session.close();
        }
    }

    fn back_off_after(
        &mut self,
        failure: &WebRtcExtensionError,
        output_ports: &dyn WhepPlayerOutputPorts,
    ) {
        output_ports.log(
            WhepPlayerLogLevel::Warn,
            &format!(
                "WhepPlayer: the session ended ({failure}); retrying in {:.1}s",
                self.delay.as_secs_f64()
            ),
        );
        self.backoff_owed = true;
    }
}

/// The endpoints open on the thread, oldest first; the newest is the one played.
struct EndpointsOfTheThread<Dialer: WhepPlayerSessionDialer> {
    endpoints: Vec<EndpointOfTheThread<Dialer>>,
}

impl<Dialer: WhepPlayerSessionDialer> EndpointsOfTheThread<Dialer> {
    fn take(
        &mut self,
        command: WhepPlayerCommand<Dialer>,
        output_ports: &dyn WhepPlayerOutputPorts,
    ) {
        match command {
            WhepPlayerCommand::Play {
                number,
                dialer,
                connected,
            } => {
                // Only the newest endpoint is drained, so an older one's session
                // would only fill its queue: it is closed, and redialled if the
                // newer endpoint is let go first.
                for older in &mut self.endpoints {
                    older.close_its_session();
                }
                if connected.is_some() {
                    output_ports.log(
                        WhepPlayerLogLevel::Info,
                        "WhepPlayer: the relay accepted the session",
                    );
                }
                self.endpoints.push(EndpointOfTheThread {
                    number,
                    dialer,
                    session: connected,
                    delay: FIRST_RECONNECT_DELAY,
                    backoff_owed: false,
                });
            }
            WhepPlayerCommand::Close { number } => {
                self.endpoints.retain_mut(|endpoint| {
                    let closing = endpoint.number == number;
                    if closing {
                        endpoint.close_its_session();
                    }
                    !closing
                });
            }
        }
    }

    fn take_the_commands(
        &mut self,
        commands: &Receiver<WhepPlayerCommand<Dialer>>,
        output_ports: &dyn WhepPlayerOutputPorts,
    ) {
        while let Ok(command) = commands.try_recv() {
            self.take(command, output_ports);
        }
    }
}

impl<Dialer: WhepPlayerSessionDialer> Drop for EndpointsOfTheThread<Dialer> {
    fn drop(&mut self) {
        for endpoint in &mut self.endpoints {
            endpoint.close_its_session();
        }
    }
}

/// Play the newest endpoint opened through `commands`, redialling it with
/// backoff whenever its session ends, until `stop_signal` is set; each backoff
/// is waited out through `wait_for_a_command`, which a newer endpoint cuts
/// short.
pub(crate) fn play_until_stopped<Dialer: WhepPlayerSessionDialer>(
    commands: &Receiver<WhepPlayerCommand<Dialer>>,
    output_ports: &dyn WhepPlayerOutputPorts,
    stop_signal: &WhepPlayerStopSignal,
    wait_for_a_command: &mut dyn FnMut(Duration) -> Option<WhepPlayerCommand<Dialer>>,
) {
    let mut stated = WhatThePlayerStated::default();
    let mut endpoints = EndpointsOfTheThread {
        endpoints: Vec::new(),
    };
    while !stop_signal.is_set() {
        endpoints.take_the_commands(commands, output_ports);
        let Some(newest) = endpoints.endpoints.last_mut() else {
            if let Some(command) = wait_for_a_command(PLAYER_POLL_TIMEOUT) {
                endpoints.take(command, output_ports);
            }
            continue;
        };
        if newest.backoff_owed {
            newest.backoff_owed = false;
            let delay = newest.delay;
            newest.delay = (delay * 2).min(LONGEST_RECONNECT_DELAY);
            if let Some(command) = wait_for_a_command(delay) {
                endpoints.take(command, output_ports);
            }
            continue;
        }
        let Some(session) = newest.session.as_mut() else {
            output_ports.log(WhepPlayerLogLevel::Info, "WhepPlayer: opening the session");
            match newest.dialer.dial() {
                Ok(session) => {
                    output_ports.log(
                        WhepPlayerLogLevel::Info,
                        "WhepPlayer: the relay accepted the session",
                    );
                    newest.session = Some(session);
                    newest.delay = FIRST_RECONNECT_DELAY;
                }
                Err(failure) => newest.back_off_after(&failure, output_ports),
            }
            continue;
        };
        match session.next_message(PLAYER_POLL_TIMEOUT) {
            Ok(None) => {}
            Ok(Some(message)) => {
                if let Err(refusal) = write_one_message(&message, output_ports, &mut stated) {
                    // A write or entry the engine refused is not the endpoint's
                    // fault, and not retryable.
                    output_ports.log(
                        WhepPlayerLogLevel::Error,
                        &format!(
                            "WhepPlayer: writing to `{}` failed, and the reading thread is \
                             ending: {refusal}",
                            message.port.port_name()
                        ),
                    );
                    return;
                }
            }
            Err(failure) => {
                newest.close_its_session();
                newest.back_off_after(&failure, output_ports);
            }
        }
    }
}

fn write_one_message(
    message: &MessageTheWhepPlayerWrites,
    output_ports: &dyn WhepPlayerOutputPorts,
    stated: &mut WhatThePlayerStated,
) -> Result<(), String> {
    let port = message.port;
    let stream_facts_stated = &mut stated.stream_facts_by_port[port.index()];
    if stream_facts_stated.as_ref() != Some(&message.stream_facts) {
        output_ports.set_stream_facts(port, &message.stream_facts)?;
        output_ports.log(
            WhepPlayerLogLevel::Info,
            &format!(
                "WhepPlayer: `{}` states stream facts {}",
                port.port_name(),
                message.stream_facts.stream_facts_json()
            ),
        );
        *stream_facts_stated = Some(message.stream_facts.clone());
    }
    if message.packets_unit_bytes.len() > HELPER_LINK_PAYLOAD_CEILING_BYTES
        && !stated.reported_an_oversized_packets_unit
    {
        stated.reported_an_oversized_packets_unit = true;
        output_ports.log(
            WhepPlayerLogLevel::Error,
            &format!(
                "WhepPlayer: a {}-byte unit on `{}` is past the \
                 {HELPER_LINK_PAYLOAD_CEILING_BYTES}-byte link ceiling and will be dropped \
                 without reaching the decoder. Reported once.",
                message.packets_unit_bytes.len(),
                port.port_name()
            ),
        );
    }
    output_ports.write(
        port,
        &message.packets_unit_bytes,
        message.timestamp_ns,
        message.starts_group,
    )?;
    let written = &mut stated.media_frames_written_by_port[port.index()];
    *written += 1;
    if *written == 1 {
        output_ports.log(
            WhepPlayerLogLevel::Info,
            &format!(
                "WhepPlayer: first media frame written on `{}`",
                port.port_name()
            ),
        );
    } else if written.is_multiple_of(MEDIA_FRAMES_BETWEEN_PROGRESS_REPORTS) {
        output_ports.log(
            WhepPlayerLogLevel::Info,
            &format!(
                "WhepPlayer: `{}` media_frames_written={written}",
                port.port_name()
            ),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
