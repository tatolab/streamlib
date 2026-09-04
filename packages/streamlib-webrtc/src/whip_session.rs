// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Publishing encoded media to a WHIP endpoint.

use crate::error::{Result, WebRtcExtensionError};
use crate::http_signalling::WhipWhepSignallingClient;
use crate::session_description::opus_format_parameters_for_offer;
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
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
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::track::track_local::TrackLocal;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

const PROTOCOL: &str = "WHIP";
const H264_PAYLOAD_TYPE: u8 = 102;
const OPUS_PAYLOAD_TYPE: u8 = 111;
const H264_CLOCK_RATE_HZ: u32 = 90_000;
const OPUS_CLOCK_RATE_HZ: u32 = 48_000;

/// Constrained-baseline 3.1, the profile every WHIP relay accepts, in the
/// asymmetric form RFC 6184 §8.2.2 defines.
const H264_FORMAT_PARAMETERS: &str =
    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";

/// RFC 7587 §7 fixes Opus's rtpmap encoding parameter at 2 whatever the stream
/// actually carries, so the channel count reaches the far end as the fmtp's
/// `sprop-stereo` and never here.
const OPUS_RTPMAP_CHANNELS: u16 = 2;

/// What the session will carry, settled from the publisher's inbound links
/// before the offer is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishedMediaSet {
    pub video: bool,
    pub audio: bool,
    /// The channel count the first audio bag declared, when one arrived before
    /// the connect. Absent leaves the sender's hint at stereo, which RFC 7587
    /// §3.1.1 makes a hint and not a promise.
    pub audio_channels: Option<u32>,
}

/// One connected WHIP session: at most one video track and one audio track.
pub struct WhipPublishingSession {
    signalling: WhipWhepSignallingClient,
    peer_connection: Arc<RTCPeerConnection>,
    video_track: Option<Arc<TrackLocalStaticSample>>,
    audio_track: Option<Arc<TrackLocalStaticSample>>,
    session_url: String,
}

impl WhipPublishingSession {
    /// Build the peer connection, offer it, and set the answer.
    pub async fn connect(
        endpoint_url: String,
        bearer_token: Option<String>,
        media: PublishedMediaSet,
    ) -> Result<Self> {
        if !media.video && !media.audio {
            return Err(WebRtcExtensionError::Refused {
                what: "a WHIP session carrying neither video nor audio".to_owned(),
            });
        }

        let signalling = WhipWhepSignallingClient::new(endpoint_url, bearer_token, PROTOCOL)?;
        let opus_format_parameters =
            opus_format_parameters_for_offer(media.audio_channels.unwrap_or(2));

        let peer_connection =
            Arc::new(build_peer_connection(media, &opus_format_parameters).await?);
        let (gathered_candidates_sender, gathered_candidates) = mpsc::unbounded_channel();
        report_state_changes(&peer_connection);
        collect_ice_candidates(&peer_connection, gathered_candidates_sender);

        let video_track = if media.video {
            Some(add_track(&peer_connection, video_codec_capability()).await?)
        } else {
            None
        };
        let audio_track = if media.audio {
            Some(
                add_track(
                    &peer_connection,
                    opus_codec_capability(opus_format_parameters.clone()),
                )
                .await?,
            )
        } else {
            None
        };
        set_every_track_send_only(&peer_connection).await;

        let sdp_offer = create_offer_once_ice_has_gathered(&peer_connection).await?;
        let opened = signalling.post_offer(&sdp_offer).await?;

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

        trickle_gathered_candidates(&signalling, &opened.session_url, gathered_candidates).await;

        tracing::info!(session_url = opened.session_url, "WHIP session connected");
        Ok(Self {
            signalling,
            peer_connection,
            video_track,
            audio_track,
            session_url: opened.session_url,
        })
    }

