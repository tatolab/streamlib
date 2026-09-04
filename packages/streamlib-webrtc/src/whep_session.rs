// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Playing encoded media back from a WHEP endpoint.

use crate::error::{Result, WebRtcExtensionError};
use crate::http_signalling::WhipWhepSignallingClient;
use crate::received_media_assembly::{
    OpusPacketAssembler, ReceivedOpusPacket, ReceivedVideoAccessUnit, VideoAccessUnitAssembler,
};
use crate::session_description::opus_sender_stereo_hint_in_answer;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MIME_TYPE_H264, MIME_TYPE_OPUS, MediaEngine};
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType,
};
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;

const PROTOCOL: &str = "WHEP";
const H264_PAYLOAD_TYPE: u8 = 102;
const OPUS_PAYLOAD_TYPE: u8 = 111;

/// How many assembled bags may wait for the processor thread to collect them.
/// Bounded and dropped rather than blocked: a stalled reader must not stall the
/// network loop, and a gap in `sequence_index` is how a consumer learns it lost
/// something — which is exactly what happened.
const RECEIVED_MEDIA_QUEUE_DEPTH: usize = 256;

/// The helper's teardown reply is bounded at five seconds before the parent
/// stops waiting, and the peer connection's own close has no bound of its own.
/// This leaves room for the join that precedes it.
const CLOSE_BUDGET: Duration = Duration::from_secs(3);

/// One bag's worth of media, assembled and ready to be spelled.
#[derive(Debug, Clone)]
pub enum ReceivedMedia {
    Video(ReceivedVideoAccessUnit),
    Audio(ReceivedOpusPacket),
}

/// One connected WHEP session, draining into an assembled-media queue.
pub struct WhepPlayingSession {
    signalling: WhipWhepSignallingClient,
    peer_connection: Arc<RTCPeerConnection>,
    session_url: String,
    received_media: mpsc::Receiver<ReceivedMedia>,
}

impl WhepPlayingSession {
    /// Offer two receive-only transceivers and start draining what arrives.
    pub async fn connect(endpoint_url: String, bearer_token: Option<String>) -> Result<Self> {
        let signalling = WhipWhepSignallingClient::new(endpoint_url, bearer_token, PROTOCOL)?;
        let peer_connection = Arc::new(build_peer_connection().await?);

        let (received_media_sender, received_media) = mpsc::channel(RECEIVED_MEDIA_QUEUE_DEPTH);
        let sender_declared_stereo = Arc::new(OnceLock::new());
        report_state_changes(&peer_connection);
        drain_every_arriving_track(
            &peer_connection,
            received_media_sender,
            Arc::clone(&sender_declared_stereo),
        );
        add_receive_only_transceivers(&peer_connection).await?;

        let sdp_offer = create_offer_once_ice_has_gathered(&peer_connection).await?;
        let opened = signalling.post_offer(&sdp_offer).await?;

        // Before the answer is applied, because applying it is what fires the
        // track handlers that read this.
        let _ = sender_declared_stereo.set(opus_sender_stereo_hint_in_answer(&opened.sdp_answer));

        let answer = RTCSessionDescription::answer(opened.sdp_answer).map_err(|failure| {
            WebRtcExtensionError::Signalling {
                protocol: PROTOCOL,
                what: format!("the relay's answer is not valid SDP: {failure}"),
            }
        })?;
        peer_connection
            .set_remote_description(answer)
            .await
            .map_err(|failure| WebRtcExtensionError::Signalling {
                protocol: PROTOCOL,
                what: format!("the relay's answer was not accepted: {failure}"),
            })?;

        tracing::info!(session_url = opened.session_url, "WHEP session connected");
        Ok(Self {
            signalling,
            peer_connection,
            session_url: opened.session_url,
            received_media,
        })
    }

