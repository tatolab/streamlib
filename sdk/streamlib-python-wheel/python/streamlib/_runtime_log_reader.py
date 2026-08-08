# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Reading a runtime's on-disk JSONL log file.

The JSONL log schema is a durable contract, and so is the pretty rendering: a
replayed line must match what the runtime mirrored to its own stdout, byte for
byte, or the same record read two ways reads as two records. Both are mirrored
from the engine's `format_event_pretty`; the field names are the contract, and
this file is a consumer of it, never a second definition.

streamlib:lint-logging:allow-file — this module's whole job is rendering log
records to a terminal on a user's request. Routing that through the engine's log
pipeline would re-ingest the records it was asked to display.
"""

from __future__ import annotations

import json
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Generator, NamedTuple, Optional, TextIO

__all__ = [
    "LogRecordFilters",
    "wait_for_runtime_log_file",
    "format_started_at",
    "format_size",
    "RuntimeLogFile",
    "runtime_log_directory_path",
    "enumerate_runtime_log_files",
    "newest_log_file_for_runtime",
    "format_record_pretty",
    "read_log_file",
]

#: Severity order, lowest first — a `--level` floor admits everything at or
#: above it. Mirrors the engine's `level_rank`.
_LEVEL_ORDER = ("trace", "debug", "info", "warn", "error")

#: Rendered severity column: five characters, right-aligned, exactly as the
#: engine's stdout mirror writes it.
_LEVEL_COLUMN = {
    "trace": "TRACE",
    "debug": "DEBUG",
    "info": " INFO",
    "warn": " WARN",
    "error": "ERROR",
}

#: How long the follow loop waits before re-checking a file that is at EOF.
_FOLLOW_POLL_SECONDS = 0.1


def format_started_at(started_at_millis: int) -> str:
    """A log file's start time as an ISO-8601 UTC stamp.

    `--list` exists so a human can pick a runtime_id; raw epoch millis defeats
    that, which is why the engine's own listing rendered a date.

    A stamp outside the representable range degrades to the raw number rather
    than raising — the file name is parsed with an unbounded `int()`, so one
    stray file in the directory would otherwise take the whole listing down and
    hide every healthy runtime with it.
    """
    try:
        stamped = datetime.fromtimestamp(started_at_millis / 1000, tz=timezone.utc)
    except (ValueError, OSError, OverflowError):
        return str(started_at_millis)
    return stamped.strftime("%Y-%m-%dT%H:%M:%SZ")


def format_size(size_bytes: int) -> str:
    """A byte count in the binary units the engine's listing used."""
    for unit, scale in (("GiB", 1024**3), ("MiB", 1024**2), ("KiB", 1024)):
        if size_bytes >= scale:
            return f"{size_bytes / scale:.1f} {unit}"
    return f"{size_bytes} B"


def runtime_log_directory_path() -> Path:
    """The directory the engine writes per-runtime JSONL logs into.

    Resolved by the engine, not recomputed here: `STREAMLIB_HOME` and the
    walk-up that backs it are the engine's rules, and a second implementation
    would drift into reporting "no logs" for a runtime that logged fine.
    """
    from ._engine import runtime_log_directory

    return runtime_log_directory()


class RuntimeLogFile(NamedTuple):
    """One `<runtime_id>-<started_at_millis>.jsonl` file on disk."""

    runtime_id: str
    started_at_millis: int
    path: Path
    size_bytes: int


class LogRecordFilters(NamedTuple):
    """The `--processor` / `--level` / … narrowing applied to each record."""

    processor: "Optional[str]" = None
    pipeline: "Optional[str]" = None
    rhi_only: bool = False
    minimum_level: "Optional[str]" = None
    source: "Optional[str]" = None
    intercepted_only: bool = False

    def matches(self, record: "dict[str, Any]") -> bool:
        """Whether `record` survives every filter that is set."""
        if self.processor is not None and record.get("processor_id") != self.processor:
            return False
        if self.pipeline is not None and record.get("pipeline_id") != self.pipeline:
            return False
        if self.rhi_only and record.get("rhi_op") is None:
            return False
        if self.minimum_level is not None:
            level = record.get("level", "trace")
            if level not in _LEVEL_ORDER:
                return False
            if _LEVEL_ORDER.index(level) < _LEVEL_ORDER.index(self.minimum_level):
                return False
        if self.source is not None and record.get("source") != self.source:
            return False
        if self.intercepted_only and not record.get("intercepted", False):
            return False
        return True