    /// Hand one whole Annex-B access unit to the H.264 payloader, which does
    /// the STAP-A aggregation and FU-A fragmentation itself.
    ///
    /// `duration` advances the track's RTP clock to the *next* sample, so it is
    /// the gap to the frame after this one and not this frame's own length.
    pub async fn write_video_access_unit(
        &self,
        annex_b_access_unit: Bytes,
        duration: Duration,
    ) -> Result<()> {
        let track = self
            .video_track
            .as_ref()
            .ok_or(WebRtcExtensionError::NotConnected { protocol: PROTOCOL })?;
        write_sample(track, annex_b_access_unit, duration).await
    }

    /// Hand one Opus packet to the audio track, its duration taken from the
    /// bag's own sample count rather than assumed to be one 20 ms frame.
    pub async fn write_audio_packet(
        &self,
        opus_packet: Bytes,
        duration: Duration,
    ) -> Result<()> {
        let track = self
            .audio_track
            .as_ref()
            .ok_or(WebRtcExtensionError::NotConnected { protocol: PROTOCOL })?;
        write_sample(track, opus_packet, duration).await
    }

    /// Close the peer connection and DELETE the session, both bounded so the
    /// helper's five-second teardown budget cannot be overrun.
    pub async fn close(&self) {
        if let Err(failure) = self.peer_connection.close().await {
            tracing::warn!(%failure, "closing the WHIP peer connection failed");
        }
        self.signalling.delete_session(&self.session_url).await;
    }
}

async fn write_sample(
    track: &Arc<TrackLocalStaticSample>,
    data: Bytes,
    duration: Duration,
) -> Result<()> {
    track
        .write_sample(&webrtc::media::Sample {
            data,
            duration,
            ..Default::default()
        })
        .await
        .map_err(|failure| WebRtcExtensionError::Transport {
            what: format!("writing to the WHIP track failed: {failure}"),
        })
}

fn video_codec_capability() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_H264.to_owned(),
        clock_rate: H264_CLOCK_RATE_HZ,
        channels: 0,
        sdp_fmtp_line: H264_FORMAT_PARAMETERS.to_owned(),
        rtcp_feedback: vec![],
    }
}

fn opus_codec_capability(format_parameters: String) -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_OPUS.to_owned(),
        clock_rate: OPUS_CLOCK_RATE_HZ,
        channels: OPUS_RTPMAP_CHANNELS,
        sdp_fmtp_line: format_parameters,
        rtcp_feedback: vec![],
    }
}

async fn build_peer_connection(
    media: PublishedMediaSet,
    opus_format_parameters: &str,
) -> Result<RTCPeerConnection> {
    let mut media_engine = MediaEngine::default();
    if media.video {
        register_codec(
            &mut media_engine,
            video_codec_capability(),
            H264_PAYLOAD_TYPE,
            RTPCodecType::Video,
            // Without NACK and PLI the relay has no way to ask for a fresh IDR
            // after loss, and the far end stays frozen until the next one.
            vec![
                RTCPFeedback {
                    typ: "nack".to_owned(),
                    parameter: String::new(),
                },
                RTCPFeedback {
                    typ: "nack".to_owned(),
                    parameter: "pli".to_owned(),
                },
                RTCPFeedback {
                    typ: "goog-remb".to_owned(),
                    parameter: String::new(),
                },
            ],
        )?;
    }
    if media.audio {
        register_codec(
            &mut media_engine,
            opus_codec_capability(opus_format_parameters.to_owned()),
            OPUS_PAYLOAD_TYPE,
            RTPCodecType::Audio,
            vec![],
        )?;
    }

    let registry = register_default_interceptors(Registry::new(), &mut media_engine).map_err(
        |failure| WebRtcExtensionError::Transport {
            what: format!("the RTCP interceptors could not be registered: {failure}"),
        },
    )?;

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

fn register_codec(
    media_engine: &mut MediaEngine,
    capability: RTCRtpCodecCapability,
    payload_type: u8,
    codec_type: RTPCodecType,
    rtcp_feedback: Vec<RTCPFeedback>,
) -> Result<()> {
    media_engine
        .register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    rtcp_feedback,
                    ..capability
                },
                payload_type,
                ..Default::default()
            },
            codec_type,
        )
        .map_err(|failure| WebRtcExtensionError::Transport {
            what: format!("the {codec_type} codec could not be registered: {failure}"),
        })
}

