# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Two independent tone streams recorded into one file, one track each.

`StereoToneSource -> OpusEncoder` twice, both encoders into the single
`tracks` input of one `Mp4Sink`. Nothing configures the second track: the
sink enumerates its inbound links at `setup()` and each one becomes a track
named by the channel it subscribed to, so the whole of "record two sources"
is a second pair of `stream_builder.add` calls and a second `stream_builder.connect`.

Two sources rather than one fanned out, because a fan-out is one channel with
two subscribers and would be one track. The two links have to come from two
producers for the sink to owe two tracks.

No camera and no microphone: the recording under test is the container, and a
tone the source states the format of keeps the file's Opus tracks the test's
own fact rather than the rig's.

The track names are read off the live graph and printed before the readiness
marker, because the test cannot derive them — a channel name carries the
producer's engine-minted processor id, not its node name.
"""

import argparse
import json
import threading

import tatolab.runtime
import tatolab.stream
from opus_blocks_probes import StereoToneSource
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream
from tatolab.runtime._control_plane_client import call_tool
from this_processes_node_registry_entry import this_processes_local_api_socket

READINESS_TIMEOUT_SECONDS = 20.0

RECORDED_TRACK_NAMES_MARKER = "MARKER:RECORDED_TRACK_NAMES "

# What each recorded pair is called in the graph. Two entries, because the
# file owes one track per inbound link and this is the list of them.
RECORDED_PAIR_NAMES = ("first", "second")


def _parse_mp4_sink_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--path", required=True, help="the file to record into")
    return parser.parse_args()


@stream
def two_tone_pairs_recorded_into_one_mp4(stream_builder: StreamBuilder) -> None:
    sink = stream_builder.add(
        tatolab.stream.Mp4Sink,
        name="recorder",
        config={"path": _parse_mp4_sink_arguments().path},
    )
    for pair_name in RECORDED_PAIR_NAMES:
        source = stream_builder.add(StereoToneSource, name=f"{pair_name}_tone")
        encoder = stream_builder.add(tatolab.stream.OpusEncoder, name=f"{pair_name}_encoder")
        stream_builder.connect(source.output("audio"), encoder.input("audio"))
        stream_builder.connect(encoder.output("encoded_audio"), sink.input("tracks"))


def _recorded_track_names() -> "list[str]":
    """The name each pair's track will carry, in `RECORDED_PAIR_NAMES` order.

    A track is named by the channel its link subscribed to: the producing
    processor's id lowercased over its output port — what `graph` and `tap` show.
    """
    graph = json.loads(call_tool(this_processes_local_api_socket(), "graph", {}))
    processor_id_by_node_name = {node["name"]: node["id"] for node in graph["nodes"]}
    return [
        f"{processor_id_by_node_name[f'{pair_name}_encoder'].lower()}/encoded_audio"
        for pair_name in RECORDED_PAIR_NAMES
    ]


def main() -> None:
    graph = compile_stream_to_graph(two_tone_pairs_recorded_into_one_mp4)
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)
    runtime.host_control_plane()

    def watch_readiness() -> None:
        try:
            runtime.wait_until_every_node_is_running(
                timeout=READINESS_TIMEOUT_SECONDS
            )
        except RuntimeError as refusal:
            print(f"MARKER:NOT_EVERY_PROCESSOR_RUNNING {refusal}", flush=True)
            runtime.shutdown()
            return

        # Shut down rather than report readiness: the test reads the names as
        # soon as it sees the readiness marker, so a run that cannot name its
        # tracks has nothing to be judged by.
        try:
            recorded_track_names = _recorded_track_names()
        except Exception as unreadable:  # noqa: BLE001 — the marker is the report
            print(f"MARKER:RECORDED_TRACK_NAMES_UNREADABLE {unreadable}", flush=True)
            runtime.shutdown()
            return
        print(RECORDED_TRACK_NAMES_MARKER + json.dumps(recorded_track_names), flush=True)
        print("MARKER:EVERY_PROCESSOR_RUNNING", flush=True)

    threading.Thread(target=watch_readiness, daemon=True).start()
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
