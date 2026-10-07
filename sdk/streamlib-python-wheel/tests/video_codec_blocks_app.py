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
import sys
import threading

import tatolab.runtime
import tatolab.stream
from video_codec_blocks_probes import (
    DecodedVideoFrameProbe,
    EncodedFrameProbe,
    EncodedFrameTimestampProbe,
)
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream
from tatolab.runtime._control_plane_client import call_tool
from this_processes_node_registry_entry import this_processes_local_api_socket

READINESS_TIMEOUT_SECONDS = 20.0

# One second at the pattern's 30 fps. The encoder resolves its rate from the
# frame, and the pattern's bag carries 30, so this is 30 frames per group.
KEYFRAME_INTERVAL_SECONDS = 1


CODEC_BLOCKS = {
    "h264": (tatolab.stream.H264Encoder, tatolab.stream.H264Decoder),
    "h265": (tatolab.stream.H265Encoder, tatolab.stream.H265Decoder),
}


@stream
def a_codec_round_trip_with_probes(stream_builder: StreamBuilder) -> None:
    """The encoder and decoder pair `argv[1]` names, with the three probes."""
    encoder_class, decoder_class = CODEC_BLOCKS[sys.argv[1]]
    pattern = stream_builder.add(
        tatolab.stream.TestPatternSource, config={"width": 320, "height": 180}
    )
    encoder = stream_builder.add(
        encoder_class,
        config={"keyframe_interval_seconds": KEYFRAME_INTERVAL_SECONDS},
    )
    decoder = stream_builder.add(decoder_class)
    encoded_frame_probe = stream_builder.add(EncodedFrameProbe)
    encoded_frame_timestamp_probe = stream_builder.add(EncodedFrameTimestampProbe)
    decoded_frame_probe = stream_builder.add(DecodedVideoFrameProbe)
    stream_builder.connect(pattern.output("video"), encoder.input("video"))
    stream_builder.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))
    stream_builder.connect(
        encoder.output("encoded_video"),
        encoded_frame_probe.input("encoded_video_from_upstream"),
    )
    stream_builder.connect(
        encoder.output("encoded_video"),
        encoded_frame_timestamp_probe.input("encoded_video_from_upstream"),
    )
    stream_builder.connect(
        decoder.output("video"), decoded_frame_probe.input("video_from_upstream")
    )


def _report_the_codec_nodes_rendered_types(
    marker_class_name_by_node_name: "dict[str, str]",
) -> None:
    """Print the `type` `graph` renders for the two codec nodes, keyed by the
    marker class each was added as."""
    graph = json.loads(call_tool(this_processes_local_api_socket(), "graph", {}))
    rendered_types = {
        marker_class_name_by_node_name[node["name"]]: node["type"]
        for node in graph["nodes"]
        if node["name"] in marker_class_name_by_node_name
    }
    print(f"MARKER:CODEC_NODE_TYPES {json.dumps(rendered_types)}", flush=True)


def main() -> None:
    encoder_class, decoder_class = CODEC_BLOCKS[sys.argv[1]]

    graph = compile_stream_to_graph(a_codec_round_trip_with_probes)
    runtime = tatolab.runtime.Runtime()
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
            runtime.wait_until_every_node_is_running(
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
