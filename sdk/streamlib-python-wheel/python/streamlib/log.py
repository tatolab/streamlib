# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Logging from a processor, straight onto the engine's log pipeline.

Writing to stdout also works, but while the engine is alive the stdio
interceptor captures it and re-emits it at WARN. These functions carry the level
the author meant and interleave in order with the engine's own records.
"""

from __future__ import annotations

from ._engine import log_event

__all__ = ["debug", "error", "info", "trace", "warning"]

_DEFAULT_EMITTER = "app"


def trace(message: str, *, emitted_by: str = _DEFAULT_EMITTER) -> None:
    """Emit a TRACE record."""
    log_event("trace", emitted_by, message)


def debug(message: str, *, emitted_by: str = _DEFAULT_EMITTER) -> None:
    """Emit a DEBUG record."""
    log_event("debug", emitted_by, message)


def info(message: str, *, emitted_by: str = _DEFAULT_EMITTER) -> None:
    """Emit an INFO record."""
    log_event("info", emitted_by, message)


def warning(message: str, *, emitted_by: str = _DEFAULT_EMITTER) -> None:
    """Emit a WARN record."""
    log_event("warn", emitted_by, message)


def error(message: str, *, emitted_by: str = _DEFAULT_EMITTER) -> None:
    """Emit an ERROR record."""
    log_event("error", emitted_by, message)
