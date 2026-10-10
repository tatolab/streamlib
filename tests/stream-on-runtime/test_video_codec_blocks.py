# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The four hardware video codec built-ins, built-in class to decoded frame.

The load tests need no device: `tatolabd` loads the graph and its start is
then refused at the GPU. The graph tests start the engine and need a hardware encoder and
decoder — Vulkan Video queues on Linux, VideoToolbox
on macOS — so they carry `requires_gpu` like every other graph test here and
run nowhere in CI.

No camera: the test pattern is the source, at an extent both codecs pad — so
the decoded frames arriving back at the source extent is the conformance
crop, proven from Python for both codecs, and the encoded frames carrying
the padded extent is the other half of the same fact.

The encoded-channel test is where the cast meets the engine: what
`EncodedVideoFrame` says an encoded bag is, asserted against bags the
hardware encoder actually wrote. Its GPU-free half — the wire keys, the
refusals, the payload's msgpack type — is `test_encoded_video_frame_cast.py`.
"""

from collections.abc import Callable

import pytest

from block_wiring_streams import h264_round_trip_into_a_window, h265_round_trip_into_a_window
from conftest import StreamRunWithNoVulkanDriverOutcome, TatolabdUnderTest
from tatolab.stream import (
    H264Decoder,
    H264Encoder,
    H265Decoder,
    H265Encoder,
    VideoFrame,
    compile_stream_to_graph,
)
from video_codec_blocks_probes import DECODED_STAMPS_REPORTED, ENCODED_FRAMES_REPORTED
from video_codec_blocks_streams import (
    an_h264_decoder_alone,
    an_h264_encoder_alone,
    an_h264_round_trip_with_probes,
    an_h265_decoder_alone,
    an_h265_encoder_alone,
    an_h265_round_trip_with_probes,
)

READINESS_TIMEOUT_SECONDS = 20.0

# How much of the decoded stream has to fall inside the encoded probe's own
# report for the cross-check to be about the stream rather than about one
# frame. Both stamp reports run to shutdown and the run outlives both probes'
# completion markers, so the measured overlap is the whole of the shorter
# report — this is the floor under it, not the expectation.
DECODED_STAMPS_TO_CROSS_CHECK = DECODED_STAMPS_REPORTED // 2

# The extent a 320×180 source codes at under both codecs: H.264 pads to the
# 16-sample macroblock and H.265 to the 64-sample CTU, and 192 is the first
# multiple of both above 180. 320 is already a multiple of both.
CODED_EXTENT_OF_THE_320_BY_180_PATTERN = (320, 192)

# Annex-B start codes, as the opening bytes of an access unit. Both lengths
# are legal and which one a driver emits is its own business, so the
# assertion takes either.
ANNEX_B_START_CODES = ([0, 0, 0, 1], [0, 0, 1])

FOUR_CODEC_MARKERS = [H264Encoder, H264Decoder, H265Encoder, H265Decoder]


ONE_CODEC_BLOCK_ALONE_BY_MARKER_CLASS = {
    H264Encoder: an_h264_encoder_alone,
    H264Decoder: an_h264_decoder_alone,
    H265Encoder: an_h265_encoder_alone,
    H265Decoder: an_h265_decoder_alone,
}


CODEC_ROUND_TRIPS = {
    "h264": {
        "stream": h264_round_trip_into_a_window,
        "probed_stream": an_h264_round_trip_with_probes,
        "marker_classes": (H264Encoder, H264Decoder),
        "rendered_types": {
            "H264Encoder": "tatolab.stream:H264Encoder",
            "H264Decoder": "tatolab.stream:H264Decoder",
        },
    },
    "h265": {
        "stream": h265_round_trip_into_a_window,
        "probed_stream": an_h265_round_trip_with_probes,
        "marker_classes": (H265Encoder, H265Decoder),
        "rendered_types": {
            "H265Encoder": "tatolab.stream:H265Encoder",
            "H265Decoder": "tatolab.stream:H265Decoder",
        },
    },
}


# ---- built-in class semantics (no GPU) -------------------------------------


@pytest.mark.parametrize("marker_class", FOUR_CODEC_MARKERS)
def test_node_name_defaults_to_the_type_name(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]", marker_class
):
    graph = compile_stream_to_graph(ONE_CODEC_BLOCK_ALONE_BY_MARKER_CLASS[marker_class])
    (codec_node,) = [
        node for node in graph["nodes"] if node["type"] == marker_class.type
    ]
    assert codec_node["name"] == marker_class.__name__.lower()

    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(graph)
    assert outcome.loaded and outcome.loaded_node_count == 1, outcome.tatolab_run_stderr_text


@pytest.mark.parametrize("codec", sorted(CODEC_ROUND_TRIPS))
def test_the_round_trip_wires_without_an_adapter(
    run_stream_on_tatolabd_with_no_vulkan_driver: "Callable[..., StreamRunWithNoVulkanDriverOutcome]", codec
):
    """Pattern into encoder, encoder into decoder, decoder into window — the
    port names compose as published, which is what makes four `stream_builder.add`
    calls and three `stream_builder.connect` calls the whole of a codec round trip.
    The builder checks no port name, so the proof is the engine's load."""
    outcome = run_stream_on_tatolabd_with_no_vulkan_driver(CODEC_ROUND_TRIPS[codec]["stream"])
    assert outcome.loaded and outcome.loaded_node_count == 4, outcome.tatolab_run_stderr_text