    /// The next assembled bag, or `None` if none arrived within `timeout`.
    pub async fn next_media(&mut self, timeout: Duration) -> Option<ReceivedMedia> {
        tokio::time::timeout(timeout, self.received_media.recv())
            .await
            .ok()
            .flatten()
    }

    /// Close the peer connection and DELETE the session, bounded as a whole so
    /// the helper's five-second teardown budget cannot be overrun.
    pub async fn close(&self) {
        if tokio::time::timeout(CLOSE_BUDGET, self.close_the_session())
            .await
            .is_err()
        {
            tracing::warn!(
                "the WHEP session did not close inside {CLOSE_BUDGET:?}; \
                 the relay may hold it until it times out"
            );
        }
    }

    async fn close_the_session(&self) {
        if let Err(failure) = self.peer_connection.close().await {
            tracing::warn!(%failure, "closing the WHEP peer connection failed");
        }
        self.signalling.delete_session(&self.session_url).await;
    }
}

async fn build_peer_connection() -> Result<RTCPeerConnection> {
    let mut media_engine = MediaEngine::default();
    for (capability, payload_type, codec_type) in [
        (
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_H264.to_owned(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line:
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                        .to_owned(),
                rtcp_feedback: vec![],
            },
            H264_PAYLOAD_TYPE,
            RTPCodecType::Video,
        ),
        (
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                rtcp_feedback: vec![],
            },
            OPUS_PAYLOAD_TYPE,
            RTPCodecType::Audio,
        ),
    ] {
        media_engine
            .register_codec(
                RTCRtpCodecParameters {
                    capability,
                    payload_type,
                    ..Default::default()
                },
                codec_type,
            )
            .map_err(|failure| WebRtcExtensionError::Transport {
                what: format!("the {codec_type} codec could not be registered: {failure}"),
            })?;
    }

    let registry =
        register_default_interceptors(Registry::new(), &mut media_engine).map_err(|failure| {
            WebRtcExtensionError::Transport {
                what: format!("the RTCP interceptors could not be registered: {failure}"),
            }
        })?;

    APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build()
        .new_peer_connection(RTCConfiguration::default())
        .await
        .map_err(|failure| WebRtcExtensionError::Transport {
            what: format!("the peer connection could not be created: {failure}"),
        })
}

async fn add_receive_only_transceivers(peer_connection: &Arc<RTCPeerConnection>) -> Result<()> {
    for codec_type in [RTPCodecType::Video, RTPCodecType::Audio] {
        peer_connection
            .add_transceiver_from_kind(
                codec_type,
                Some(RTCRtpTransceiverInit {
                    direction: RTCRtpTransceiverDirection::Recvonly,
                    send_encodings: vec![],
                }),
            )
            .await
            .map_err(|failure| WebRtcExtensionError::Transport {
                what: format!("the {codec_type} transceiver could not be added: {failure}"),
            })?;
    }
    Ok(())
}

/// Each arriving track gets its own reader task and its own assembler, because
/// each is a separate producer with its own RTP clock and its own ordering.
fn drain_every_arriving_track(
    peer_connection: &Arc<RTCPeerConnection>,
    received_media: mpsc::Sender<ReceivedMedia>,
    sender_declared_stereo: Arc<OnceLock<Option<bool>>>,
) {
    let video_track_taken = Arc::new(AtomicBool::new(false));
    let audio_track_taken = Arc::new(AtomicBool::new(false));

    peer_connection.on_track(Box::new(move |track, _receiver, _transceiver| {
        let received_media = received_media.clone();
        let sender_declared_stereo = Arc::clone(&sender_declared_stereo);
        let video_track_taken = Arc::clone(&video_track_taken);
        let audio_track_taken = Arc::clone(&audio_track_taken);

        Box::pin(async move {
            let mime_type = track.codec().capability.mime_type;
            let is_video = mime_type.eq_ignore_ascii_case(MIME_TYPE_H264);
            let already_taken = if is_video {
                &video_track_taken
            } else {
                &audio_track_taken
            };

            // Two tracks of one kind would interleave into one output port
            // with two independent ordering counters, which reads downstream
            // as loss rather than as a second stream.
            if already_taken.swap(true, Ordering::SeqCst) {
                tracing::warn!(
                    mime_type,
                    "a second track of this kind arrived and is not being read"
                );
                return;
            }
            tracing::info!(mime_type, "reading a WHEP track");

            tokio::spawn(async move {
                if is_video {
                    read_video_track(track, received_media).await;
                } else {
                    let declared = sender_declared_stereo.get().copied().flatten();
                    read_audio_track(track, received_media, declared).await;
                }
            });
        })
    }));
}

