# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The audio built-in feeding one Python processor in its real placement.

Added with no `config` at all — the spelling the plan blesses for a block that
needs no configuration, and the one that reaches the backend's default device.
"""

import streamlib
from microphone_source_probes import AudioBlockProbe


def main() -> None:
    runtime = streamlib.Runtime()
    microphone = runtime.add(streamlib.MicrophoneSource)
    probe = runtime.add(AudioBlockProbe)
    runtime.connect(microphone.output("audio"), probe.input("audio_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