# ---- the round trip in a real graph (GPU) ----------------------------------


def start_the_codec_round_trip(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]", codec: str
) -> TatolabdUnderTest:
    """The codec's probed round trip started on `tatolabd`, once every node is Running."""
    tatolabd = start_tatolabd_running_stream(CODEC_ROUND_TRIPS[codec]["probed_stream"])
    tatolabd.local_api_client().await_every_node_running(
        stream=tatolabd.await_the_latest_attached_stream_loaded(),
        timeout=READINESS_TIMEOUT_SECONDS,
    )
    return tatolabd


def the_codec_nodes_rendered_types(live_graph: dict, codec: str) -> "dict[str, str]":
    """The `type` `graph` renders for the two codec nodes, keyed by the built-in
    class each was added as."""
    marker_classes = CODEC_ROUND_TRIPS[codec]["marker_classes"]
    compiled_graph = compile_stream_to_graph(CODEC_ROUND_TRIPS[codec]["probed_stream"])
    marker_class_name_by_node_name = {
        node["name"]: marker_class.__name__
        for node in compiled_graph["nodes"]
        for marker_class in marker_classes
        if node["type"] == marker_class.type
    }
    return {
        marker_class_name_by_node_name[node["name"]]: node["type"]
        for node in live_graph["nodes"]
        if node["name"] in marker_class_name_by_node_name
    }


@pytest.mark.requires_gpu
@pytest.mark.parametrize("codec", sorted(CODEC_ROUND_TRIPS))
def test_the_codec_round_trip_publishes_decoded_frames_at_the_source_extent(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]", codec
):
    """The whole surface, end to end: built-in class → native registration →
    hardware encode and decode in `tatolabd` → decoded bags read by a Python
    processor in its own processor interpreter.

    The pattern publishes 320×180, both codecs code it at 320×192, and the
    probe must see 320×180 back — the conformance crop. The decoded frame
    carries `color_info` with `fps` absent, so an ordinary `VideoFrame` read
    consumes it unchanged. That the stamp is the encoded frame's own rather
    than re-stamped at publication is the sibling test's, which has the
    encoded side's frame-header stamps to compare against."""
    tatolabd = start_the_codec_round_trip(start_tatolabd_running_stream, codec)
    # The import path the built-in class resolved to is what identifies the
    # node, read back off the live graph the run loaded.
    rendered_types = the_codec_nodes_rendered_types(
        tatolabd.local_api_client().call_tool(
            "graph", {"stream": tatolabd.await_the_latest_attached_stream_loaded()}
        ),
        codec,
    )
    decoded_frames = tatolabd.await_marker("DECODED_FRAMES_SEEN")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    assert isinstance(decoded_frames, list), (
        f"no parseable decoded-frame report:\n{tatolabd.recent_stderr()}"
    )
    first_bag, second_bag = decoded_frames

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

    assert rendered_types == CODEC_ROUND_TRIPS[codec]["rendered_types"]