async fn add_track(
    peer_connection: &Arc<RTCPeerConnection>,
    capability: RTCRtpCodecCapability,
) -> Result<Arc<TrackLocalStaticSample>> {
    let kind = if capability.mime_type == MIME_TYPE_H264 {
        "video"
    } else {
        "audio"
    };
    let track = Arc::new(TrackLocalStaticSample::new(
        capability,
        kind.to_owned(),
        format!("streamlib-{kind}"),
    ));

    let sender = peer_connection
        .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
        .await
        .map_err(|failure| WebRtcExtensionError::Transport {
            what: format!("the {kind} track could not be added: {failure}"),
        })?;

    // webrtc-rs stalls its interceptor pipeline unless incoming RTCP is read,
    // and a stalled pipeline emits no Sender Reports for the far end to sync
    // against. The read ends when the sender closes, which ends this task.
    tokio::spawn(async move {
        let mut rtcp_buffer = vec![0u8; 1500];
        while sender.read(&mut rtcp_buffer).await.is_ok() {}
        tracing::debug!(kind, "the RTCP drain for a WHIP track ended");
    });

    Ok(track)
}

async fn set_every_track_send_only(peer_connection: &Arc<RTCPeerConnection>) {
    for transceiver in peer_connection.get_transceivers().await {
        if transceiver.sender().await.track().await.is_some() {
            transceiver
                .set_direction(RTCRtpTransceiverDirection::Sendonly)
                .await;
        }
    }
}

/// Offer with the candidates already in it. A relay that supports trickle gets
/// the late ones by PATCH; one that does not still has a usable offer.
async fn create_offer_once_ice_has_gathered(
    peer_connection: &Arc<RTCPeerConnection>,
) -> Result<String> {
    let offer = peer_connection.create_offer(None).await.map_err(|failure| {
        WebRtcExtensionError::Signalling {
            protocol: PROTOCOL,
            what: format!("the offer could not be created: {failure}"),
        }
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

fn collect_ice_candidates(
    peer_connection: &Arc<RTCPeerConnection>,
    gathered: mpsc::UnboundedSender<String>,
) {
    peer_connection.on_ice_candidate(Box::new(move |candidate| {
        let gathered = gathered.clone();
        Box::pin(async move {
            if let Some(candidate) = candidate
                && let Ok(as_json) = candidate.to_json()
            {
                let _ = gathered.send(format!("a={}", as_json.candidate));
            }
        })
    }));
}

fn report_state_changes(peer_connection: &Arc<RTCPeerConnection>) {
    peer_connection.on_peer_connection_state_change(Box::new(|state| {
        Box::pin(async move {
            tracing::info!(?state, "WHIP peer connection state");
        })
    }));
    peer_connection.on_ice_connection_state_change(Box::new(|state| {
        Box::pin(async move {
            tracing::debug!(?state, "WHIP ICE connection state");
        })
    }));
}

/// A relay that does not implement trickle answers with an error, which is not
/// a connect failure — the offer already carried every gathered candidate.
async fn trickle_gathered_candidates(
    signalling: &WhipWhepSignallingClient,
    session_url: &str,
    mut gathered: mpsc::UnboundedReceiver<String>,
) {
    let mut candidates = Vec::new();
    while let Ok(candidate) = gathered.try_recv() {
        candidates.push(candidate);
    }
    if candidates.is_empty() {
        return;
    }
    if let Err(failure) = signalling
        .patch_ice_candidates(session_url, candidates.join("\r\n"))
        .await
    {
        tracing::debug!(%failure, "the relay did not accept trickled candidates");
    }
}
