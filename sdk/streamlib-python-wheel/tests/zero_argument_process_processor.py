# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor `zero_argument_process_app.py` adds.

Its own module rather than the entry file because a processor class identifies
by its import path, and a class in the entry file identifies as `__main__:…` —
a name the child interpreter that hosts it cannot import.
"""

from streamlib import processor


@processor(execution="continuous", interval_ms=1)
class ZeroArgumentProcess:
    def process(self) -> None:  # deliberately missing the ctx parameter
        print("MARKER:HOOK_BODY_RAN", flush=True)
