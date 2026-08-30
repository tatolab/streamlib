# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: microphone → reverb → speaker.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest
and no `main()`. Nothing here names a sample rate, a channel count or a block
size, and that is the point: the capture device opens at whatever it opens at,
the playback device likewise, and the two conversions between them are the
engine's. `ReverbEffect` declares the one format it wants and `SpeakerSink`
matches its own device.

Processors live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from processors.reverb_effect import ReverbEffect

from streamlib import MicrophoneSource, Runtime, SpeakerSink


def setup(rt: Runtime) -> None:
    # `device_id` unset on both: the backend's default capture and playback
    # devices. Naming one that cannot be opened raises at `setup()` rather than
    # silently landing on another device.
    microphone = rt.add(MicrophoneSource)
    reverb = rt.add(ReverbEffect)
    speaker = rt.add(SpeakerSink)

    rt.connect(microphone.output("audio"), reverb.input("dry_audio_from_upstream"))
    rt.connect(reverb.output("reverberated_audio_to_downstream"), speaker.input("audio"))
