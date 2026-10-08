# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Logging from a node, straight onto the engine's log pipeline.

Writing to stdout also works, but while the engine is alive the stdio
interceptor captures it and re-emits it at WARN. These functions carry the
level the author meant and interleave in order with the engine's own records.
Keyword arguments become the structured `attrs` columns of the JSONL record —
`log.info("captured frame", width=1920)` — and node attribution is
automatic inside lifecycle hooks; nothing needs to be threaded through.

Each function hands its record to the runtime from the interpreter the
runtime starts for a node; called anywhere else it raises `RuntimeError`
naming itself.
"""

from __future__ import annotations

from typing import Any, Callable, Optional

from ._runtime_lend import RuntimeIsNotLentToThisInterpreterError

__all__ = ["debug", "error", "info", "trace", "warn", "warning"]

HelperProcessLogSink = Callable[[str, str, "Optional[dict[str, Any]]"], None]

# A helper process has no engine in it, so its records travel to the parent's
# pipeline instead of being handed straight to one. Installed by
# `tatolab/runtime/_processor_interpreter_bootstrap.py` at startup and never by
# app code.
_helper_process_sink: "Optional[HelperProcessLogSink]" = None


def install_helper_process_sink(sink: HelperProcessLogSink) -> None:
    """Route this process's records to its parent. Called only by the helper."""
    global _helper_process_sink
    _helper_process_sink = sink


def _emit(
    level: str,
    message: str,
    attrs: "Optional[dict[str, Any]]",
    called_function_name: str,
) -> None:
    sink = _helper_process_sink
    if sink is None:
        raise RuntimeIsNotLentToThisInterpreterError(
            f"{called_function_name}() runs only in the interpreter the runtime "
            "starts for a node, which hands its records to the runtime; this "
            "interpreter has nowhere to send them"
        )
    sink(level, message, attrs)


def trace(message: str, **attrs: Any) -> None:
    """Emit a TRACE record."""
    _emit("trace", message, attrs or None, "log.trace")


def debug(message: str, **attrs: Any) -> None:
    """Emit a DEBUG record."""
    _emit("debug", message, attrs or None, "log.debug")


def info(message: str, **attrs: Any) -> None:
    """Emit an INFO record."""
    _emit("info", message, attrs or None, "log.info")


def warn(message: str, **attrs: Any) -> None:
    """Emit a WARN record."""
    _emit("warn", message, attrs or None, "log.warn")


def warning(message: str, **attrs: Any) -> None:
    """Emit a WARN record, as `warn` does."""
    _emit("warn", message, attrs or None, "log.warning")


def error(message: str, **attrs: Any) -> None:
    """Emit an ERROR record."""
    _emit("error", message, attrs or None, "log.error")
