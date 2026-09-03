#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A node that encodes the known signal to Opus and captures what decodes back.

`KnownAudioSignalSource -> OpusEncoder -> OpusDecoder ->
CapturedAudioWaveformRecorder`. No audio device is in the path at all: where
`audio_loopback_node.py` measures the transport by playing into a sink and
capturing its monitor, this measures the codec by keeping the whole loop
inside the graph. So a failure here with the loopback arm green is the codec's,
and a failure of both is the engine's or the rig's.

The source publishes 48 kHz stereo `f32`, which is exactly what the encoder's
window contract asks the stage to resample to — stated rather than discovered
so nothing between the reference and the measurement is a resampler and the
comparison stays about the codec. The stage still does the framing: the source
publishes 480-sample blocks and the encoder's port declares 960/960.

What is being scored is lossy by design, so the analysis is asked for tone
identity and the DTMF timing grid — what Opus preserves — rather than a
sample-exact match, which no codec would give.
"""

import argparse
import os


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "captured_waveform",
        help="where to write the WAV the analysis then scores",
    )
    parser.add_argument("--control-plane-port", type=int, default=9000)
    parser.add_argument(
        "--record-seconds",
        type=float,
        default=3.0,
        help=(
            "how much decoded audio to record before writing; the signal is "
            "2.78 s and the source stops at 3.78 s, so this sits between them"
        ),
    )
    arguments = parser.parse_args()

    # The recorder reads both of these, and it runs in its own helper process —
    # an argument reaches this process alone, while the environment is the seam
    # a child inherits.
    os.environ["STREAMLIB_CAPTURED_WAVEFORM"] = arguments.captured_waveform
    os.environ["STREAMLIB_CAPTURED_WAVEFORM_SECONDS"] = str(arguments.record_seconds)

    # Imported after the environment is set: the recorder resolves its window at
    # import, and this module's own import of it happens in this process too.
    import streamlib
    from captured_audio_waveform_recorder import CapturedAudioWaveformRecorder
    from known_audio_signal_source import KnownAudioSignalSource

    runtime = streamlib.Runtime()
    signal = runtime.add(KnownAudioSignalSource)
    encoder = runtime.add(streamlib.OpusEncoder)
    decoder = runtime.add(streamlib.OpusDecoder)
    recorder = runtime.add(CapturedAudioWaveformRecorder)

    runtime.connect(signal.output("audio"), encoder.input("audio"))
    runtime.connect(encoder.output("encoded_audio"), decoder.input("encoded_audio"))
    runtime.connect(decoder.output("audio"), recorder.input("audio_from_upstream"))

    # Loopback rather than the default every interface: this node exists to be
    # tapped from the machine it runs on, and it carries no authentication.
    runtime.host_control_plane(
        bind_host="127.0.0.1", bind_port=arguments.control_plane_port
    )
    runtime.run()


if __name__ == "__main__":
    main()