def enumerate_runtime_log_files(log_directory: Path) -> "list[RuntimeLogFile]":
    """Every parseable `<runtime_id>-<millis>.jsonl` under `log_directory`."""
    if not log_directory.is_dir():
        return []

    found: "list[RuntimeLogFile]" = []
    for entry in log_directory.iterdir():
        if entry.suffix != ".jsonl":
            continue
        # `runtime_id` may itself contain dashes, so split on the LAST one.
        runtime_id, separator, millis_text = entry.stem.rpartition("-")
        if not separator or not runtime_id:
            continue
        try:
            started_at_millis = int(millis_text)
            size_bytes = entry.stat().st_size
        except (ValueError, OSError):
            continue
        found.append(
            RuntimeLogFile(
                runtime_id=runtime_id,
                started_at_millis=started_at_millis,
                path=entry,
                size_bytes=size_bytes,
            )
        )
    return found


def newest_log_file_for_runtime(
    log_directory: Path, runtime_id: str
) -> "Optional[RuntimeLogFile]":
    """The most recently started log file for `runtime_id`, if any."""
    candidates = [
        log_file
        for log_file in enumerate_runtime_log_files(log_directory)
        if log_file.runtime_id == runtime_id
    ]
    if not candidates:
        return None
    return max(candidates, key=lambda log_file: log_file.started_at_millis)


