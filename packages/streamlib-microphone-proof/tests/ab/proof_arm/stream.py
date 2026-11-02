# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The microphone A/B's proof arm: `MicrophoneProofSource` into the recorder.

`tatolab run` hands a stream no argv, so each setting arrives in the runtime's
environment, which the compile inherits.
"""

import os

from tatolab.stream import StreamBuilder, stream

from tatolab.microphone_proof import AudioBlockArrivalRecorder, MicrophoneProofSource


@stream
def proof_microphone_into_the_recorder(stream_builder: StreamBuilder) -> None:
    recording_path = os.environ.get("STREAMLIB_MICROPHONE_AB_RECORDING_PATH")
    if not recording_path:
        raise SystemExit("STREAMLIB_MICROPHONE_AB_RECORDING_PATH is unset")
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
        AudioBlockArrivalRecorder, config={"recording_path": recording_path}, name="recorder"
    )
    stream_builder.connect(microphone.output("audio"), recorder.input("audio"))
