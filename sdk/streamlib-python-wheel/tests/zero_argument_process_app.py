# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""An app whose only processor defines `process` without the ctx parameter.

Run as a real `python app.py` because the failure under test is a log line:
the engine's tracing writer binds this process's stdout at first boot, so only
a parent reading the pipe observes it reliably — capfd inside the test process
sees nothing once another test booted an engine first.
"""

import streamlib
from zero_argument_process_processor import ZeroArgumentProcess

if __name__ == "__main__":
    runtime = streamlib.Runtime()
    runtime.add(ZeroArgumentProcess)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)
