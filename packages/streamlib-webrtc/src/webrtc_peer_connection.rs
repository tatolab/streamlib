// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a WHIP publish and a WHEP play share: the codecs they negotiate, the
//! peer connection they build, and the offer/answer exchange they make.
//!
//! The two differ in their media direction and in nothing else at this layer,
//! so the protocol name is a parameter here exactly as it is on the signalling
//! client, and every refusal names the caller rather than this module.

use crate::error::{Result, WebRtcExtensionError};
use crate::http_signalling::WhipWhepSignallingClient;
use std::sync::Arc;
use std::time::Duration;
use webrtc::api::APIBuilder;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MIME_TYPE_H264, MIME_TYPE_OPUS, MediaEngine};
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::RTCPFeedback;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType,
};

/// The payload types and clock rates both directions negotiate. Spelled once:
/// a wire constant with two spellings drifts.
pub(crate) const H264_PAYLOAD_TYPE: u8 = 102;
pub(crate) const OPUS_PAYLOAD_TYPE: u8 = 111;
pub(crate) const H264_CLOCK_RATE_HZ: u32 = 90_000;
/// Opus's RTP clock is its wire sample rate; the two are one number.
pub(crate) const OPUS_CLOCK_RATE_HZ: u32 = crate::opus_packet::OPUS_WIRE_SAMPLE_RATE_HZ;

/// Constrained-baseline 3.1, the profile every WHIP relay accepts, in the
/// asymmetric form RFC 6184 §8.2.2 defines.
pub(crate) const H264_FORMAT_PARAMETERS: &str =
    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";

/// The Opus fmtp a receiver offers, which declares nothing about what it will
/// itself send.
pub(crate) const OPUS_RECEIVE_FORMAT_PARAMETERS: &str = "minptime=10;useinbandfec=1";

/// RFC 7587 §7 fixes Opus's rtpmap encoding parameter at 2 whatever the stream
/// actually carries, so a channel count reaches the far end as the fmtp's
/// `sprop-stereo` and never here.
pub(crate) const OPUS_RTPMAP_CHANNELS: u16 = 2;

/// Host-only ICE gathering on a reachable relay is prompt, but this sits inside
/// a `connect()` that WHIP calls from `process()` on the first bag — so a stall
/// has to end in a usable offer rather than in a processor that never returns.
const ICE_GATHERING_BUDGET: Duration = Duration::from_secs(10);

/// Which medium a track carries. The mime type is stringly typed on the wire;
/// this is where that stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackMedium {
    Video,
    Audio,
}

impl TrackMedium {
    /// `None` for a mime type neither side negotiated, which is a track to
    /// refuse by name rather than to guess the medium of.
    pub(crate) fn from_mime_type(mime_type: &str) -> Option<Self> {
        if mime_type.eq_ignore_ascii_case(MIME_TYPE_H264) {
            Some(Self::Video)
        } else if mime_type.eq_ignore_ascii_case(MIME_TYPE_OPUS) {
            Some(Self::Audio)
        } else {
            None
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
        }
    }
}

pub(crate) fn video_codec_capability() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_H264.to_owned(),
        clock_rate: H264_CLOCK_RATE_HZ,
        channels: 0,
        sdp_fmtp_line: H264_FORMAT_PARAMETERS.to_owned(),
        rtcp_feedback: vec![],
    }
}

pub(crate) fn opus_codec_capability(format_parameters: String) -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: OPUS_CLOCK_RATE_HZ,
        channels: OPUS_RTPMAP_CHANNELS,
        sdp_fmtp_line: format_parameters,
        rtcp_feedback: vec![],
    }
}

/// Without NACK and PLI a relay has no way to ask for a fresh IDR after loss,
/// and the far end stays frozen until the next one.
pub(crate) fn video_rtcp_feedback() -> Vec<RTCPFeedback> {
    ["nack", "goog-remb"]
        .into_iter()
        .map(|typ| RTCPFeedback {
            typ: typ.to_owned(),
            parameter: String::new(),
        })
        .chain(std::iter::once(RTCPFeedback {
            typ: "nack".to_owned(),
            parameter: "pli".to_owned(),
        }))
        .collect()
}

/// One codec to register, with the payload type and feedback it takes.
pub(crate) struct NegotiatedCodec {
    pub capability: RTCRtpCodecCapability,
    pub payload_type: u8,
    pub medium: TrackMedium,
    pub rtcp_feedback: Vec<RTCPFeedback>,
}

