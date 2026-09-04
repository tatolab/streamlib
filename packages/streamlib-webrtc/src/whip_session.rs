// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Publishing encoded media to a WHIP endpoint.

use crate::error::{Result, WebRtcExtensionError};
use crate::http_signalling::WhipWhepSignallingClient;
use crate::session_description::opus_format_parameters_for_offer;
use crate::webrtc_peer_connection::{
    H264_PAYLOAD_TYPE, NegotiatedCodec, OPUS_PAYLOAD_TYPE, TrackMedium, apply_the_relays_answer,
    build_peer_connection, close_the_session, create_offer_once_ice_has_gathered,
    opus_codec_capability, report_state_changes, video_codec_capability, video_rtcp_feedback,
};
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use webrtc::track::track_local::TrackLocal;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;

const PROTOCOL: &str = "WHIP";

/// What the session will carry, settled from the publisher's inbound links
/// before the offer is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PublishedMediaSet {
    pub video: bool,
    pub audio: bool,
    /// The channel count the first audio bag declared, when one arrived before
    /// the connect. Absent leaves the sender's hint at stereo, which RFC 7587
    /// §3.1.1 makes a hint and not a promise.
    pub audio_channels: Option<u32>,
}

/// One connected WHIP session: at most one video track and one audio track.
pub(crate) struct WhipPublishingSession {
    signalling: WhipWhepSignallingClient,
    peer_connection: Arc<RTCPeerConnection>,
    video_track: Option<Arc<TrackLocalStaticSample>>,
    audio_track: Option<Arc<TrackLocalStaticSample>>,
    session_url: String,
}

impl WhipPublishingSession {
    /// Build the peer connection, offer it, and set the answer.
    pub(crate) async fn connect(
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

        let peer_connection = Arc::new(
            build_peer_connection(published_codecs(media, &opus_format_parameters)).await?,
        );
        let (gathered_candidates_sender, gathered_candidates) = mpsc::unbounded_channel();
        report_state_changes(&peer_connection, PROTOCOL);
        collect_ice_candidates(&peer_connection, gathered_candidates_sender);

        let video_track = match media.video {
            true => Some(
                add_track(
                    &peer_connection,
                    TrackMedium::Video,
                    video_codec_capability(),
                )
                .await?,
            ),
            false => None,
        };
        let audio_track = match media.audio {
            true => Some(
                add_track(
                    &peer_connection,
                    TrackMedium::Audio,
                    opus_codec_capability(opus_format_parameters),
                )
                .await?,
            ),
            false => None,
        };
        set_every_track_send_only(&peer_connection).await;

        let sdp_offer = create_offer_once_ice_has_gathered(&peer_connection, PROTOCOL).await?;
        let opened = signalling.post_offer(&sdp_offer).await?;
        apply_the_relays_answer(&peer_connection, opened.sdp_answer, PROTOCOL).await?;
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
    pub(crate) async fn write_video_access_unit(
        &self,
        annex_b_access_unit: Bytes,
        duration: Duration,
    ) -> Result<()> {
        write_sample(self.video_track.as_ref(), annex_b_access_unit, duration).await
    }

    /// Hand one Opus packet to the audio track, its duration taken from the
    /// bag's own sample count rather than assumed to be one 20 ms frame.
    pub(crate) async fn write_audio_packet(
        &self,
        opus_packet: Bytes,
        duration: Duration,
    ) -> Result<()> {
        write_sample(self.audio_track.as_ref(), opus_packet, duration).await
    }

    /// What the peer connection reports about itself, for a test that has to
    /// prove media crossed a real connection.
    #[cfg(test)]
    pub(crate) fn peer_connection_state(
        &self,
    ) -> webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState {
        self.peer_connection.connection_state()
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

fn published_codecs(
    media: PublishedMediaSet,
    opus_format_parameters: &str,
) -> Vec<NegotiatedCodec> {
    let mut codecs = Vec::new();
    if media.video {
        codecs.push(NegotiatedCodec {
            capability: video_codec_capability(),
            payload_type: H264_PAYLOAD_TYPE,
            medium: TrackMedium::Video,
            rtcp_feedback: video_rtcp_feedback(),
        });
    }
    if media.audio {
        codecs.push(NegotiatedCodec {
            capability: opus_codec_capability(opus_format_parameters.to_owned()),
            payload_type: OPUS_PAYLOAD_TYPE,
            medium: TrackMedium::Audio,
            rtcp_feedback: vec![],
        });
    }
    codecs
}

async fn write_sample(
    track: Option<&Arc<TrackLocalStaticSample>>,
    encoded_media: Bytes,
    duration: Duration,
) -> Result<()> {
    track
        .ok_or(WebRtcExtensionError::NotConnected { protocol: PROTOCOL })?
        .write_sample(&webrtc::media::Sample {
            data: encoded_media,
            duration,
            ..Default::default()
        })
        .await
        .map_err(|failure| WebRtcExtensionError::Transport {
            what: format!("writing to the WHIP track failed: {failure}"),
        })
}

async fn add_track(
    peer_connection: &Arc<RTCPeerConnection>,
    medium: TrackMedium,
    capability: RTCRtpCodecCapability,
) -> Result<Arc<TrackLocalStaticSample>> {
    let track = Arc::new(TrackLocalStaticSample::new(
        capability,
        medium.as_str().to_owned(),
        format!("streamlib-{}", medium.as_str()),
    ));

    let sender = peer_connection
        .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
        .await
        .map_err(|failure| WebRtcExtensionError::Transport {
            what: format!(
                "the {} track could not be added: {failure}",
                medium.as_str()
            ),
        })?;

    // webrtc-rs stalls its interceptor pipeline unless incoming RTCP is read,
    // and a stalled pipeline emits no Sender Reports for the far end to sync
    // against. The read ends when the sender closes, which ends this task.
    tokio::spawn(async move {
        let mut rtcp_buffer = vec![0u8; 1500];
        while sender.read(&mut rtcp_buffer).await.is_ok() {}
        tracing::debug!(
            medium = medium.as_str(),
            "the RTCP drain for a WHIP track ended"
        );
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
