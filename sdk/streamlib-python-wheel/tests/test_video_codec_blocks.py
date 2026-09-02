# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The four hardware video codec built-ins, marker class to decoded frame.

The marker tests are pure Python. The graph tests boot a real engine and need
a device with Vulkan Video encode and decode queues, so they carry
`requires_gpu` like every other graph test here and run nowhere in CI.

No camera: the test pattern is the source, at an extent both codecs pad — so
the decoded frames arriving back at the source extent is the conformance
crop, proven from Python for both codecs, and the encoded frames carrying
the padded extent is the other half of the same fact.

The encoded-channel test is where the cast meets the engine: what
`EncodedVideoFrame` says an encoded bag is, asserted against bags the
hardware encoder actually wrote. Its GPU-free half — the wire keys, the
refusals, the payload's msgpack type — is `test_encoded_video_frame_cast.py`.
"""

import json
import re
from pathlib import Path

import pytest

import streamlib
from streamlib import H264Decoder, H264Encoder, H265Decoder, H265Encoder, VideoFrame
from video_codec_blocks_probes import ENCODED_FRAMES_REPORTED

VIDEO_CODEC_BLOCKS_APP = Path(__file__).parent / "video_codec_blocks_app.py"

DECODED_FRAMES_SEEN = re.compile(r"MARKER:DECODED_FRAMES_SEEN (\[.*\])")
CODEC_NODE_TYPES = re.compile(r"MARKER:CODEC_NODE_TYPES (\{.*\})")
ENCODED_FRAME = re.compile(r"MARKER:ENCODED_FRAME (\{.*\})")
ENCODED_FRAME_STAMP = re.compile(r"MARKER:ENCODED_FRAME_STAMP (\{.*\})")

# The extent a 320×180 source codes at under both codecs: H.264 pads to the
# 16-sample macroblock and H.265 to the 64-sample CTU, and 192 is the first
# multiple of both above 180. 320 is already a multiple of both.
CODED_EXTENT_OF_THE_320_BY_180_PATTERN = (320, 192)

# Annex-B start codes, as the opening bytes of an access unit. Both lengths
# are legal and which one a driver emits is its own business, so the
# assertion takes either.
ANNEX_B_START_CODES = ([0, 0, 0, 1], [0, 0, 1])

FOUR_CODEC_MARKERS = [H264Encoder, H264Decoder, H265Encoder, H265Decoder]

CODEC_ROUND_TRIPS = {
    "h264": {
        "encoder": H264Encoder,
        "decoder": H264Decoder,
        "rendered_types": {
            "H264Encoder": "streamlib_media_builtins::h264_encoder::H264Encoder",
            "H264Decoder": "streamlib_media_builtins::h264_decoder::H264Decoder",
        },
    },
    "h265": {
        "encoder": H265Encoder,
        "decoder": H265Decoder,
        "rendered_types": {
            "H265Encoder": "streamlib_media_builtins::h265_encoder::H265Encoder",
            "H265Decoder": "streamlib_media_builtins::h265_decoder::H265Decoder",
        },
    },
}


# ---- marker semantics (no GPU) ---------------------------------------------


@pytest.mark.parametrize("marker_class", FOUR_CODEC_MARKERS)
def test_the_marker_class_cannot_be_instantiated(marker_class):
    with pytest.raises(TypeError):
        marker_class()


@pytest.mark.parametrize("marker_class", FOUR_CODEC_MARKERS)
def test_display_name_defaults_to_the_type_name(marker_class):
    runtime = streamlib.Runtime()
    try:
        block = runtime.add(marker_class)
        assert block.display_name == marker_class.__name__
    finally:
        runtime.shutdown()


@pytest.mark.parametrize("codec", sorted(CODEC_ROUND_TRIPS))
def test_the_round_trip_wires_without_an_adapter(codec):
    """Pattern into encoder, encoder into decoder, decoder into window — the
    port names compose as published, which is what makes four `rt.add` calls
    and three `rt.connect` calls the whole of a codec round trip."""
    blocks = CODEC_ROUND_TRIPS[codec]
    runtime = streamlib.Runtime()
    try:
        pattern = runtime.add(streamlib.TestPatternSource)
        encoder = runtime.add(blocks["encoder"])
        decoder = runtime.add(blocks["decoder"])
        window = runtime.add(streamlib.DisplayWindow)
        runtime.connect(pattern.output("video"), encoder.input("video"))
        runtime.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))
        runtime.connect(decoder.output("video"), window.input("video"))
    finally:
        runtime.shutdown()


# ---- the round trip in a real graph (GPU) ----------------------------------


@pytest.mark.requires_gpu
@pytest.mark.parametrize("codec", sorted(CODEC_ROUND_TRIPS))
def test_the_codec_round_trip_publishes_decoded_frames_at_the_source_extent(
    start_app_under_test, codec
):
    """The whole surface, end to end: marker class → native registration →
    hardware encode and decode in the app process → decoded bags read by a
    Python processor in its own helper process.

    The pattern publishes 320×180, both codecs code it at 320×192, and the
    probe must see 320×180 back — the conformance crop. The decoded frame
    carries `color_info` with `fps` absent, so an ordinary `VideoFrame` read
    consumes it unchanged. That the stamp is the encoded frame's own rather
    than re-stamped at publication is the sibling test's, which has the
    encoded side's frame-header stamps to compare against."""
    app = start_app_under_test(VIDEO_CODEC_BLOCKS_APP, codec)
    app.await_marker("EVERY_PROCESSOR_RUNNING")
    app.await_output_containing(
        "MARKER:DECODED_FRAMES_SEEN", "the probe's first two decoded frames"
    )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    match = DECODED_FRAMES_SEEN.search(app.output)
    assert match is not None, f"no parseable decoded-frame report:\n{app.output}"
    first_bag, second_bag = json.loads(match.group(1))

    frame = VideoFrame.from_bag(first_bag)
    assert (frame.width, frame.height) == (320, 180), (
        "the decoder must publish the conformance-windowed extent, never the "
        "coded picture"
    )
    assert frame.surface_id, "surface_id names the decoder's pooled frame"
    assert frame.color_info is not None, (
        "the decoded frame carries the stream's color"
    )
    assert frame.fps is None, (
        "a decoded elementary stream knows no rate, so the bag must not "
        "invent one"
    )

    later_frame = VideoFrame.from_bag(second_bag)
    assert later_frame.timestamp_ns > frame.timestamp_ns, (
        "timestamps are the ordering primitive and must advance"
    )

    # The import path the marker resolved to is what identifies the node —
    # readable only off a live graph, because a marker class exposes no
    # import path to Python.
    assert "MARKER:CODEC_NODE_TYPES_UNREADABLE" not in app.output, (
        f"the run could not read its own graph:\n{app.output}"
    )
    types_match = CODEC_NODE_TYPES.search(app.output)
    assert types_match is not None, (
        f"the app never reported the graph's rendering:\n{app.output}"
    )
    assert json.loads(types_match.group(1)) == CODEC_ROUND_TRIPS[codec]["rendered_types"]


