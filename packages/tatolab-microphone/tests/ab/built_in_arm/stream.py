# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The microphone A/B's built-in arm: `MicrophoneSource` into the recorder.

`tatolab run` hands a stream no argv, so each setting arrives in the runtime's
environment, which the compile inherits.
"""

import os

from tatolab.stream import MicrophoneSource, MicrophoneSourceConfig, StreamBuilder, stream

from audio_block_arrival_recorder import AudioBlockArrivalRecorder


@stream
def built_in_microphone_into_the_recorder(stream_builder: StreamBuilder) -> None:
    recording_path = os.environ.get("STREAMLIB_MICROPHONE_AB_RECORDING_PATH")
    if not recording_path:
        raise SystemExit("STREAMLIB_MICROPHONE_AB_RECORDING_PATH is unset")
    microphone_config: MicrophoneSourceConfig = {}
    device_id = os.environ.get("STREAMLIB_MICROPHONE_AB_BUILT_IN_DEVICE_ID")
    if device_id:
        microphone_config["device_id"] = device_id
    microphone = stream_builder.add(MicrophoneSource, config=microphone_config, name="microphone")
    recorder = stream_builder.add(
        AudioBlockArrivalRecorder, config={"recording_path": recording_path}, name="recorder"
    )
    stream_builder.connect(microphone.output("audio"), recorder.input("audio"))
