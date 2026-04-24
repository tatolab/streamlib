# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Subprocess-side interceptors that route `print`, raw stdout/stderr
writes, and `logging` records through `streamlib.log`.

Three layers, independent and additive:

1. `sys.stdout` / `sys.stderr` replaced with a line-buffered writer that
   emits one record per `\\n`. Records carry `intercepted=True` and
   `channel=stdout|stderr`.
2. A root `logging.Handler` that converts `logging` records into log
   entries tagged `channel=logging`.
3. Parent-side fd-level capture lives in the Rust host
   (`spawn_python_native_subprocess_op.rs::spawn`) — it reads fd2 lines and
   tags them `channel=fd2` directly. fd1 is the IPC channel and is not
   intercepted; see #443 AI-agent notes.
"""

from __future__ import annotations

import logging
import sys
import threading
from typing import List, Optional

from . import log


_installed = False
_original_stdout = None
_original_stderr = None
_root_handler: Optional[logging.Handler] = None


# ============================================================================
# Line-buffered writer — replaces sys.stdout / sys.stderr
# ============================================================================


class _LineBufferedInterceptor:
    """Text-mode writer that emits one log record per completed line.

    Trailing partial lines (no final `\\n`) stay buffered until the next
    `\\n` or an explicit `flush()` / `close()`.
    """

    def __init__(self, channel: str, level: str):
        self._channel = channel
        self._level = level
        self._buf: List[str] = []
        self._lock = threading.Lock()

    def write(self, data) -> int:
        if isinstance(data, (bytes, bytearray)):
            text = data.decode("utf-8", "replace")
        elif isinstance(data, str):
            text = data
        else:
            text = str(data)

        with self._lock:
            self._buf.append(text)
            combined = "".join(self._buf)
            if "\n" not in combined:
                # Fast path — nothing to emit yet.
                self._buf = [combined] if combined else []
                return len(data)
            lines = combined.split("\n")
            trailing = lines[-1]
            complete = lines[:-1]
            self._buf = [trailing] if trailing else []

        for line in complete:
            log.emit_intercepted(self._level, line, self._channel)
        return len(data)

    def flush(self) -> None:
        with self._lock:
            if not self._buf:
                return
            partial = "".join(self._buf)
            self._buf = []
        if partial:
            log.emit_intercepted(self._level, partial, self._channel)

    def close(self) -> None:
        self.flush()

    def writable(self) -> bool:
        return True

    def readable(self) -> bool:
        return False

    def isatty(self) -> bool:
        return False

    def fileno(self) -> int:
        raise OSError(
            f"streamlib intercepted {self._channel!r} writer has no fileno"
        )

    @property
    def encoding(self) -> str:
        return "utf-8"

    @property
    def errors(self) -> str:
        return "replace"


# ============================================================================
# logging.Handler — routes `logging` records through streamlib.log
# ============================================================================


_PY_LEVEL_TO_STREAMLIB = {
    logging.DEBUG: "debug",
    logging.INFO: "info",
    logging.WARNING: "warn",
    logging.ERROR: "error",
    logging.CRITICAL: "error",
}


class _RootLoggingHandler(logging.Handler):
    """Root handler that converts every `logging` record into a streamlib log
    entry with `channel="logging"` and `intercepted=True`."""

    def __init__(self) -> None:
        super().__init__(level=logging.DEBUG)

    def emit(self, record: logging.LogRecord) -> None:
        try:
            level = _PY_LEVEL_TO_STREAMLIB.get(record.levelno, "warn")
            message = self.format(record)
            attrs = {"logger": record.name}
            if record.exc_info:
                import traceback as _tb
                attrs["exc"] = "".join(
                    _tb.format_exception(*record.exc_info)
                )
            log.emit_intercepted(level, message, "logging", attrs)
        except Exception:
            # Never let the log handler raise into user code.
            self.handleError(record)


# ============================================================================
# Install / uninstall
# ============================================================================


def install() -> None:
    """Replace `sys.stdout` / `sys.stderr` with line-buffered interceptors
    and install the root logging handler.

    Safe to call multiple times — subsequent calls are no-ops.
    """
    global _installed, _original_stdout, _original_stderr, _root_handler
    if _installed:
        return

    _original_stdout = sys.stdout
    _original_stderr = sys.stderr
    sys.stdout = _LineBufferedInterceptor("stdout", "warn")
    sys.stderr = _LineBufferedInterceptor("stderr", "warn")

    root = logging.getLogger()
    root.setLevel(logging.DEBUG)
    # Remove any pre-existing stream handlers — otherwise a basicConfig()
    # call before install() leaves stderr writers in place that now feed
    # back into our interceptor, creating duplicate records.
    for existing in list(root.handlers):
        if isinstance(existing, logging.StreamHandler) and not isinstance(
            existing, _RootLoggingHandler
        ):
            root.removeHandler(existing)
    _root_handler = _RootLoggingHandler()
    root.addHandler(_root_handler)

    _installed = True


def uninstall() -> None:
    """Restore the original stdout/stderr and remove the logging handler.

    Used by tests and during subprocess shutdown.
    """
    global _installed, _original_stdout, _original_stderr, _root_handler
    if not _installed:
        return
    try:
        if isinstance(sys.stdout, _LineBufferedInterceptor):
            sys.stdout.flush()
        if isinstance(sys.stderr, _LineBufferedInterceptor):
            sys.stderr.flush()
    except Exception:
        pass
    if _original_stdout is not None:
        sys.stdout = _original_stdout
    if _original_stderr is not None:
        sys.stderr = _original_stderr
    root = logging.getLogger()
    if _root_handler is not None:
        try:
            root.removeHandler(_root_handler)
        except Exception:
            pass
    _original_stdout = None
    _original_stderr = None
    _root_handler = None
    _installed = False
