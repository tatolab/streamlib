// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A real WHIP publish, received by a peer connection in the same process.
//!
//! Everything else in this crate tests one side of a seam against packets a
//! test wrote. This drives the whole path: the offer this wheel builds, its
//! HTTP signalling, ICE and DTLS-SRTP over loopback, webrtc-rs's own H.264
//! payloader fragmenting the access units, and then the receive side's
//! depacketiser and assembler run against what the payloader actually emitted
//! rather than against packets shaped to match their own assumptions.
//!
//! No network and no relay: both peers gather host candidates on 127.0.0.1.

#![cfg(test)]

use crate::h264_test_bitstreams::{baseline_320x180, no_vui};
use crate::http_test_responder::HttpResponderUnderTest;
use crate::received_media_assembly::{OpusPacketAssembler, VideoAccessUnitAssembler};
use crate::webrtc_peer_connection::{
    H264_PAYLOAD_TYPE, NegotiatedCodec, OPUS_PAYLOAD_TYPE, OPUS_RECEIVE_FORMAT_PARAMETERS,
    TrackMedium, build_peer_connection, opus_codec_capability, video_codec_capability,
    video_rtcp_feedback,
};
use crate::whep_session::ReceivedMedia;
use crate::whip_session::{PublishedMediaSet, WhipPublishingSession};
use bytes::Bytes;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

/// ICE plus a DTLS handshake on loopback is milliseconds in the ordinary case;
/// this is the bound past which the test has failed rather than been slow.
const CONNECT_DEADLINE: Duration = Duration::from_secs(20);

/// How long to wait for the access unit to come back out the far side.
const MEDIA_DEADLINE: Duration = Duration::from_secs(15);

/// F=0, NRI=3, type=8 (picture parameter set).
const PICTURE_PARAMETER_SET: &[u8] = &[0x68, 0xCE, 0x3C, 0x80];

/// Big enough that the payloader has to fragment it into FU-As rather than
/// sending it whole, which is the path a real keyframe always takes.
fn an_idr_slice_larger_than_one_packet() -> Vec<u8> {
    let mut nal_unit = vec![0x65];
    // A byte pattern rather than zeroes: a run of zeroes would need emulation
    // prevention, and this is testing packetisation and not escaping.
    nal_unit.extend((0..3000u32).map(|index| (index % 251 + 1) as u8));
    nal_unit
}

fn annex_b(nal_units: &[&[u8]]) -> Vec<u8> {
    let mut stream = Vec::new();
    for nal_unit in nal_units {
        stream.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        stream.extend_from_slice(nal_unit);
    }
    stream
}

fn only_video(media: ReceivedMedia) -> crate::received_media_assembly::ReceivedVideoAccessUnit {
    match media {
        ReceivedMedia::Video(access_unit) => access_unit,
        ReceivedMedia::Audio(_) => panic!("an audio packet arrived on a video-only session"),
    }
}

