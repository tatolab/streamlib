# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A test pattern encoded and decoded back, with no Python in the frame path.

`TestPatternSource → <codec>Encoder → <codec>Decoder → DecodedVideoFrameProbe`,
the codec chosen by argv so one app serves both hardware video codec pairs.
The pattern's extent is 320×180, an extent both codecs pad (to 320×192: the
16-sample macroblock and the 64-sample CTU agree on it), so the decoded probe
seeing 320×180 is the conformance crop proven from Python.

Two more probes fan off the encoded link beside the decoder, which is what
makes one run prove both halves: what the encoder published, and that the
decoder carried each frame's own stamp through. The encoder asks for a
1-second keyframe interval — at the pattern's 30 fps that is a sync point
every 30 frames — so the probes' window spans a group boundary rather than
asserting about a single group.

The control plane is hosted so the run can read its own graph back and report
the `type` the running engine renders for each codec node — the import path the
marker class resolved to.
"""

import json
import os
import sys
import threading

import streamlib
from video_codec_blocks_probes import (
    DecodedVideoFrameProbe,
    EncodedFrameProbe,
    EncodedFrameTimestampProbe,
)
from streamlib import Stream, compile_stream_to_graph, stream
from streamlib._control_plane_client import call_tool
from streamlib._node_registry import live_nodes

READINESS_TIMEOUT_SECONDS = 20.0

# One second at the pattern's 30 fps. The encoder resolves its rate from the
# frame, and the pattern's bag carries 30, so this is 30 frames per group.
KEYFRAME_INTERVAL_SECONDS = 1


def _add_a_codec_round_trip_with_probes(
    stream: Stream,
    encoder_class: "type[streamlib.H264Encoder] | type[streamlib.H265Encoder]",
    decoder_class: "type[streamlib.H264Decoder] | type[streamlib.H265Decoder]",
) -> None:
    pattern = stream.add(
        streamlib.TestPatternSource, config={"width": 320, "height": 180}
    )
    encoder = stream.add(
        encoder_class,
        config={"keyframe_interval_seconds": KEYFRAME_INTERVAL_SECONDS},
    )
    decoder = stream.add(decoder_class)
    encoded_frame_probe = stream.add(EncodedFrameProbe)
    encoded_frame_timestamp_probe = stream.add(EncodedFrameTimestampProbe)
    decoded_frame_probe = stream.add(DecodedVideoFrameProbe)
    stream.connect(pattern.output("video"), encoder.input("video"))
    stream.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))
    stream.connect(
        encoder.output("encoded_video"),
        encoded_frame_probe.input("encoded_video_from_upstream"),
    )
    stream.connect(
        encoder.output("encoded_video"),
        encoded_frame_timestamp_probe.input("encoded_video_from_upstream"),
    )
    stream.connect(
        decoder.output("video"), decoded_frame_probe.input("video_from_upstream")
    )


@stream
def h264_round_trip_with_probes(stream: Stream) -> None:
    _add_a_codec_round_trip_with_probes(
        stream, streamlib.H264Encoder, streamlib.H264Decoder
    )


@stream
def h265_round_trip_with_probes(stream: Stream) -> None:
    _add_a_codec_round_trip_with_probes(
        stream, streamlib.H265Encoder, streamlib.H265Decoder
    )


CODEC_BLOCKS = {
    "h264": (
        h264_round_trip_with_probes,
        streamlib.H264Encoder,
        streamlib.H264Decoder,
    ),
    "h265": (
        h265_round_trip_with_probes,
        streamlib.H265Encoder,
        streamlib.H265Decoder,
    ),
}


def _this_processes_control_url() -> str:
    """This run's own control plane, found by pid.

    By pid rather than by "the only live node": another test's app may be up at
    the same time, and this must never read that one's graph.
    """
    for node in live_nodes():
        if node.pid == os.getpid():
            return node.control_url
    raise RuntimeError("this run published no node registry entry")


def _report_the_codec_nodes_rendered_types(
    marker_class_name_by_node_name: "dict[str, str]",
) -> None:
    """Print the `type` `graph` renders for the two codec nodes, keyed by the
    marker class each was added as."""
    graph = json.loads(call_tool(_this_processes_control_url(), "graph", {}))
    rendered_types = {
        marker_class_name_by_node_name[node["name"]]: node["type"]
        for node in graph["nodes"]
        if node["name"] in marker_class_name_by_node_name
    }
    print(f"MARKER:CODEC_NODE_TYPES {json.dumps(rendered_types)}", flush=True)


def main() -> None:
    codec = sys.argv[1]
    stream_function, encoder_class, decoder_class = CODEC_BLOCKS[codec]

    graph = compile_stream_to_graph(stream_function)
    runtime = streamlib.Runtime()
    runtime.load(graph)
    runtime.host_control_plane()
    marker_class_name_by_node_name = {
        node["name"]: marker_class.__name__
        for node in graph["nodes"]
        for marker_class in (encoder_class, decoder_class)
        if node["type"] == marker_class.type
    }

    def watch_readiness() -> None:
        try:
            runtime.wait_until_every_processor_is_running(
                timeout=READINESS_TIMEOUT_SECONDS
            )
            print("MARKER:EVERY_PROCESSOR_RUNNING", flush=True)
        except RuntimeError as refusal:
            print(f"MARKER:NOT_EVERY_PROCESSOR_RUNNING {refusal}", flush=True)
            runtime.shutdown()
            return

        # Never fatal: reading the graph can fail on its own terms, and a
        # failure here after the readiness report would read as a startup
        # failure this graph did not have.
        try:
            _report_the_codec_nodes_rendered_types(marker_class_name_by_node_name)
        except Exception as unreadable:  # noqa: BLE001 — the marker is the report
            print(f"MARKER:CODEC_NODE_TYPES_UNREADABLE {unreadable}", flush=True)

    threading.Thread(target=watch_readiness, daemon=True).start()
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
