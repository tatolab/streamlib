# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A `device_id` no capture backend on this machine can open.

A wrong device id is a wiring error and must not quietly land on whichever
camera happens to be attached. Watching from a second thread is what the
readiness wait is for — `run()` owns the calling thread until teardown.
"""

import threading

import tatolab.runtime
import tatolab.stream
from tatolab.stream import Stream, compile_stream_to_graph, stream

UNOPENABLE_DEVICE_ID = "/dev/video-not-a-real-camera"
READINESS_TIMEOUT_SECONDS = 10.0


@stream
def a_camera_naming_a_device_no_backend_can_open(stream: Stream) -> None:
    stream.add(tatolab.stream.CameraSource, config={"device_id": UNOPENABLE_DEVICE_ID})


def main() -> None:
    graph = compile_stream_to_graph(a_camera_naming_a_device_no_backend_can_open)
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)

    def watch_readiness() -> None:
        try:
            runtime.wait_until_every_processor_is_running(
                timeout=READINESS_TIMEOUT_SECONDS
            )
            print("MARKER:EVERY_PROCESSOR_RUNNING", flush=True)
        except RuntimeError as refusal:
            print(f"MARKER:NOT_EVERY_PROCESSOR_RUNNING {refusal}", flush=True)
        finally:
            runtime.shutdown()

    threading.Thread(target=watch_readiness, daemon=True).start()
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
