# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The microphone A/B's module arm: `tatolab.microphone.MicrophoneSource` into the recorder.

`tatolab run` hands a stream no argv, so each setting arrives in the runtime's
environment, which the compile inherits.
"""

import os

from audio_block_arrival_recorder import AudioBlockArrivalRecorder
from tatolab.microphone import MicrophoneSource
from tatolab.stream import StreamBuilder, stream


@stream
def module_microphone_into_the_recorder(stream_builder: StreamBuilder) -> None:
    recording_path = os.environ.get("STREAMLIB_MICROPHONE_AB_RECORDING_PATH")
    if not recording_path:
        raise SystemExit("STREAMLIB_MICROPHONE_AB_RECORDING_PATH is unset")
    microphone = stream_builder.add(
        MicrophoneSource,
        config={"device_id": os.environ.get("STREAMLIB_MICROPHONE_AB_MODULE_DEVICE_ID") or None},
        name="microphone",
    )
    recorder = stream_builder.add(
        AudioBlockArrivalRecorder, config={"recording_path": recording_path}, name="recorder"
    )
    stream_builder.connect(microphone.output("audio"), recorder.input("audio"))
