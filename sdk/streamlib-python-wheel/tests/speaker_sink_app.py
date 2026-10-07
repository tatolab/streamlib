# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A microphone wired straight to a speaker, with no Python in the sample path.

`stream_builder.add` with no `config` on either end records `{}`, so this is also the
added-without-config proof for the playback built-in: every field of a
built-in's config struct carries a serde default, so `{}` deserializes.

The two ends need not agree on rate, channels or dtype, and on a stock machine
they do not: the ALSA arm asks a capture device for mono and a playback device
for stereo. `SpeakerSink`'s input port declares `audio_window = match_device`,
so the engine converts every block into whatever format the speaker's own
device opened at — which is the thing under test here.

A probe hangs off the same output the speaker reads, so the test has a marker
saying enough blocks have really flowed rather than a sleep guessing that they
have. It is a second consumer of the microphone's port, not a stage between the
two built-ins — the samples the speaker plays never enter an interpreter.

The control plane is hosted so the run can be asked what the sentinel settled
to. `graph` renders the resolved five values on the speaker's own port, and on
a real device those values are this machine's — which is the point of resolving
them from the device rather than writing them down.
"""

import json
import threading

import tatolab.runtime
import tatolab.stream
from speaker_sink_probes import AudioBlockCountingProbe
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream
from tatolab.runtime._control_plane_client import call_tool
from this_processes_node_registry_entry import this_processes_local_api_socket

READINESS_TIMEOUT_SECONDS = 20.0
SPEAKER_NODE_NAME = "speakersink"


def _report_the_speakers_settled_window_contract(speaker_node_name: str) -> None:
    """Print what `graph` renders for the speaker's `audio` port."""
    graph = json.loads(call_tool(this_processes_local_api_socket(), "graph", {}))
    for node in graph["nodes"]:
        if node["name"] != speaker_node_name:
            continue
        audio = next(
            (port for port in node["ports"]["inputs"] if port["name"] == "audio"),
            None,
        )
        if audio is None:
            raise RuntimeError(f"the speaker node renders no `audio` input port: {node}")
        print(
            f"MARKER:SPEAKER_AUDIO_WINDOW {json.dumps(audio.get('audio_window'))}",
            flush=True,
        )
        return
    print("MARKER:SPEAKER_AUDIO_WINDOW null", flush=True)


@stream
def microphone_into_a_speaker_and_a_block_counting_probe(stream_builder: StreamBuilder) -> None:
    microphone = stream_builder.add(tatolab.stream.MicrophoneSource)
    speaker = stream_builder.add(tatolab.stream.SpeakerSink, name=SPEAKER_NODE_NAME)
    stream_builder.connect(microphone.output("audio"), speaker.input("audio"))

    probe = stream_builder.add(AudioBlockCountingProbe)
    stream_builder.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


def main() -> None:
    graph = compile_stream_to_graph(
        microphone_into_a_speaker_and_a_block_counting_probe
    )
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)
    runtime.host_control_plane()

    def watch_readiness() -> None:
        try:
            runtime.wait_until_every_node_is_running(
                timeout=READINESS_TIMEOUT_SECONDS
            )
            print("MARKER:EVERY_PROCESSOR_RUNNING", flush=True)
        except RuntimeError as refusal:
            print(f"MARKER:NOT_EVERY_PROCESSOR_RUNNING {refusal}", flush=True)
            # Shut down rather than leave `run()` holding the main thread: a
            # processor that failed setup never reaches Running, so waiting for
            # it is waiting for nothing.
            runtime.shutdown()
            return

        # Reported outside the readiness `try`, and never fatal. Reading the
        # graph can fail on its own terms — no registry entry, a control plane
        # that does not answer, a speaker with no `audio` port — and inside the
        # readiness handler every one of those would print
        # `NOT_EVERY_PROCESSOR_RUNNING` after the run had already reported
        # itself healthy, which reads as a startup failure this graph did not
        # have.
        try:
            _report_the_speakers_settled_window_contract(SPEAKER_NODE_NAME)
        except Exception as unreadable:  # noqa: BLE001 — the marker is the report
            print(f"MARKER:SPEAKER_AUDIO_WINDOW_UNREADABLE {unreadable}", flush=True)

    threading.Thread(target=watch_readiness, daemon=True).start()
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
