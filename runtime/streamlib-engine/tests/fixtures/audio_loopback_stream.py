# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stream that plays a known signal and captures it back off the same sink.

The engine carries the audio in both directions: a Python node publishes
`AudioBlock` bags, `SpeakerSink` plays them into the sink named by
`STREAMLIB_AUDIO_SINK`, and `MicrophoneSource` captures from the device named
by `STREAMLIB_AUDIO_CAPTURE_DEVICE_ID` — by default that sink's PipeWire
monitor. `e2e_audio_loopback.sh` closes the same loop with no StreamLib at
all — so when this run fails and that one passes, the rig is sound and the
engine is not.

What consumes the microphone writes the capture out as one waveform, because
that is the only way to measure the whole signal: `tatolab tap` collects
inside a bounded 500 ms window, and the signal runs for nearly three seconds.
The recorder is an ordinary consumer reading the microphone's port over a real
link, so the tap still sees the same channel and can judge the block-level
contract on it.
"""

import os

import tatolab.stream
from captured_audio_waveform_recorder import CapturedAudioWaveformRecorder
from known_audio_signal_source import KnownAudioSignalSource
from tatolab.stream import StreamBuilder, stream


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
def known_signal_played_and_captured_back(stream_builder: StreamBuilder) -> None:
    sink, capture_device_id = _sink_and_capture_device_from_the_environment()

    signal = stream_builder.add(KnownAudioSignalSource)
    speaker = stream_builder.add(
        tatolab.stream.SpeakerSink, config={"device_id": sink} if sink else {}
    )
    stream_builder.connect(signal.output("audio"), speaker.input("audio"))

    microphone = stream_builder.add(
        tatolab.stream.MicrophoneSource, config={"device_id": capture_device_id}
    )
    recorder = stream_builder.add(CapturedAudioWaveformRecorder)
    stream_builder.connect(microphone.output("audio"), recorder.input("audio_from_upstream"))
