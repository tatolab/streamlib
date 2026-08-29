# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A microphone wired straight to a speaker, with no Python in the sample path.

Added with no `config` on either end, so this is also the added-without-config
proof for the playback built-in: the config travels to the engine as JSON and
every field of a built-in's config struct carries a serde default, so `{}`
deserializes and `null` does not.

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
import os
import threading

import streamlib
from speaker_sink_probes import AudioBlockCountingProbe
from streamlib._control_plane_client import call_tool
from streamlib._node_registry import live_nodes

READINESS_TIMEOUT_SECONDS = 20.0


def _this_processes_control_url() -> str:
    """This run's own control plane, found by pid.

    By pid rather than by "the only live node": another test's app may be up at
    the same time, and this must never read that one's graph.
    """
    for node in live_nodes():
        if node.pid == os.getpid():
            return node.control_url
    raise RuntimeError("this run published no node registry entry")


def _report_the_speakers_settled_window_contract() -> None:
    """Print what `graph` renders for the speaker's `audio` port."""
    graph = json.loads(call_tool(_this_processes_control_url(), "graph", {}))
    for node in graph["nodes"]:
        # By the display name the marker class defaults to, because a marker
        # class exposes no import path to Python — and this app adds exactly one
        # speaker, so the default is unambiguous.
        if node["display_name"] != "SpeakerSink":
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


def main() -> None:
    runtime = streamlib.Runtime()
    runtime.host_control_plane()
    microphone = runtime.add(streamlib.MicrophoneSource)
    speaker = runtime.add(streamlib.SpeakerSink)
    runtime.connect(microphone.output("audio"), speaker.input("audio"))

    probe = runtime.add(AudioBlockCountingProbe)
    runtime.connect(microphone.output("audio"), probe.input("audio_from_upstream"))

    def watch_readiness() -> None:
        try:
            runtime.wait_until_every_processor_is_running(
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
            _report_the_speakers_settled_window_contract()
        except Exception as unreadable:  # noqa: BLE001 — the marker is the report
            print(f"MARKER:SPEAKER_AUDIO_WINDOW_UNREADABLE {unreadable}", flush=True)

    threading.Thread(target=watch_readiness, daemon=True).start()
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
