# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""An app whose only processor defines `process` without the ctx parameter.

Run as a real `python app.py` because the failure under test is a log line:
the engine's tracing writer binds this process's stdout at first boot, so only
a parent reading the pipe observes it reliably — capfd inside the test process
sees nothing once another test booted an engine first.
"""

import streamlib
from streamlib import processor


@processor(execution="continuous", interval_ms=1)
class ZeroArgumentProcess:
    def process(self) -> None:  # deliberately missing the ctx parameter
        print("MARKER:HOOK_BODY_RAN", flush=True)


if __name__ == "__main__":
    runtime = streamlib.Runtime()
    runtime.add(ZeroArgumentProcess)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