def _reported_encoded_frames(app_output: str) -> "list[dict]":
    """Every encoded frame the probe admitted, in the order it admitted them."""
    return [json.loads(report) for report in ENCODED_FRAME.findall(app_output)]


@pytest.mark.requires_gpu
@pytest.mark.parametrize("codec", sorted(CODEC_ROUND_TRIPS))
def test_the_encoded_channel_casts_and_carries_the_ordering_contract(
    start_app_under_test, codec
):
    """The encoded-domain link, read from Python: every bag the hardware
    encoder published casts to an `EncodedVideoFrame`, and what the cast then
    reports is the wire contract the plan fixed.

    The probe enters the stream at a sync point, as every reader of an encoded
    stream must — the first bag off a link is not necessarily the producer's
    first. From there the ordering pair is the whole assertion: a
    `sequence_index` step other than exactly one is loss, and `group_index`
    moves only where a decoder could have entered.
    """
    app = start_app_under_test(VIDEO_CODEC_BLOCKS_APP, codec)
    app.await_marker("EVERY_PROCESSOR_RUNNING")
    # Decoded first: it needs two frames where the encoded probe needs forty,
    # and each wait scans forward only.
    app.await_output_containing(
        "MARKER:DECODED_FRAMES_SEEN", "the probe's first two decoded frames"
    )
    app.await_output_containing(
        "MARKER:ENCODED_FRAMES_COMPLETE", "the encoded probe's full window"
    )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    encoded_frames = _reported_encoded_frames(app.output)
    assert len(encoded_frames) == ENCODED_FRAMES_REPORTED, (
        f"the probe reported {len(encoded_frames)} frames, not "
        f"{ENCODED_FRAMES_REPORTED}; output:\n{app.output}"
    )

    assert encoded_frames[0]["is_sync_point"], (
        "a reader enters an encoded stream only at a sync point, so the first "
        "frame it admits is one by construction"
    )
    for frame in encoded_frames:
        assert frame["codec"] == codec, (
            "the bag names the elementary stream its bitstream actually is"
        )
        assert (frame["width"], frame["height"]) == (
            CODED_EXTENT_OF_THE_320_BY_180_PATTERN
        ), "an encoded bag carries the coded extent, before the conformance crop"
        assert any(
            frame["opening_bytes"][: len(start_code)] == start_code
            for start_code in ANNEX_B_START_CODES
        ), (
            f"the payload must be one Annex-B access unit, and this one opens "
            f"{frame['opening_bytes']}"
        )
        assert frame["byte_count"] > 0, "an access unit with no bytes decodes to nothing"
        assert frame["carries_color"], (
            "the encoder bakes the stream's color into its parameter sets, so "
            "the bag says what it baked"
        )

    for earlier, later in zip(encoded_frames, encoded_frames[1:]):
        assert later["sequence_index"] == earlier["sequence_index"] + 1, (
            f"`sequence_index` is monotonic in publication order and never "
            f"resets, so the step {earlier['sequence_index']} → "
            f"{later['sequence_index']} is loss on the link"
        )
        expected_group = earlier["group_index"] + (1 if later["is_sync_point"] else 0)
        assert later["group_index"] == expected_group, (
            f"`group_index` counts sync points, so it steps at one and nowhere "
            f"else: {earlier['group_index']} → {later['group_index']} across a "
            f"frame with is_sync_point={later['is_sync_point']}"
        )

    assert any(frame["is_sync_point"] for frame in encoded_frames[1:]), (
        "the window must span a group boundary, or the group-index assertion "
        "above is about nothing — the app asks for a 1-second keyframe interval "
        "at the pattern's 30 fps for exactly this reason"
    )


