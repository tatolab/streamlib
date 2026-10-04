# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One cast-claim probe against a real source, in its real placement.

Run as a real `python app.py`: the probe executes in its own helper process and
reports what it saw over the child→parent log forwarding.

The source is the second argument. The camera is what the lifetime probes need
— only a real capture pool recycles a slot underneath a held frame. The native
test pattern serves the probes that only need a real surface published by a
real producer, so they run on any GPU rather than only on a rig with a camera.
"""

import sys

import streamlib
from streamlib import NodeReference, Stream, compile_stream_to_graph, stream

import cast_claim_probes
from camera_under_test import camera_source_config


def _add_source(stream: Stream, source_name: str) -> NodeReference:
    if source_name == "camera":
        return stream.add(streamlib.CameraSource, config=camera_source_config())
    if source_name == "test_pattern":
        return stream.add(
            streamlib.TestPatternSource, config={"width": 640, "height": 480}
        )
    raise SystemExit(f"unknown source {source_name!r}: use 'camera' or 'test_pattern'")


def _probe_class_named_on_the_command_line() -> type:
    return getattr(cast_claim_probes, sys.argv[1])


def _source_named_on_the_command_line() -> str:
    return sys.argv[2]


@stream
def one_cast_claim_probe_off_a_real_source(stream: Stream) -> None:
    """The probe class named first on the command line, fed by the source named second."""
    source = _add_source(stream, _source_named_on_the_command_line())
    probe = stream.add(_probe_class_named_on_the_command_line())
    stream.connect(source.output("video"), probe.input("video_from_upstream"))


if __name__ == "__main__":
    graph = compile_stream_to_graph(one_cast_claim_probe_off_a_real_source)
    runtime = streamlib.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