async fn read_video_track(
    track: Arc<webrtc::track::track_remote::TrackRemote>,
    received_media: mpsc::Sender<ReceivedMedia>,
) {
    let mut assembler = VideoAccessUnitAssembler::new();
    while let Ok((packet, _attributes)) = track.read_rtp().await {
        if let Some(access_unit) = assembler.accept_rtp_packet(
            packet.payload,
            packet.header.timestamp,
            packet.header.sequence_number,
            packet.header.marker,
        ) && forward(&received_media, ReceivedMedia::Video(access_unit)).is_err()
        {
            break;
        }
    }
    tracing::debug!("the WHEP video track ended");
}

async fn read_audio_track(
    track: Arc<webrtc::track::track_remote::TrackRemote>,
    received_media: mpsc::Sender<ReceivedMedia>,
    sender_declared_stereo: Option<bool>,
) {
    let mut assembler = OpusPacketAssembler::new(sender_declared_stereo);
    while let Ok((packet, _attributes)) = track.read_rtp().await {
        match assembler.accept_rtp_packet(packet.payload, packet.header.timestamp) {
            Ok(opus_packet) => {
                if forward(&received_media, ReceivedMedia::Audio(opus_packet)).is_err() {
                    break;
                }
            }
            Err(refusal) => tracing::warn!(%refusal, "an Opus packet was dropped"),
        }
    }
    tracing::debug!("the WHEP audio track ended");
}

/// A full queue drops rather than blocks. `Err` means the reader is gone, which
/// is the only condition that ends a track's task.
fn forward(received_media: &mpsc::Sender<ReceivedMedia>, media: ReceivedMedia) -> Result<()> {
    match received_media.try_send(media) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::debug!("the received-media queue is full; a bag was dropped");
            Ok(())
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            Err(WebRtcExtensionError::NotConnected { protocol: PROTOCOL })
        }
    }
}

async fn create_offer_once_ice_has_gathered(
    peer_connection: &Arc<RTCPeerConnection>,
) -> Result<String> {
    let offer = peer_connection
        .create_offer(None)
        .await
        .map_err(|failure| WebRtcExtensionError::Signalling {
            protocol: PROTOCOL,
            what: format!("the offer could not be created: {failure}"),
        })?;
    peer_connection
        .set_local_description(offer)
        .await
        .map_err(|failure| WebRtcExtensionError::Signalling {
            protocol: PROTOCOL,
            what: format!("the offer could not be set as the local description: {failure}"),
        })?;

    let mut gathering_complete = peer_connection.gathering_complete_promise().await;
    let _ = gathering_complete.recv().await;

    peer_connection
        .local_description()
        .await
        .map(|description| description.sdp)
        .ok_or(WebRtcExtensionError::Signalling {
            protocol: PROTOCOL,
            what: "ICE gathering finished with no local description".to_owned(),
        })
}

fn report_state_changes(peer_connection: &Arc<RTCPeerConnection>) {
    peer_connection.on_peer_connection_state_change(Box::new(|state| {
        Box::pin(async move {
            tracing::info!(?state, "WHEP peer connection state");
        })
    }));
    peer_connection.on_ice_connection_state_change(Box::new(|state| {
        Box::pin(async move {
            tracing::debug!(?state, "WHEP ICE connection state");
        })
    }));
}