@pytest.mark.requires_gpu
@pytest.mark.parametrize("codec", sorted(CODEC_ROUND_TRIPS))
def test_the_encoded_channel_casts_and_carries_the_ordering_contract(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]", codec
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
    tatolabd = start_the_codec_round_trip(start_tatolabd_running_stream, codec)
    tatolabd.await_marker("ENCODED_FRAMES_COMPLETE")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    encoded_frames = tatolabd.marker_payloads("ENCODED_FRAME")
    assert len(encoded_frames) == ENCODED_FRAMES_REPORTED, (
        f"the probe reported {len(encoded_frames)} frames, not "
        f"{ENCODED_FRAMES_REPORTED}; standard error:\n{tatolabd.recent_stderr()}"
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
            "the payload must be one Annex-B access unit, and this one opens "
            f"{frame['opening_bytes']}"
        )
        assert frame["byte_count"] > 0, "an access unit with no bytes decodes to nothing"
        assert frame["carries_color"], (
            "the encoder bakes the stream's color into its parameter sets, so "
            "the bag says what it baked"
        )

    for earlier, later in zip(encoded_frames, encoded_frames[1:]):
        assert later["sequence_index"] == earlier["sequence_index"] + 1, (
            "`sequence_index` is monotonic in publication order and never "
            f"resets, so the step {earlier['sequence_index']} → "
            f"{later['sequence_index']} is loss on the link"
        )
        expected_group = earlier["group_index"] + (1 if later["is_sync_point"] else 0)
        assert later["group_index"] == expected_group, (
            "`group_index` counts sync points, so it steps at one and nowhere "
            f"else: {earlier['group_index']} → {later['group_index']} across a "
            f"frame with is_sync_point={later['is_sync_point']}"
        )

    assert any(frame["is_sync_point"] for frame in encoded_frames[1:]), (
        "the window must span a group boundary, or the group-index assertion "
        "above is about nothing — the stream asks for a 1-second keyframe interval "
        "at the pattern's 30 fps for exactly this reason"
    )


@pytest.mark.requires_gpu
@pytest.mark.parametrize("codec", sorted(CODEC_ROUND_TRIPS))
def test_each_decoded_frame_carries_the_stamp_of_the_encoded_frame_it_came_from(
    start_tatolabd_running_stream: "Callable[..., TatolabdUnderTest]", codec
):
    """The decoder stamps its output with the encoded frame's own timestamp,
    never the moment of publication — proven from Python by reading both
    links of one run.

    A stamp taken at publication would still advance and still look like a
    plausible clock, which is why this compares against the encoded side's
    frame-header stamps rather than asserting monotonicity.
    """
    tatolabd = start_the_codec_round_trip(start_tatolabd_running_stream, codec)
    # Both, in whichever order they arrive: the run must not end before each
    # side has produced its own evidence, and neither probe's window bounds
    # the other's.
    tatolabd.await_every_marker("ENCODED_FRAMES_COMPLETE", "DECODED_FRAME_STAMPS_COMPLETE")
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    encoded_stamps = {
        report["timestamp_ns"] for report in tatolabd.marker_payloads("ENCODED_FRAME_STAMP")
    }
    decoded_stamps = [
        report["timestamp_ns"] for report in tatolabd.marker_payloads("DECODED_FRAME_STAMP")
    ]
    assert encoded_stamps, f"the encoded link reported no stamps:\n{tatolabd.recent_stderr()}"

    # Bounded by the encoded probe's own report: the two probes are separate
    # helper processes attaching at their own pace, and a decoded frame from
    # before the encoded probe attached rode a bag nobody wrote down.
    cross_checkable = [
        stamp
        for stamp in decoded_stamps
        if min(encoded_stamps) <= stamp <= max(encoded_stamps)
    ]
    assert len(cross_checkable) >= DECODED_STAMPS_TO_CROSS_CHECK, (
        f"only {len(cross_checkable)} decoded frames fell inside the encoded "
        "probe's report, which is too few to be about the stream; "
        f"standard error:\n{tatolabd.recent_stderr()}"
    )

    for stamp in cross_checkable:
        assert stamp in encoded_stamps, (
            f"the decoded frame is stamped {stamp}, which rode no encoded frame "
            "this run wrote down — either the decoder re-stamped it at "
            "publication, or the encoded probe's own link lost the bag it came on"
        )
