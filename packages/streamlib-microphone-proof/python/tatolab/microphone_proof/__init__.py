# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A microphone captured in a Python node's own helper process, built as a third party would.

Throwaway: it exists to measure the helper-process capture path against the
built-in `MicrophoneSource`. Linux only — ALSA, opened at runtime.
"""

from .processors import AudioBlockArrivalRecorder as AudioBlockArrivalRecorder
from .processors import AudioBlockArrivalRecorderConfig as AudioBlockArrivalRecorderConfig
from .processors import MicrophoneProofSource as MicrophoneProofSource
from .processors import MicrophoneProofSourceConfig as MicrophoneProofSourceConfig

__all__ = [
    "AudioBlockArrivalRecorder",
    "AudioBlockArrivalRecorderConfig",
    "MicrophoneProofSource",
    "MicrophoneProofSourceConfig",
]