def _format_wall_clock_time(host_ts_nanoseconds: int) -> str:
    """`HH:MM:SS.mmm` from a nanosecond stamp, as the engine's mirror renders it.

    Deliberately not a date: the authoritative stamp stays in the JSONL as
    `host_ts`, and this column exists to be skimmed while tailing.
    """
    total_seconds = host_ts_nanoseconds // 1_000_000_000
    milliseconds = (host_ts_nanoseconds % 1_000_000_000) // 1_000_000
    hours = (total_seconds // 3600) % 24
    minutes = (total_seconds // 60) % 60
    seconds = total_seconds % 60
    return f"{hours:02}:{minutes:02}:{seconds:02}.{milliseconds:03}"


def _render_attribute_value(value: Any) -> str:
    """One `attrs` value as `serde_json::Value`'s `Display` writes it.

    Two places where the obvious `json.dumps` call diverges from serde_json:

    - `ensure_ascii` escapes non-ASCII, where serde passes UTF-8 through raw
      (its escape table marks 0x80-0xFF as no-escape), so a `café` in an attr
      would render `caf\u00e9` here and `café` in the runtime's own mirror.
    - Python spells an exponent `1e+20` / `1e-07`; ryu emits the shortest
      round-trip form `1e20` / `1e-7`. Same value, different spelling.
    - ryu switches to decimal one decade earlier than Python does at the small
      end: its rule is decimal when `-5 < kk <= 0`, so `1e-5` is written
      `0.00001` where Python still writes `1e-05`. That is exactly one band —
      `1e-6` and below are exponential on both sides, `1e-4` and above decimal
      on both — so it is expanded here rather than left as a divergence.
    """
    if isinstance(value, float) and not isinstance(value, bool):
        rendered = json.dumps(value)
        mantissa, exponent_marker, exponent = rendered.partition("e")
        if not exponent_marker:
            return rendered
        negative_exponent = exponent.startswith("-")
        magnitude = exponent.lstrip("+-").lstrip("0") or "0"
        if negative_exponent and magnitude == "5":
            sign = "-" if mantissa.startswith("-") else ""
            digits = mantissa.lstrip("-").replace(".", "")
            return f"{sign}0.0000{digits}"
        return f"{mantissa}e{'-' if negative_exponent else ''}{magnitude}"
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


def format_record_pretty(record: "dict[str, Any]") -> str:
    """Render one JSONL record exactly as the engine's stdout mirror does.

    Field order and separators are the contract. `attrs` values go through
    [`_render_attribute_value`], which matches `serde_json::Value`'s `Display` —
    strings keep their quotes, numbers stay bare.
    """
    level = record.get("level", "info")
    rendered = (
        f"{_format_wall_clock_time(int(record.get('host_ts', 0)))} "
        f"[{_LEVEL_COLUMN.get(level, level.upper()):>5}] "
        f"[{record.get('runtime_id', '')}/{record.get('source', '')}] "
        f"{record.get('target', '')} — {record.get('message', '')}"
    )
    for optional_column in ("pipeline_id", "processor_id", "rhi_op"):
        value = record.get(optional_column)
        if value is not None:
            rendered += f" {optional_column}={value}"
    for attribute_name in sorted(record.get("attrs", {})):
        attribute_value = record["attrs"][attribute_name]
        rendered += f" {attribute_name}={_render_attribute_value(attribute_value)}"
    return rendered


def _decode_line(line: str, errors: TextIO) -> "Optional[dict[str, Any]]":
    """One JSONL line as a record, or `None` (reported) if it is malformed.

    A truncated final line is normal while a runtime is still writing, so a bad
    line is skipped with a note rather than ending the read.
    """
    trimmed = line.strip()
    if not trimmed:
        return None
    try:
        decoded = json.loads(trimmed)
    except ValueError as decode_failure:
        print(f"warning: skipping malformed JSONL line: {decode_failure}", file=errors)
        return None
    return decoded if isinstance(decoded, dict) else None


def wait_for_runtime_log_file(
    log_directory: Path, runtime_id: str, errors: TextIO
) -> RuntimeLogFile:
    """Block until `runtime_id` has a log file, for `--follow` before a boot.

    Following a node you are about to start is the point of `--follow`; failing
    because the file does not exist yet would refuse the one case the flag is
    for.
    """
    print(
        f"note: no log file yet for runtime '{runtime_id}', waiting in --follow mode...",
        file=errors,
    )
    while True:
        log_file = newest_log_file_for_runtime(log_directory, runtime_id)
        if log_file is not None:
            return log_file
        time.sleep(_FOLLOW_POLL_SECONDS)


def read_log_file(
    log_file: RuntimeLogFile,
    filters: LogRecordFilters,
    *,
    follow: bool,
    errors: TextIO,
    log_directory: Path,
) -> "Generator[str, None, None]":
    """Yield rendered lines from `log_file`, optionally tailing it forever.

    Drains what is already there, then — with `follow` — polls for appended
    bytes. A restart under a pinned `STREAMLIB_RUNTIME_ID` writes a SECOND file
    for the same runtime, so the tail switches to it and says so; without that
    the tail sits on a file that will never grow again and goes silently quiet.
    The caller owns the loop, so a `KeyboardInterrupt` stops the tail without
    unwinding through file handling.
    """
    current = log_file
    while True:
        with current.path.open("r", encoding="utf-8", errors="replace") as opened:
            while True:
                line = opened.readline()
                if line:
                    record = _decode_line(line, errors)
                    if record is not None and filters.matches(record):
                        yield format_record_pretty(record)
                    continue
                if not follow:
                    return
                newer = newest_log_file_for_runtime(log_directory, current.runtime_id)
                if newer is not None and newer.started_at_millis > current.started_at_millis:
                    print(
                        f"note: runtime '{current.runtime_id}' rotated to a newer "
                        f"log file; switching.",
                        file=errors,
                    )
                    current = newer
                    break
                time.sleep(_FOLLOW_POLL_SECONDS)
