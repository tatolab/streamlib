#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A node that plays a known signal and captures it back off the same sink.

The engine carries the audio in both directions: a Python processor publishes
`AudioBlock` bags, `SpeakerSink` plays them into the sink named by
`STREAMLIB_AUDIO_SINK`, and `MicrophoneSource` captures from the device named
by `STREAMLIB_AUDIO_CAPTURE_DEVICE_ID` — by default that sink's PipeWire
monitor. `e2e_audio_loopback.sh` closes the same loop with no StreamLib at
all — so when this run fails and that one passes, the rig is sound and the
engine is not.

On macOS there is no monitor to name, so with
`STREAMLIB_COREAUDIO_PROCESS_TAP_MUTE_BEHAVIOUR` set the node makes one: a
private Core Audio process tap of its own output under a private aggregate
device whose UID is the capture device id. It has to be made here, before the
graph opens a device: the native built-ins run in this process, and a private
device is visible only to the process that created it.

What consumes the microphone writes the capture out as one waveform, because
that is the only way to measure the whole signal: `streamlib tap` collects
inside a bounded 500 ms window, and the signal runs for nearly three seconds.
The recorder is an ordinary consumer reading the microphone's port over a real
link, so the tap still sees the same channel and can judge the block-level
contract on it.
"""

import contextlib
import os

import streamlib
from captured_audio_waveform_recorder import CapturedAudioWaveformRecorder
from known_audio_signal_source import KnownAudioSignalSource
from streamlib import Stream, compile_stream_to_graph, stream


def _this_nodes_output_as_a_capture_device_when_asked(capture_device_id, sink):
    """The private process tap `MicrophoneSource` opens on macOS, or nothing."""
    mute_behaviour = os.environ.get("STREAMLIB_COREAUDIO_PROCESS_TAP_MUTE_BEHAVIOUR")
    if not mute_behaviour:
        return contextlib.nullcontext()
    import coreaudio_process_tap

    clock_device_uid = sink
    if not clock_device_uid:
        default_output = coreaudio_process_tap.default_output_device()
        if default_output is None:
            raise coreaudio_process_tap.CoreAudioFixtureError(
                "this Mac has no output device for the speaker to play into"
            )
        clock_device_uid = default_output.uid
    return coreaudio_process_tap.PrivateCaptureDeviceTappingThisProcessesOutput(
        aggregate_device_uid=capture_device_id,
        mute_behaviour=mute_behaviour,
        clock_device_uid=clock_device_uid,
    )


def _sink_and_capture_device_from_the_environment() -> tuple[str | None, str]:
    """The sink the speaker plays into, and the device the microphone captures from."""
    sink = os.environ.get("STREAMLIB_AUDIO_SINK")
    # `<sink>.monitor` is the capture endpoint PipeWire already routes for a
    # sink: what is played into it is readable there, which is the whole loop.
    capture_device_id = (
        os.environ.get("STREAMLIB_AUDIO_CAPTURE_DEVICE_ID") or f"{sink}.monitor"
    )
    return sink, capture_device_id


@stream
def known_signal_played_and_captured_back(stream: Stream) -> None:
    sink, capture_device_id = _sink_and_capture_device_from_the_environment()

    signal = stream.add(KnownAudioSignalSource)
    speaker = stream.add(
        streamlib.SpeakerSink, config={"device_id": sink} if sink else {}
    )
    stream.connect(signal.output("audio"), speaker.input("audio"))

    microphone = stream.add(
        streamlib.MicrophoneSource, config={"device_id": capture_device_id}
    )
    recorder = stream.add(CapturedAudioWaveformRecorder)
    stream.connect(microphone.output("audio"), recorder.input("audio_from_upstream"))


def main() -> None:
    sink, capture_device_id = _sink_and_capture_device_from_the_environment()

    with _this_nodes_output_as_a_capture_device_when_asked(
        capture_device_id, sink
    ) as process_tap:
        if process_tap is not None:
            print(f"MARKER:COREAUDIO_PROCESS_TAP {process_tap.evidence()}", flush=True)

        graph = compile_stream_to_graph(known_signal_played_and_captured_back)
        runtime = streamlib.Runtime()
        runtime.load(graph)

        # Loopback rather than the default every interface: this node exists to
        # be tapped from the machine it runs on, and it carries no
        # authentication.
        runtime.host_control_plane(
            bind_host="127.0.0.1",
            bind_port=int(os.environ.get("CONTROL_PORT", "9000")),
        )
        runtime.run()


if __name__ == "__main__":
    main()
