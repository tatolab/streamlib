# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One native built-in feeding one Python processor in its real placement."""

import streamlib
from native_builtin_probes import VideoFrameProbe


def main() -> None:
    runtime = streamlib.Runtime()
    pattern = runtime.add(
        streamlib.TestPatternSource, config={"width": 320, "height": 180}
    )
    probe = runtime.add(VideoFrameProbe)
    runtime.connect(pattern.output("video"), probe.input("video_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
