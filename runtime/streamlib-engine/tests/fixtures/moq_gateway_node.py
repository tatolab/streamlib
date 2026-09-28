#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One runtime of the MoQ gateway fixtures: a source or a sink, bags or video.

The transport is the environment's: `STREAMLIB_MESH_TRANSPORT=moq` with a
relay configured carries the remote link over MoQ, unset carries it over
Zenoh. Everything else about the two runs is identical.
"""

import argparse

import streamlib

SOURCE_DISPLAY_NAME = "BenchSource"
ENCODER_DISPLAY_NAME = "BenchEncoder"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--role", choices=["source", "sink"], required=True)
    parser.add_argument("--workload", choices=["bags", "video", "smoke"], required=True)
    parser.add_argument("--runtime-name", required=True)
    parser.add_argument("--source-runtime-name", default="moq-bench-source")
    parser.add_argument("--control-plane-port", type=int, default=0)
    arguments = parser.parse_args()

    runtime = streamlib.Runtime(runtime_name=arguments.runtime_name)
    if arguments.role == "source":
        if arguments.workload in ("bags", "smoke"):
            from moq_gateway_processors import MoqBenchStampSource

            runtime.add(MoqBenchStampSource, display_name=SOURCE_DISPLAY_NAME)
        if arguments.workload in ("video", "smoke"):
            pattern = runtime.add(
                streamlib.TestPatternSource, config={"width": 1920, "height": 1080}
            )
            encoder = runtime.add(
                streamlib.H264Encoder,
                config={"keyframe_interval_seconds": 2},
                display_name=ENCODER_DISPLAY_NAME,
            )
            runtime.connect(pattern.output("video"), encoder.input("video"))
    else:
        if arguments.workload == "bags":
            from moq_gateway_processors import MoqBenchLatencySink

            sink = runtime.add(MoqBenchLatencySink, display_name="BenchSink")
            runtime.connect(
                runtime.remote_processor_output(
                    arguments.source_runtime_name, SOURCE_DISPLAY_NAME, "stamps"
                ),
                sink.input("stamps"),
            )
        else:
            from moq_gateway_processors import MoqBenchEncodedVideoSink

            sink = runtime.add(MoqBenchEncodedVideoSink, display_name="BenchVideoSink")
            runtime.connect(
                runtime.remote_processor_output(
                    arguments.source_runtime_name, ENCODER_DISPLAY_NAME, "encoded_video"
                ),
                sink.input("encoded_video"),
            )

    if arguments.control_plane_port:
        runtime.host_control_plane(bind_host="127.0.0.1", bind_port=arguments.control_plane_port)
    runtime.run()


if __name__ == "__main__":
    main()
