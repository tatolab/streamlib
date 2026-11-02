// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `WhepPlayer`'s reading thread: connect, drain each received access unit and
//! Opus packet onto the node's output ports, and reconnect for as long as the
//! node runs — all of it native, writing through the module host table with no
//! Python on the path.

use std::sync::{Condvar, Mutex};
use std::time::Duration;

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

    fn index(self) -> usize {
        match self {
            Self::EncodedVideo => 0,
            Self::EncodedAudio => 1,
        }
    }
}

/// What one port's catalog entry states, taken from the stream itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WhepPlayerCatalogEntry {
    /// The codec string and coded extent the SPS carries; no `description`,
    /// because the parameter sets ride inline in the access units.
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

impl WhepPlayerCatalogEntry {
    /// The catalog section the entry belongs to.
    pub(crate) fn section(&self) -> &'static str {
        match self {
            Self::Video { .. } => "video",
            Self::Audio { .. } => "audio",
        }
    }

    /// The entry's fields as one JSON object, in hang's field names.
    pub(crate) fn entry_json(&self) -> String {
        match self {
            Self::Video {
                codec,
                coded_width,
                coded_height,
            } => format!(
                "{{\"codec\":{},\"codedWidth\":{coded_width},\"codedHeight\":{coded_height}}}",
                json_string(codec)
            ),
            Self::Audio {
                number_of_channels,
                opus_identification_header_hex,
            } => format!(
                "{{\"codec\":\"opus\",\"sampleRate\":{OPUS_RTP_SAMPLE_RATE_HZ},\
                 \"numberOfChannels\":{number_of_channels},\"description\":{}}}",
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
    pub media_frame: Vec<u8>,
    pub timestamp_ns: i64,
    /// Only a sync point opens a group on the video port; every Opus packet
    /// opens one.
    pub starts_group: bool,
    pub catalog_entry: WhepPlayerCatalogEntry,
}

impl MessageTheWhepPlayerWrites {
    pub(crate) fn of_received_media(
        received_media: ReceivedMedia,
    ) -> Result<Self, WebRtcExtensionError> {
        Ok(match received_media {
            ReceivedMedia::Video(access_unit) => Self {
                port: WhepPlayerOutputPort::EncodedVideo,
                media_frame: access_unit.media_frame_bytes()?,
                timestamp_ns: access_unit.timestamp_ns,
                starts_group: access_unit.is_sync_point,
                catalog_entry: WhepPlayerCatalogEntry::Video {
                    codec: access_unit.sequence_parameter_set.avc1_codec_string(),
                    coded_width: access_unit.sequence_parameter_set.coded_width,
                    coded_height: access_unit.sequence_parameter_set.coded_height,
                },
            },
            ReceivedMedia::Audio(packet) => Self {
                port: WhepPlayerOutputPort::EncodedAudio,
                media_frame: packet.media_frame_bytes()?,
                timestamp_ns: packet.timestamp_ns,
                starts_group: true,
                catalog_entry: WhepPlayerCatalogEntry::Audio {
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
        media_frame: &[u8],
        timestamp_ns: i64,
        starts_group: bool,
    ) -> Result<(), String>;

    fn set_catalog_entry(
        &self,
        port: WhepPlayerOutputPort,
        catalog_entry: &WhepPlayerCatalogEntry,
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

/// Opens one session per attempt: a closed peer connection cannot be dialled
/// again.
pub(crate) trait WhepPlayerSessionDialer {
    fn dial(&self) -> Result<Box<dyn WhepPlayerConnectedSession>, WebRtcExtensionError>;
}

/// Whether the player has been asked to stop, and the wait a backoff spends
/// that a stop cuts short.
pub(crate) struct WhepPlayerStopSignal {
    stopped: Mutex<bool>,
    stop_arrived: Condvar,
    stopped_from_elsewhere: Box<dyn Fn() -> bool + Send + Sync>,
}

impl WhepPlayerStopSignal {
    /// A signal that is also set whenever `stopped_from_elsewhere` says so —
    /// the node host's stop flag.
    pub(crate) fn also_set_when(
        stopped_from_elsewhere: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            stopped: Mutex::new(false),
            stop_arrived: Condvar::new(),
            stopped_from_elsewhere,
        }
    }

    pub(crate) fn is_set(&self) -> bool {
        self.stopped.lock().map(|stopped| *stopped).unwrap_or(true)
            || (self.stopped_from_elsewhere)()
    }

    pub(crate) fn set(&self) {
        if let Ok(mut stopped) = self.stopped.lock() {
            *stopped = true;
        }
        self.stop_arrived.notify_all();
    }

    /// Wait out `delay` unless a stop arrives first.
    pub(crate) fn wait_unless_stopped(&self, delay: Duration) {
        let Ok(stopped) = self.stopped.lock() else {
            return;
        };
        let _ = self
            .stop_arrived
            .wait_timeout_while(stopped, delay, |stopped| !*stopped);
    }
}

/// What a player has said about its ports so far, so it says each thing once.
#[derive(Default)]
struct WhatThePlayerStated {
    catalog_entry_by_port: [Option<WhepPlayerCatalogEntry>; 2],
    media_frames_written_by_port: [u64; 2],
    reported_an_oversized_media_frame: bool,
}

/// Why a drain ended.
enum WhyTheDrainEnded {
    Stopped,
    /// A write or entry the engine refused: not the endpoint's fault, and not
    /// retryable.
    OutputPortRefused {
        port: WhepPlayerOutputPort,
        refusal: String,
    },
    SessionEnded(WebRtcExtensionError),
}

/// Connect, drain and reconnect until `stop_signal` is set, waiting out each
/// backoff through `wait_out_the_backoff`.
pub(crate) fn play_until_stopped(
    dialer: &dyn WhepPlayerSessionDialer,
    output_ports: &dyn WhepPlayerOutputPorts,
    stop_signal: &WhepPlayerStopSignal,
    wait_out_the_backoff: &mut dyn FnMut(Duration),
) {
    let mut stated = WhatThePlayerStated::default();
    let mut delay = FIRST_RECONNECT_DELAY;
    while !stop_signal.is_set() {
        output_ports.log(WhepPlayerLogLevel::Info, "WhepPlayer: opening the session");
        match dialer.dial() {
            Ok(mut session) => {
                output_ports.log(
                    WhepPlayerLogLevel::Info,
                    "WhepPlayer: the relay accepted the session",
                );
                delay = FIRST_RECONNECT_DELAY;
                let why_it_ended =
                    drain_until_stopped(session.as_mut(), output_ports, stop_signal, &mut stated);
                session.close();
                match why_it_ended {
                    WhyTheDrainEnded::Stopped => return,
                    WhyTheDrainEnded::OutputPortRefused { port, refusal } => {
                        output_ports.log(
                            WhepPlayerLogLevel::Error,
                            &format!(
                                "WhepPlayer: writing to `{}` failed, and the reading thread is \
                                 ending: {refusal}",
                                port.port_name()
                            ),
                        );
                        return;
                    }
                    WhyTheDrainEnded::SessionEnded(failure) => {
                        output_ports.log(
                            WhepPlayerLogLevel::Warn,
                            &format!(
                                "WhepPlayer: the session ended ({failure}); retrying in {:.1}s",
                                delay.as_secs_f64()
                            ),
                        );
                    }
                }
            }
            Err(failure) => output_ports.log(
                WhepPlayerLogLevel::Warn,
                &format!(
                    "WhepPlayer: the session ended ({failure}); retrying in {:.1}s",
                    delay.as_secs_f64()
                ),
            ),
        }
        if stop_signal.is_set() {
            return;
        }
        wait_out_the_backoff(delay);
        delay = (delay * 2).min(LONGEST_RECONNECT_DELAY);
    }
}

fn drain_until_stopped(
    session: &mut dyn WhepPlayerConnectedSession,
    output_ports: &dyn WhepPlayerOutputPorts,
    stop_signal: &WhepPlayerStopSignal,
    stated: &mut WhatThePlayerStated,
) -> WhyTheDrainEnded {
    while !stop_signal.is_set() {
        let message = match session.next_message(PLAYER_POLL_TIMEOUT) {
            Ok(Some(message)) => message,
            Ok(None) => continue,
            Err(failure) => return WhyTheDrainEnded::SessionEnded(failure),
        };
        if let Err(refusal) = write_one_message(&message, output_ports, stated) {
            return WhyTheDrainEnded::OutputPortRefused {
                port: message.port,
                refusal,
            };
        }
    }
    WhyTheDrainEnded::Stopped
}

fn write_one_message(
    message: &MessageTheWhepPlayerWrites,
    output_ports: &dyn WhepPlayerOutputPorts,
    stated: &mut WhatThePlayerStated,
) -> Result<(), String> {
    let port = message.port;
    let entry_stated = &mut stated.catalog_entry_by_port[port.index()];
    if entry_stated.as_ref() != Some(&message.catalog_entry) {
        output_ports.set_catalog_entry(port, &message.catalog_entry)?;
        output_ports.log(
            WhepPlayerLogLevel::Info,
            &format!(
                "WhepPlayer: `{}` states {} {}",
                port.port_name(),
                message.catalog_entry.section(),
                message.catalog_entry.entry_json()
            ),
        );
        *entry_stated = Some(message.catalog_entry.clone());
    }
    if message.media_frame.len() > HELPER_LINK_PAYLOAD_CEILING_BYTES
        && !stated.reported_an_oversized_media_frame
    {
        stated.reported_an_oversized_media_frame = true;
        output_ports.log(
            WhepPlayerLogLevel::Error,
            &format!(
                "WhepPlayer: a {}-byte media frame on `{}` is past the \
                 {HELPER_LINK_PAYLOAD_CEILING_BYTES}-byte link ceiling and will be dropped \
                 without reaching the decoder. Reported once.",
                message.media_frame.len(),
                port.port_name()
            ),
        );
    }
    output_ports.write(
        port,
        &message.media_frame,
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
