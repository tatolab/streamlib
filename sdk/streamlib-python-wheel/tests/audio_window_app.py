# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The audio built-in feeding a helper-placed consumer that declared a window.

Run as a real `python app.py`: the claim is that the contract crosses the
parent→child wiring envelope and the child's own stage honours it, which only a
real child can show.
"""

import sys

import streamlib
from audio_window_probes import (
    DeclaredMonoWindowProbe,
    ExactWindowProbe,
    RollingWindowProbe,
    SourceFollowingWindowProbe,
    StereoToneSource,
)


def run_with(probe_class) -> None:
    runtime = streamlib.Runtime()
    microphone = runtime.add(streamlib.MicrophoneSource)
    probe = runtime.add(probe_class)
    runtime.connect(microphone.output("audio"), probe.input("audio_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def run_both_probes_off_one_stereo_source() -> None:
    """One stated-format source into two consumers: one that declares no
    channel count and one that declares mono.

    A Python source rather than the microphone, because what is under test is
    that the count follows *the source* — which needs a source whose count the
    test knows.
    """
    runtime = streamlib.Runtime()
    source = runtime.add(StereoToneSource)
    following = runtime.add(SourceFollowingWindowProbe)
    declared_mono = runtime.add(DeclaredMonoWindowProbe)
    runtime.connect(source.output("audio"), following.input("audio_from_upstream"))
    runtime.connect(source.output("audio"), declared_mono.input("audio_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


SCENARIOS = {
    "contiguous_windows": lambda: run_with(ExactWindowProbe),
    "rolling_windows": lambda: run_with(RollingWindowProbe),
    "source_following_windows": run_both_probes_off_one_stereo_source,
}


def main() -> None:
    scenario = sys.argv[1] if len(sys.argv) > 1 else "contiguous_windows"
    SCENARIOS[scenario]()


if __name__ == "__main__":
    main()
