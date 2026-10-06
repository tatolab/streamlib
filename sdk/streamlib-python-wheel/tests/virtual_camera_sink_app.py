# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The test pattern presented as one or two virtual cameras.

`TestPatternSource -> VirtualCameraSink`, and a second sink on the same source
when `--second-name` is given: two cameras from one graph is a second
`stream.add` and a second `stream.connect`, nothing more. `--door` is passed
straight through, and defaults to `v4l2loopback` so a machine without the
permission refuses by name rather than quietly taking the other door — which
is what the loopback tests want to observe. The PipeWire test names its door
instead.

Readiness is reported as a marker either way: a refusal at `setup()` reaches
the test as `MARKER:NOT_EVERY_PROCESSOR_RUNNING` with the sink's own text.
"""

import argparse
import threading

import tatolab.runtime
import tatolab.stream
from tatolab.stream import Stream, compile_stream_to_graph, stream

READINESS_TIMEOUT_SECONDS = 20.0


def _parse_virtual_camera_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--name", required=True, help="the first camera's name")
    parser.add_argument("--second-name", help="a second camera's name, for two from one graph")
    parser.add_argument(
        "--door",
        default="v4l2loopback",
        choices=("auto", "v4l2loopback", "pipewire"),
        help="which door the sinks take",
    )
    parser.add_argument("--width", type=int, default=640)
    parser.add_argument("--height", type=int, default=360)
    return parser.parse_args()


@stream
def a_test_pattern_into_virtual_cameras(stream: Stream) -> None:
    arguments = _parse_virtual_camera_arguments()
    pattern = stream.add(
        tatolab.stream.TestPatternSource,
        config={"width": arguments.width, "height": arguments.height},
    )
    camera_names = [arguments.name]
    if arguments.second_name:
        camera_names.append(arguments.second_name)
    for camera_name in camera_names:
        sink = stream.add(
            tatolab.stream.VirtualCameraSink,
            config={"name": camera_name, "door": arguments.door},
        )
        stream.connect(pattern.output("video"), sink.input("video"))


def main() -> None:
    graph = compile_stream_to_graph(a_test_pattern_into_virtual_cameras)
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)

    def watch_readiness() -> None:
        try:
            runtime.wait_until_every_node_is_running(
                timeout=READINESS_TIMEOUT_SECONDS
            )
            print("MARKER:EVERY_PROCESSOR_RUNNING", flush=True)
        except RuntimeError as refusal:
            print(f"MARKER:NOT_EVERY_PROCESSOR_RUNNING {refusal}", flush=True)
            runtime.shutdown()

    threading.Thread(target=watch_readiness, daemon=True).start()
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
