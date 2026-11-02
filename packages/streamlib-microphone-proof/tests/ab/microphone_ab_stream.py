# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The microphone A/B's two streams: one source each, into the same recorder.

`microphone_ab.sh` loads one per arm with `tatolab run`, which hands a stream no
argv, so each setting arrives in the runtime's environment, which the compile
inherits.
"""

import os

from tatolab.stream import MicrophoneSource, MicrophoneSourceConfig, StreamBuilder, stream

from tatolab.microphone_proof import AudioBlockArrivalRecorder, MicrophoneProofSource


def _recorder_config() -> dict[str, object]:
    recording_path = os.environ.get("STREAMLIB_MICROPHONE_AB_RECORDING_PATH")
    if not recording_path:
        raise SystemExit("STREAMLIB_MICROPHONE_AB_RECORDING_PATH is unset")
    return {"recording_path": recording_path}


@stream
def built_in_microphone_into_the_recorder(stream_builder: StreamBuilder) -> None:
    microphone_config: MicrophoneSourceConfig = {}
    device_id = os.environ.get("STREAMLIB_MICROPHONE_AB_BUILT_IN_DEVICE_ID")
    if device_id:
        microphone_config["device_id"] = device_id
    microphone = stream_builder.add(MicrophoneSource, config=microphone_config, name="microphone")
    recorder = stream_builder.add(
        AudioBlockArrivalRecorder, config=_recorder_config(), name="recorder"
    )
    stream_builder.connect(microphone.output("audio"), recorder.input("audio"))


@stream
def proof_microphone_into_the_recorder(stream_builder: StreamBuilder) -> None:
    microphone = stream_builder.add(
        MicrophoneProofSource,
        config={
            "pcm_name": os.environ.get("STREAMLIB_MICROPHONE_AB_PROOF_PCM_NAME") or "default",
            "gil_churn_threads": int(
                os.environ.get("STREAMLIB_MICROPHONE_AB_PROOF_GIL_CHURN_THREADS") or "0"
            ),
        },
        name="microphone",
    )
    recorder = stream_builder.add(
        AudioBlockArrivalRecorder, config=_recorder_config(), name="recorder"
    )
    stream_builder.connect(microphone.output("audio"), recorder.input("audio"))
