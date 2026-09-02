# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The four hardware video codec built-ins, marker class to decoded frame.

The marker tests are pure Python. The graph tests boot a real engine and need
a device with Vulkan Video encode and decode queues, so they carry
`requires_gpu` like every other graph test here and run nowhere in CI.

No camera: the test pattern is the source, at an extent both codecs pad — so
the decoded frames arriving back at the source extent is the conformance
crop, proven from Python for both codecs.
"""

import json
import re
from pathlib import Path

import pytest

import streamlib
from streamlib import H264Decoder, H264Encoder, H265Decoder, H265Encoder, VideoFrame

VIDEO_CODEC_BLOCKS_APP = Path(__file__).parent / "video_codec_blocks_app.py"

DECODED_FRAMES_SEEN = re.compile(r"MARKER:DECODED_FRAMES_SEEN (\[.*\])")
CODEC_NODE_TYPES = re.compile(r"MARKER:CODEC_NODE_TYPES (\{.*\})")

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
    consumes it unchanged. What is *not* asserted here is that the stamp is
    the encoded frame's own rather than re-stamped at publication; that
    ride-through is proven at the engine layer, where the
    `h264_decoder_completes_the_round_trip` / `h265_…` harness compares
    decoded frame-header stamps against the encoded set."""
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