/// Build a peer connection offering exactly `codecs`.
pub(crate) async fn build_peer_connection(
    codecs: Vec<NegotiatedCodec>,
) -> Result<RTCPeerConnection> {
    let mut media_engine = MediaEngine::default();
    for codec in codecs {
        let codec_type = match codec.medium {
            TrackMedium::Video => RTPCodecType::Video,
            TrackMedium::Audio => RTPCodecType::Audio,
        };
        media_engine
            .register_codec(
                RTCRtpCodecParameters {
                    capability: RTCRtpCodecCapability {
                        rtcp_feedback: codec.rtcp_feedback,
                        ..codec.capability
                    },
                    payload_type: codec.payload_type,
                    ..Default::default()
                },
                codec_type,
            )
            .map_err(|failure| WebRtcExtensionError::Transport {
                what: format!(
                    "the {} codec could not be registered: {failure}",
                    codec.medium.as_str()
                ),
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

/// Offer with the candidates already in it. A relay that supports trickle gets
/// the late ones by PATCH; one that does not still has a usable offer.
pub(crate) async fn create_offer_once_ice_has_gathered(
    peer_connection: &Arc<RTCPeerConnection>,
    protocol: &'static str,
) -> Result<String> {
    let refusal = |what: String| WebRtcExtensionError::Signalling { protocol, what };

    let offer = peer_connection
        .create_offer(None)
        .await
        .map_err(|failure| refusal(format!("the offer could not be created: {failure}")))?;
    peer_connection
        .set_local_description(offer)
        .await
        .map_err(|failure| {
            refusal(format!(
                "the offer could not be set as the local description: {failure}"
            ))
        })?;

    let mut gathering_complete = peer_connection.gathering_complete_promise().await;
    if tokio::time::timeout(ICE_GATHERING_BUDGET, gathering_complete.recv())
        .await
        .is_err()
    {
        // Whatever gathered is still a usable offer, and a relay that supports
        // trickle gets the rest by PATCH.
        tracing::warn!(
            protocol,
            "ICE gathering did not finish inside {ICE_GATHERING_BUDGET:?}; \
             offering the candidates gathered so far"
        );
    }

    peer_connection
        .local_description()
        .await
        .map(|description| description.sdp)
        .ok_or_else(|| refusal("ICE gathering finished with no local description".to_owned()))
}

/// Apply the relay's answer, refusing one this peer cannot accept.
pub(crate) async fn apply_the_relays_answer(
    peer_connection: &Arc<RTCPeerConnection>,
    sdp_answer: String,
    protocol: &'static str,
) -> Result<()> {
    let answer = RTCSessionDescription::answer(sdp_answer).map_err(|failure| {
        WebRtcExtensionError::Signalling {
            protocol,
            what: format!("the relay's answer is not valid SDP: {failure}"),
        }
    })?;
    peer_connection
        .set_remote_description(answer)
        .await
        .map_err(|failure| WebRtcExtensionError::Signalling {
            protocol,
            what: format!("the relay's answer was not accepted: {failure}"),
        })
}

pub(crate) fn report_state_changes(
    peer_connection: &Arc<RTCPeerConnection>,
    protocol: &'static str,
) {
    peer_connection.on_peer_connection_state_change(Box::new(move |peer_connection_state| {
        Box::pin(async move {
            tracing::info!(protocol, ?peer_connection_state, "peer connection state");
        })
    }));
    peer_connection.on_ice_connection_state_change(Box::new(move |ice_connection_state| {
        Box::pin(async move {
            tracing::debug!(protocol, ?ice_connection_state, "ICE connection state");
        })
    }));
}

/// The helper's teardown reply is bounded at five seconds before the parent
/// stops waiting, and the peer connection's own close has no bound of its own.
/// This leaves room for the join that precedes it.
const CLOSE_BUDGET: Duration = Duration::from_secs(3);

/// Close the peer connection and DELETE the session, bounded as a whole.
pub(crate) async fn close_the_session(
    peer_connection: &Arc<RTCPeerConnection>,
    signalling: &WhipWhepSignallingClient,
    session_url: &str,
    protocol: &'static str,
) {
    let closing = async {
        if let Err(failure) = peer_connection.close().await {
            tracing::warn!(protocol, %failure, "closing the peer connection failed");
        }
        signalling.delete_session(session_url).await;
    };

    if tokio::time::timeout(CLOSE_BUDGET, closing).await.is_err() {
        tracing::warn!(
            protocol,
            "the session did not close inside {CLOSE_BUDGET:?}; \
             the relay may hold it until it times out"
        );
    }
}
