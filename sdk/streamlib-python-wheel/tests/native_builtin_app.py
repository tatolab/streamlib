# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One native built-in feeding one Python processor in its real placement."""

import tatolab.runtime
import tatolab.stream
from native_builtin_probes import VideoFrameProbe
from tatolab.stream import Stream, compile_stream_to_graph, stream


@stream
def a_test_pattern_into_a_video_frame_probe(stream: Stream) -> None:
    pattern = stream.add(
        tatolab.stream.TestPatternSource, config={"width": 320, "height": 180}
    )
    probe = stream.add(VideoFrameProbe)
    stream.connect(pattern.output("video"), probe.input("video_from_upstream"))


def main() -> None:
    graph = compile_stream_to_graph(a_test_pattern_into_a_video_frame_probe)
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
