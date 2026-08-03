# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Logging from a processor, straight onto the engine's log pipeline.

`print()` also works, but while the engine is alive it is captured and re-emitted
at WARN by the stdio interceptor. These functions carry the level the author
meant and interleave in order with the engine's own records.
"""

from __future__ import annotations

from ._engine import log_event

__all__ = ["debug", "error", "info", "trace", "warning"]

_DEFAULT_TARGET = "app"


def trace(message: str, *, target: str = _DEFAULT_TARGET) -> None:
    """Emit a TRACE record."""
    log_event("trace", target, message)


def debug(message: str, *, target: str = _DEFAULT_TARGET) -> None:
    """Emit a DEBUG record."""
    log_event("debug", target, message)


def info(message: str, *, target: str = _DEFAULT_TARGET) -> None:
    """Emit an INFO record."""
    log_event("info", target, message)


def warning(message: str, *, target: str = _DEFAULT_TARGET) -> None:
    """Emit a WARN record."""
    log_event("warn", target, message)


def error(message: str, *, target: str = _DEFAULT_TARGET) -> None:
    """Emit an ERROR record."""
    log_event("error", target, message)
