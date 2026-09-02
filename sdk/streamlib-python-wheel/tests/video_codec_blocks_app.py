# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A test pattern encoded and decoded back, with no Python in the frame path.

`TestPatternSource → <codec>Encoder → <codec>Decoder → DecodedVideoFrameProbe`,
the codec chosen by argv so one app serves both hardware video codec pairs.
Every block is added without config except the pattern's extent — 320×180, an
extent both codecs pad (to 320×192: the 16-sample macroblock and the 64-sample
CTU agree on it), so the probe seeing 320×180 is the conformance crop proven
from Python.

The control plane is hosted so the run can read its own graph and report what
`type` each codec node renders — the import path the marker class resolved to,
which no pure-Python test can observe.
"""

import json
import os
import sys
import threading

import streamlib
from video_codec_blocks_probes import DecodedVideoFrameProbe
from streamlib._control_plane_client import call_tool
from streamlib._node_registry import live_nodes

READINESS_TIMEOUT_SECONDS = 20.0

CODEC_BLOCKS = {
    "h264": (streamlib.H264Encoder, streamlib.H264Decoder),
    "h265": (streamlib.H265Encoder, streamlib.H265Decoder),
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
    encoder_display_name: str, decoder_display_name: str
) -> None:
    """Print the `type` `graph` renders for the two codec nodes."""
    graph = json.loads(call_tool(_this_processes_control_url(), "graph", {}))
    rendered_types = {
        node["display_name"]: node["type"]
        for node in graph["nodes"]
        if node["display_name"] in (encoder_display_name, decoder_display_name)
    }
    print(f"MARKER:CODEC_NODE_TYPES {json.dumps(rendered_types)}", flush=True)


def main() -> None:
    codec = sys.argv[1]
    encoder_class, decoder_class = CODEC_BLOCKS[codec]

    runtime = streamlib.Runtime()
    runtime.host_control_plane()
    pattern = runtime.add(
        streamlib.TestPatternSource, config={"width": 320, "height": 180}
    )
    encoder = runtime.add(encoder_class)
    decoder = runtime.add(decoder_class)
    probe = runtime.add(DecodedVideoFrameProbe)
    runtime.connect(pattern.output("video"), encoder.input("video"))
    runtime.connect(encoder.output("encoded_video"), decoder.input("encoded_video"))
    runtime.connect(decoder.output("video"), probe.input("video_from_upstream"))

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
            _report_the_codec_nodes_rendered_types(
                encoder_class.__name__, decoder_class.__name__
            )
        except Exception as unreadable:  # noqa: BLE001 — the marker is the report
            print(f"MARKER:CODEC_NODE_TYPES_UNREADABLE {unreadable}", flush=True)

    threading.Thread(target=watch_readiness, daemon=True).start()
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
