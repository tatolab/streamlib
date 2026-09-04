# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two processors as a graph sees them.

`rt.add` with no adapter and no engine change is the whole claim of the
extension model, so it is what these check — over a real `Runtime`, which needs
no device to build a graph.
"""

import pytest

import streamlib
from streamlib import H264Decoder, H264Encoder, OpusDecoder, OpusEncoder, log
from streamlib_webrtc import WhepPlayer, WhipPublisher
from streamlib_webrtc.processors import (
    HELPER_LINK_PAYLOAD_CEILING_BYTES,
    VideoOrAudio,
    refuse_audio_rtp_cannot_carry,
    resolve_track_kind,
)


@pytest.fixture
def runtime():
    runtime = streamlib.Runtime()
    try:
        yield runtime
    finally:
        runtime.shutdown()


@pytest.mark.parametrize("processor_class", [WhipPublisher, WhepPlayer])
def test_an_installed_extensions_processor_is_added_like_any_other(
    runtime, processor_class
):
    added = runtime.add(processor_class, config={"url": "https://example.invalid/x"})

    assert added.display_name == processor_class.__name__


def test_the_publisher_wires_to_both_encoders_without_an_adapter(runtime):
    """One fan-in port takes both encoders, which is what makes a WHIP session
    with video and audio a matter of wiring rather than of config."""
    video_encoder = runtime.add(H264Encoder)
    audio_encoder = runtime.add(OpusEncoder)
    publisher = runtime.add(
        WhipPublisher, config={"url": "https://example.invalid/whip"}
    )

    runtime.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    runtime.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))


def test_the_player_wires_to_both_decoders_without_an_adapter(runtime):
    player = runtime.add(WhepPlayer, config={"url": "https://example.invalid/whep"})
    video_decoder = runtime.add(H264Decoder)
    audio_decoder = runtime.add(OpusDecoder)

    runtime.connect(
        player.output("encoded_video"), video_decoder.input("encoded_video")
    )
    runtime.connect(
        player.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )


def test_a_publish_and_play_round_trip_composes_as_published(runtime):
    """The shape the live proof drives: encode, publish, play back, decode."""
    encoder = runtime.add(H264Encoder)
    publisher = runtime.add(
        WhipPublisher, config={"url": "https://example.invalid/whip"}
    )
    player = runtime.add(WhepPlayer, config={"url": "https://example.invalid/whep"})
    decoder = runtime.add(H264Decoder)

    runtime.connect(encoder.output("encoded_video"), publisher.input("tracks"))
    runtime.connect(player.output("encoded_video"), decoder.input("encoded_video"))


def test_each_codec_names_the_track_it_belongs_on():
    assert resolve_track_kind("h264", "encoder/encoded_video", {}) == "video"
    assert resolve_track_kind("opus", "encoder/encoded_audio", {}) == "audio"


@pytest.mark.parametrize("codec", ["h265", "av1", "", None, 5])
def test_a_codec_this_session_does_not_carry_is_refused_naming_it(codec):
    """H.265 included: the offer registers H.264 and Opus, so a stream this
    session never negotiated is a refusal and not a silent wrong track."""
    with pytest.raises(ValueError, match="does not carry"):
        resolve_track_kind(codec, "encoder/encoded_video", {})


def test_two_links_publishing_one_medium_are_refused_naming_both():
    already: "dict[str, VideoOrAudio]" = {"first/encoded_video": "video"}

    with pytest.raises(ValueError, match="second/encoded_video.*first/encoded_video"):
        resolve_track_kind("h264", "second/encoded_video", already)


def test_a_link_that_changes_medium_is_refused_naming_it():
    already: "dict[str, VideoOrAudio]" = {"encoder/tracks": "video"}

    with pytest.raises(ValueError, match="does not change medium"):
        resolve_track_kind("opus", "encoder/tracks", already)


def test_a_link_publishing_the_medium_it_already_claimed_is_unchanged():
    already: "dict[str, VideoOrAudio]" = {"encoder/encoded_video": "video"}

    assert resolve_track_kind("h264", "encoder/encoded_video", already) == "video"


def test_an_oversized_bag_is_reported_once_rather_than_silently_dropped(
    monkeypatch,
):
    """The engine drops a bag past the link ceiling at `debug` rather than
    raising, which downstream looks like a stream that simply stopped. The
    player is the only place that can say what actually happened — and a
    per-frame condition reported per frame is noise, so it says it once."""
    reported: "list[str]" = []
    monkeypatch.setattr(log, "error", reported.append)
    player = WhepPlayer()
    over_the_ceiling = b"\x00" * (HELPER_LINK_PAYLOAD_CEILING_BYTES + 1)

    player._report_a_bag_the_link_will_drop("encoded_video", over_the_ceiling)
    player._report_a_bag_the_link_will_drop("encoded_video", over_the_ceiling)

    assert len(reported) == 1
    assert "encoded_video" in reported[0]
    assert str(len(over_the_ceiling)) in reported[0]


def test_a_bag_inside_the_ceiling_is_not_reported(monkeypatch):
    reported: "list[str]" = []
    monkeypatch.setattr(log, "error", reported.append)

    WhepPlayer()._report_a_bag_the_link_will_drop("encoded_video", b"\x00" * 4096)

    assert reported == []


@pytest.mark.parametrize("channels", [3, 6, 8])
def test_multichannel_opus_is_refused_because_rtp_cannot_carry_it(channels):
    """`OpusEncoder` legally produces mapping-family-1 multistream packets for
    3-8 channels, and RFC 7587 defines an RTP payload format for mono and
    stereo only. Forwarded anyway, the far end decodes garbage and nothing
    upstream says why."""
    with pytest.raises(ValueError, match="mono and stereo only"):
        refuse_audio_rtp_cannot_carry(channels, "encoder/encoded_audio")


@pytest.mark.parametrize("channels", [1, 2])
def test_the_channel_counts_rtp_carries_are_published(channels):
    refuse_audio_rtp_cannot_carry(channels, "encoder/encoded_audio")