/// A WHIP endpoint that answers with a real receiving peer connection, and
/// hands back everything it reassembles from what arrives.
async fn a_relay_that_receives_what_is_published()
-> (HttpResponderUnderTest, mpsc::Receiver<ReceivedMedia>) {
    let (assembled, assembled_receiver) = mpsc::channel(64);

    let responder = HttpResponderUnderTest::answering_with(move |request| {
        let offer = request.body.clone();
        let assembled = assembled.clone();
        async move {
            match answer_the_offer(offer, assembled).await {
                Ok(answer) => format!(
                    "HTTP/1.1 201 Created\r\nLocation: /sessions/1\r\n\
                     Content-Type: application/sdp\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{answer}",
                    answer.len()
                ),
                Err(failure) => {
                    let body = format!("the receiving peer failed: {failure}");
                    format!(
                        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\n\
                         Connection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
            }
        }
    })
    .await;

    (responder, assembled_receiver)
}

/// Build the receiving side, take the offer, and hand back the answer.
///
/// The peer connection is leaked into the reading task on purpose: it has to
/// outlive this function, which returns as soon as the answer is written.
async fn answer_the_offer(
    offer: String,
    assembled: mpsc::Sender<ReceivedMedia>,
) -> crate::error::Result<String> {
    let receiving_peer = Arc::new(
        build_peer_connection(vec![
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
        ])
        .await?,
    );

    reassemble_every_arriving_track(&receiving_peer, assembled);

    let refusal = |what: String| crate::error::WebRtcExtensionError::Signalling {
        protocol: "the loopback relay",
        what,
    };
    receiving_peer
        .set_remote_description(
            RTCSessionDescription::offer(offer)
                .map_err(|failure| refusal(format!("the offer is not valid SDP: {failure}")))?,
        )
        .await
        .map_err(|failure| refusal(format!("the offer was not accepted: {failure}")))?;
    let answer = receiving_peer
        .create_answer(None)
        .await
        .map_err(|failure| refusal(format!("no answer could be created: {failure}")))?;
    receiving_peer
        .set_local_description(answer)
        .await
        .map_err(|failure| refusal(format!("the answer could not be set: {failure}")))?;
    let mut gathering_complete = receiving_peer.gathering_complete_promise().await;
    let _ = gathering_complete.recv().await;

    let local = receiving_peer
        .local_description()
        .await
        .ok_or_else(|| refusal("gathering finished with no local description".to_owned()))?;

    // Held for the life of the process the test runs in: dropping it here would
    // close the connection before a single packet arrived.
    std::mem::forget(receiving_peer);
    Ok(local.sdp)
}

fn reassemble_every_arriving_track(
    receiving_peer: &Arc<RTCPeerConnection>,
    assembled: mpsc::Sender<ReceivedMedia>,
) {
    receiving_peer.on_track(Box::new(move |track, _receiver, _transceiver| {
        let medium = TrackMedium::from_mime_type(&track.codec().capability.mime_type);
        let assembled = assembled.clone();
        Box::pin(async move {
            let Some(medium) = medium else { return };
            tokio::spawn(async move {
                match medium {
                    TrackMedium::Video => {
                        let mut assembler = VideoAccessUnitAssembler::new();
                        'reading: while let Ok((packet, _)) = track.read_rtp().await {
                            for access_unit in assembler.accept_rtp_packet(
                                packet.payload,
                                packet.header.timestamp,
                                packet.header.sequence_number,
                                packet.header.marker,
                            ) {
                                if assembled
                                    .send(ReceivedMedia::Video(access_unit))
                                    .await
                                    .is_err()
                                {
                                    break 'reading;
                                }
                            }
                        }
                    }
                    TrackMedium::Audio => {
                        // `None`: the relay is the answerer, so there is no
                        // answer for it to have read a `sprop-stereo` out of.
                        let mut assembler = OpusPacketAssembler::new(None);
                        while let Ok((packet, _)) = track.read_rtp().await {
                            let Ok(opus_packet) = assembler
                                .accept_rtp_packet(packet.payload, packet.header.timestamp)
                            else {
                                continue;
                            };
                            if assembled
                                .send(ReceivedMedia::Audio(opus_packet))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            });
        })
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_published_keyframe_arrives_as_the_same_access_unit_it_was_handed() {
    crate::transport_stack::bring_up().unwrap();
    let (relay, mut assembled) = a_relay_that_receives_what_is_published().await;

    let session = tokio::time::timeout(
        CONNECT_DEADLINE,
        WhipPublishingSession::connect(
            format!("{}/live", relay.origin),
            None,
            PublishedMediaSet {
                video: true,
                audio: false,
                audio_channels: None,
            },
        ),
    )
    .await
    .expect("the WHIP connect did not finish inside its deadline")
    .expect("the WHIP connect failed");

    let sequence_parameter_set = baseline_320x180(no_vui);
    let idr_slice = an_idr_slice_larger_than_one_packet();
    let access_unit = annex_b(&[&sequence_parameter_set, PICTURE_PARAMETER_SET, &idr_slice]);

    // The first frames are written before the DTLS handshake has necessarily
    // completed, and a track drops what it cannot yet send — so this publishes
    // until one arrives rather than asserting on a single write.
    let received = only_video(
        tokio::time::timeout(MEDIA_DEADLINE, async {
            loop {
                session
                    .write_video_access_unit(
                        Bytes::from(access_unit.clone()),
                        Duration::from_millis(33),
                    )
                    .await
                    .expect("writing the access unit failed");
                match tokio::time::timeout(Duration::from_millis(100), assembled.recv()).await {
                    Ok(Some(access_unit)) => return access_unit,
                    Ok(None) => panic!("the receiving side closed"),
                    Err(_) => continue,
                }
            }
        })
        .await
        .expect("no access unit arrived inside the deadline"),
    );

    // Asserted rather than assumed: everything below would also hold if the
    // bytes had somehow reached the assembler without crossing a connection.
    assert_eq!(
        session.peer_connection_state(),
        RTCPeerConnectionState::Connected,
        "the access unit arrived without the peer connection being connected"
    );

    session.close().await;

    // Every NAL unit, in order, through real packetisation: the parameter sets
    // ride a STAP-A and the slice is fragmented into FU-As, and what comes out
    // is the stream that went in.
    assert_eq!(
        received.annex_b_access_unit,
        annex_b(&[&sequence_parameter_set, PICTURE_PARAMETER_SET, &idr_slice]),
        "the reassembled access unit is not the one that was published"
    );
    assert!(received.is_sync_point, "an IDR access unit is a sync point");
    // The coded extent, parsed out of the SPS that survived the round trip
    // rather than taken from config: 320x180 displayed is 320x192 coded.
    assert_eq!((received.width, received.height), (320, 192));
}

/// 960 samples at Opus's 48 kHz clock, which is what the publisher derives a
/// packet's RTP advance from.
const TWENTY_MILLISECONDS: Duration = Duration::from_millis(20);

/// A 20 ms SILK frame, the framing every WebRTC Opus sender uses.
fn an_opus_packet(stereo: bool) -> Vec<u8> {
    let table_of_contents = (1u8 << 3) | (u8::from(stereo) << 2);
    let mut packet = vec![table_of_contents];
    packet.extend((0..60u32).map(|index| (index % 251 + 1) as u8));
    packet
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_published_opus_packet_arrives_whole_and_described() {
    crate::transport_stack::bring_up().unwrap();
    let (relay, mut assembled) = a_relay_that_receives_what_is_published().await;

    let session = tokio::time::timeout(
        CONNECT_DEADLINE,
        WhipPublishingSession::connect(
            format!("{}/live", relay.origin),
            None,
            PublishedMediaSet {
                video: false,
                audio: true,
                audio_channels: Some(1),
            },
        ),
    )
    .await
    .expect("the WHIP connect did not finish inside its deadline")
    .expect("the WHIP connect failed");

    let opus_packet = an_opus_packet(false);
    let received = tokio::time::timeout(MEDIA_DEADLINE, async {
        loop {
            session
                .write_audio_packet(Bytes::from(opus_packet.clone()), TWENTY_MILLISECONDS)
                .await
                .expect("writing the Opus packet failed");
            match tokio::time::timeout(Duration::from_millis(100), assembled.recv()).await {
                Ok(Some(ReceivedMedia::Audio(packet))) => return packet,
                Ok(Some(ReceivedMedia::Video(_))) => {
                    panic!("a video access unit arrived on an audio-only session")
                }
                Ok(None) => panic!("the receiving side closed"),
                Err(_) => continue,
            }
        }
    })
    .await
    .expect("no Opus packet arrived inside the deadline");

    assert_eq!(
        session.peer_connection_state(),
        RTCPeerConnectionState::Connected,
        "the packet arrived without the peer connection being connected"
    );
    session.close().await;

    // The payload crosses unchanged: an Opus payloader packetises one packet
    // per RTP packet. The channel count here comes from the first packet's TOC
    // rather than from an answer's `sprop-stereo`, because the relay in this
    // test is the answerer and so reads no answer of its own — the
    // answer-declared path is covered by the assembler's unit tests.
    assert_eq!(received.opus_packet, Bytes::from(opus_packet));
    assert_eq!(received.sample_count, 960);
    assert_eq!(received.sample_rate, 48_000);
    assert_eq!(received.channels, 1);
    assert!(received.pre_skip == 0, "RTP carries no OpusHead to skip by");
}
