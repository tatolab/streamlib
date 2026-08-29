# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The audio built-in feeding a helper-placed consumer that declared a window.

Run as a real `python app.py`: the claim is that the contract crosses the
parent→child wiring envelope and the child's own stage honours it, which only a
real child can show.
"""

import sys

import streamlib
from audio_window_probes import ExactWindowProbe, RollingWindowProbe


def run_with(probe_class) -> None:
    runtime = streamlib.Runtime()
    microphone = runtime.add(streamlib.MicrophoneSource)
    probe = runtime.add(probe_class)
    runtime.connect(microphone.output("audio"), probe.input("audio_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


SCENARIOS = {
    "contiguous_windows": lambda: run_with(ExactWindowProbe),
    "rolling_windows": lambda: run_with(RollingWindowProbe),
}


def main() -> None:
    scenario = sys.argv[1] if len(sys.argv) > 1 else "contiguous_windows"
    SCENARIOS[scenario]()


if __name__ == "__main__":
    main()