@pytest.mark.requires_gpu
@pytest.mark.parametrize("codec", sorted(CODEC_ROUND_TRIPS))
def test_each_decoded_frame_carries_the_stamp_of_the_encoded_frame_it_came_from(
    start_app_under_test, codec
):
    """The decoder stamps its output with the encoded frame's own timestamp,
    never the moment of publication — proven from Python by reading both
    links of one run.

    A stamp taken at publication would still advance and still look like a
    plausible clock, which is why this compares against the encoded side's
    frame-header stamps rather than asserting monotonicity.
    """
    app = start_app_under_test(VIDEO_CODEC_BLOCKS_APP, codec)
    app.await_marker("EVERY_PROCESSOR_RUNNING")
    app.await_output_containing(
        "MARKER:DECODED_FRAMES_SEEN", "the probe's first two decoded frames"
    )
    app.await_output_containing(
        "MARKER:ENCODED_FRAMES_COMPLETE", "the encoded probe's full window"
    )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    encoded_stamps = {
        json.loads(report)["timestamp_ns"]
        for report in ENCODED_FRAME_STAMP.findall(app.output)
    }
    assert encoded_stamps, f"the encoded link reported no stamps:\n{app.output}"

    match = DECODED_FRAMES_SEEN.search(app.output)
    assert match is not None, f"no parseable decoded-frame report:\n{app.output}"
    decoded_frames = [VideoFrame.from_bag(bag) for bag in json.loads(match.group(1))]

    for frame in decoded_frames:
        assert frame.timestamp_ns in encoded_stamps, (
            f"the decoded frame is stamped {frame.timestamp_ns}, which rode no "
            f"encoded frame — the decoder re-stamped it at publication"
        )
