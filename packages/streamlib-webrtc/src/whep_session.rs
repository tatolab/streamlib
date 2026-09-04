// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Playing encoded media back from a WHEP endpoint.

use crate::error::{Result, WebRtcExtensionError};
use crate::http_signalling::WhipWhepSignallingClient;
use crate::received_media_assembly::{
    OpusPacketAssembler, ReceivedOpusPacket, ReceivedVideoAccessUnit, VideoAccessUnitAssembler,
};
use crate::session_description::opus_sender_stereo_hint_in_answer;
use crate::webrtc_peer_connection::{
    H264_PAYLOAD_TYPE, NegotiatedCodec, OPUS_PAYLOAD_TYPE, OPUS_RECEIVE_FORMAT_PARAMETERS,
    TrackMedium, apply_the_relays_answer, build_peer_connection, close_the_session,
    create_offer_once_ice_has_gathered, opus_codec_capability, report_state_changes,
    video_codec_capability, video_rtcp_feedback,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::track::track_remote::TrackRemote;

const PROTOCOL: &str = "WHEP";

/// How many assembled bags may wait for the processor thread to collect them.
/// Bounded and dropped rather than blocked: a stalled reader must not stall the
/// network loop, and a gap in `sequence_index` is how a consumer learns it lost
/// something — which is exactly what happened.
const RECEIVED_MEDIA_QUEUE_DEPTH: usize = 256;

/// One bag's worth of media, assembled and ready to be spelled.
#[derive(Debug, Clone)]
pub(crate) enum ReceivedMedia {
    Video(ReceivedVideoAccessUnit),
    Audio(ReceivedOpusPacket),
}

/// Which mediums already have a reader, so a second track of one kind is
/// refused rather than interleaved into the first one's ordering.
#[derive(Default)]
struct TrackMediumsAlreadyTaken {
    video: AtomicBool,
    audio: AtomicBool,
}

impl TrackMediumsAlreadyTaken {
    /// `true` if this medium was already claimed by an earlier track.
    fn claim(&self, medium: TrackMedium) -> bool {
        match medium {
            TrackMedium::Video => &self.video,
            TrackMedium::Audio => &self.audio,
        }
        .swap(true, Ordering::SeqCst)
    }
}

/// One connected WHEP session, draining into an assembled-media queue.
pub(crate) struct WhepPlayingSession {
    signalling: WhipWhepSignallingClient,
    peer_connection: Arc<RTCPeerConnection>,
    session_url: String,
    received_media: mpsc::Receiver<ReceivedMedia>,
}

impl WhepPlayingSession {
    /// Offer two receive-only transceivers and start draining what arrives.
    pub(crate) async fn connect(
        endpoint_url: String,
        bearer_token: Option<String>,
    ) -> Result<Self> {
        let signalling = WhipWhepSignallingClient::new(endpoint_url, bearer_token, PROTOCOL)?;
        let peer_connection = Arc::new(build_peer_connection(played_codecs()).await?);

        let (received_media_sender, received_media) = mpsc::channel(RECEIVED_MEDIA_QUEUE_DEPTH);
        let sender_declared_stereo = Arc::new(OnceLock::new());
        report_state_changes(&peer_connection, PROTOCOL);
        drain_every_arriving_track(
            &peer_connection,
            received_media_sender,
            Arc::clone(&sender_declared_stereo),
        );
        add_receive_only_transceivers(&peer_connection).await?;

        let sdp_offer = create_offer_once_ice_has_gathered(&peer_connection, PROTOCOL).await?;
        let opened = signalling.post_offer(&sdp_offer).await?;

        // Before the answer is applied, because applying it is what fires the
        // track handlers that read this.
        let _ = sender_declared_stereo.set(opus_sender_stereo_hint_in_answer(&opened.sdp_answer));
        apply_the_relays_answer(&peer_connection, opened.sdp_answer, PROTOCOL).await?;

        tracing::info!(session_url = opened.session_url, "WHEP session connected");
        Ok(Self {
            signalling,
            peer_connection,
            session_url: opened.session_url,
            received_media,
        })
    }

    /// The next assembled bag, or `None` if none arrived within `timeout`.
    pub(crate) async fn next_media(&mut self, timeout: Duration) -> Option<ReceivedMedia> {
        tokio::time::timeout(timeout, self.received_media.recv())
            .await
            .ok()
            .flatten()
    }

    /// Close the peer connection and DELETE the session, bounded as a whole so
    /// the helper's five-second teardown budget cannot be overrun.
    pub(crate) async fn close(&self) {
        close_the_session(
            &self.peer_connection,
            &self.signalling,
            &self.session_url,
            PROTOCOL,
        )
        .await;
    }
}

fn played_codecs() -> Vec<NegotiatedCodec> {
    vec![
        NegotiatedCodec {
            capability: video_codec_capability(),
            payload_type: H264_PAYLOAD_TYPE,
            medium: TrackMedium::Video,
            rtcp_feedback: video_rtcp_feedback(),
        },
        NegotiatedCodec {
            capability: opus_codec_capability(OPUS_RECEIVE_FORMAT_PARAMETERS.to_owned()),
            payload_type: OPUS_PAYLOAD_TYPE,
            medium: TrackMedium::Audio,
            rtcp_feedback: vec![],
        },
    ]
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
    let already_taken = Arc::new(TrackMediumsAlreadyTaken::default());

    peer_connection.on_track(Box::new(move |track, _receiver, _transceiver| {
        let mime_type = track.codec().capability.mime_type;
        let medium = TrackMedium::from_mime_type(&mime_type);
        let received_media = received_media.clone();
        let sender_declared_stereo = Arc::clone(&sender_declared_stereo);
        let already_taken = Arc::clone(&already_taken);

        Box::pin(async move {
            let Some(medium) = medium else {
                tracing::warn!(
                    mime_type,
                    "a track arrived carrying a codec this session never negotiated; \
                     it is not being read"
                );
                return;
            };
            // Two tracks of one medium would interleave into one output port
            // with two independent ordering counters, which reads downstream
            // as loss rather than as a second stream.
            if already_taken.claim(medium) {
                tracing::warn!(
                    mime_type,
                    "a second {} track arrived and is not being read",
                    medium.as_str()
                );
                return;
            }
            tracing::info!(mime_type, "reading a WHEP track");

            tokio::spawn(async move {
                match medium {
                    TrackMedium::Video => read_video_track(track, received_media).await,
                    TrackMedium::Audio => {
                        let declared = sender_declared_stereo.get().copied().flatten();
                        read_audio_track(track, received_media, declared).await
                    }
                }
            });
        })
    }));
}

async fn read_video_track(track: Arc<TrackRemote>, received_media: mpsc::Sender<ReceivedMedia>) {
    let mut assembler = VideoAccessUnitAssembler::new();
    'reading: while let Ok((packet, _attributes)) = track.read_rtp().await {
        for access_unit in assembler.accept_rtp_packet(
            packet.payload,
            packet.header.timestamp,
            packet.header.sequence_number,
            packet.header.marker,
        ) {
            if try_forward_to_the_reading_thread(&received_media, ReceivedMedia::Video(access_unit))
                .is_err()
            {
                break 'reading;
            }
        }
    }
    tracing::debug!("the WHEP video track ended");
}

async fn read_audio_track(
    track: Arc<TrackRemote>,
    received_media: mpsc::Sender<ReceivedMedia>,
    sender_declared_stereo: Option<bool>,
) {
    let mut assembler = OpusPacketAssembler::new(sender_declared_stereo);
    while let Ok((packet, _attributes)) = track.read_rtp().await {
        match assembler.accept_rtp_packet(packet.payload, packet.header.timestamp) {
            Ok(opus_packet) => {
                if try_forward_to_the_reading_thread(
                    &received_media,
                    ReceivedMedia::Audio(opus_packet),
                )
                .is_err()
                {
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
fn try_forward_to_the_reading_thread(
    received_media: &mpsc::Sender<ReceivedMedia>,
    media: ReceivedMedia,
) -> Result<()> {
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
