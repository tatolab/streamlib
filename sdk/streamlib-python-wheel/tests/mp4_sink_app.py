# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Two independent tone streams recorded into one file, one track each.

`StereoToneSource -> OpusEncoder` twice, both encoders into the single
`tracks` input of one `Mp4Sink`. Nothing configures the second track: the
sink enumerates its inbound links at `setup()` and each one becomes a track
named by the channel it subscribed to, so the whole of "record two sources"
is a second pair of `rt.add` calls and a second `rt.connect`.

Two sources rather than one fanned out, because a fan-out is one channel with
two subscribers and would be one track. The two links have to come from two
producers for the sink to owe two tracks.

No camera and no microphone: the recording under test is the container, and a
tone the source states the format of keeps the file's Opus tracks the test's
own fact rather than the rig's.

The track names are printed before the run because the test cannot derive
them — a channel name carries the producer's engine-minted processor id, not
its display name.
"""

import argparse
import json
import threading

import streamlib
from opus_blocks_probes import StereoToneSource

READINESS_TIMEOUT_SECONDS = 20.0

RECORDED_TRACK_NAMES_MARKER = "MARKER:RECORDED_TRACK_NAMES "

# What each recorded pair is called in the graph. Two entries, because the
# file owes one track per inbound link and this is the list of them.
RECORDED_PAIR_NAMES = ("first", "second")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--path", required=True, help="the file to record into")
    arguments = parser.parse_args()

    runtime = streamlib.Runtime()
    sink = runtime.add(
        streamlib.Mp4Sink,
        config={"path": arguments.path},
        display_name="recorder",
    )

    recorded_track_names = []
    for pair_name in RECORDED_PAIR_NAMES:
        source = runtime.add(StereoToneSource, display_name=f"{pair_name}_tone")
        encoder = runtime.add(
            streamlib.OpusEncoder, display_name=f"{pair_name}_encoder"
        )
        runtime.connect(source.output("audio"), encoder.input("audio"))
        runtime.connect(encoder.output("encoded_audio"), sink.input("tracks"))
        # The name the track will carry: the channel the link subscribed to,
        # which is the producing processor's id lowercased over its output
        # port — what `graph` and `tap` show.
        recorded_track_names.append(f"{encoder.processor_id.lower()}/encoded_audio")

    print(RECORDED_TRACK_NAMES_MARKER + json.dumps(recorded_track_names), flush=True)

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
