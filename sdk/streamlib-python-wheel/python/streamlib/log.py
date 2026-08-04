# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Logging from a processor, straight onto the engine's log pipeline.

Writing to stdout also works, but while the engine is alive the stdio
interceptor captures it and re-emits it at WARN. These functions carry the
level the author meant and interleave in order with the engine's own records.
Keyword arguments become the structured `attrs` columns of the JSONL record —
`log.info("captured frame", width=1920)` — and processor attribution is
automatic inside lifecycle hooks; nothing needs to be threaded through.
"""

from __future__ import annotations

from typing import Any

from ._engine import log_event

__all__ = ["debug", "error", "info", "trace", "warn", "warning"]


def trace(message: str, **attrs: Any) -> None:
    """Emit a TRACE record."""
    log_event("trace", message, attrs or None)


def debug(message: str, **attrs: Any) -> None:
    """Emit a DEBUG record."""
    log_event("debug", message, attrs or None)


def info(message: str, **attrs: Any) -> None:
    """Emit an INFO record."""
    log_event("info", message, attrs or None)


def warn(message: str, **attrs: Any) -> None:
    """Emit a WARN record."""
    log_event("warn", message, attrs or None)


warning = warn


def error(message: str, **attrs: Any) -> None:
    """Emit an ERROR record."""
    log_event("error", message, attrs or None)
