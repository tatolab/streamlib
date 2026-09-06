# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The test pattern presented as one or two virtual cameras.

`TestPatternSource -> VirtualCameraSink`, and a second sink on the same source
when `--second-name` is given: two cameras from one graph is a second
`rt.add` and a second `rt.connect`, nothing more. The sink is forced onto the
loopback door so a machine without the permission refuses by name rather than
quietly taking another door, which is what the test wants to observe.

Readiness is reported as a marker either way: a refusal at `setup()` reaches
the test as `MARKER:NOT_EVERY_PROCESSOR_RUNNING` with the sink's own text.
"""

import argparse
import threading

import streamlib

READINESS_TIMEOUT_SECONDS = 20.0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--name", required=True, help="the first camera's name")
    parser.add_argument("--second-name", help="a second camera's name, for two from one graph")
    parser.add_argument("--width", type=int, default=640)
    parser.add_argument("--height", type=int, default=360)
    arguments = parser.parse_args()

    runtime = streamlib.Runtime()
    pattern = runtime.add(
        streamlib.TestPatternSource,
        config={"width": arguments.width, "height": arguments.height},
    )
    camera_names = [arguments.name]
    if arguments.second_name:
        camera_names.append(arguments.second_name)
    for camera_name in camera_names:
        sink = runtime.add(
            streamlib.VirtualCameraSink,
            config={"name": camera_name, "door": "v4l2loopback"},
        )
        runtime.connect(pattern.output("video"), sink.input("video"))

    def watch_readiness() -> None:
        try:
            runtime.wait_until_every_processor_is_running(
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
