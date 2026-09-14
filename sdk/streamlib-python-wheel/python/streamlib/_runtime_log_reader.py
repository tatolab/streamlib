# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Reading a runtime's on-disk JSONL log segments.

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
import os
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Generator, NamedTuple, Optional, TextIO

__all__ = [
    "LogRecordFilters",
    "RuntimeLogFile",
    "enumerate_runtime_log_files",
    "format_record_pretty",
    "format_size",
    "format_started_at",
    "newest_log_file_for_runtime",
    "read_log_file",
    "runtime_log_directory_path",
    "wait_for_runtime_log_file",
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
    """One runtime instance's JSONL log on disk.

    `path` names the active `<runtime_id>-<started_at_millis>.jsonl` segment and
    `size_bytes` counts every segment of the instance, rotated ones included.
    """

    runtime_id: str
    started_at_millis: int
    path: Path
    size_bytes: int


class _RuntimeLogSegmentName(NamedTuple):
    """What a segment's file name says about it."""

    runtime_id: str
    started_at_millis_text: str
    rotation_sequence: "Optional[int]"


def _parse_runtime_log_segment_name(file_name: str) -> "Optional[_RuntimeLogSegmentName]":
    """A segment's identity from its file name, or `None` for any other file.

    The active segment is `<runtime_id>-<started_at_millis>.jsonl` and a rotated one
    `<runtime_id>-<started_at_millis>.<rotation_sequence>.jsonl`, the shape the
    engine's `rotated_runtime_log_segment_path` writes. `runtime_id` may carry
    dashes and dots: an active stem always ends `-<digits>`, so the text after its
    last dot is never all digits and the two shapes cannot be confused.
    """
    if not file_name.endswith(".jsonl"):
        return None
    stem = file_name[: -len(".jsonl")]
    rotation_sequence: "Optional[int]" = None
    before_sequence, dot, sequence_text = stem.rpartition(".")
    if dot and sequence_text.isascii() and sequence_text.isdigit():
        stem = before_sequence
        rotation_sequence = int(sequence_text)
    runtime_id, separator, millis_text = stem.rpartition("-")
    if not separator or not runtime_id:
        return None
    if not (millis_text.isascii() and millis_text.isdigit()):
        return None
    return _RuntimeLogSegmentName(runtime_id, millis_text, rotation_sequence)


def _rotated_segment_path(active_segment_path: Path, rotation_sequence: int) -> Path:
    """Where rotation `rotation_sequence` of `active_segment_path` lives."""
    return active_segment_path.with_name(
        f"{active_segment_path.stem}.{rotation_sequence}.jsonl"
    )


def _rotated_segment_sequences(active_segment_path: Path) -> "list[int]":
    """The rotated segments of `active_segment_path` on disk, oldest first."""
    active = _parse_runtime_log_segment_name(active_segment_path.name)
    if active is None:
        return []
    try:
        entries = list(active_segment_path.parent.iterdir())
    except OSError:
        return []
    sequences = []
    for entry in entries:
        segment = _parse_runtime_log_segment_name(entry.name)
        if (
            segment is not None
            and segment.rotation_sequence is not None
            and segment.runtime_id == active.runtime_id
            and segment.started_at_millis_text == active.started_at_millis_text
        ):
            sequences.append(segment.rotation_sequence)
    return sorted(sequences)


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
    """Every runtime instance with a parseable log segment under `log_directory`."""
    if not log_directory.is_dir():
        return []

    size_bytes_by_instance: "dict[tuple[str, str], int]" = {}
    for entry in log_directory.iterdir():
        segment = _parse_runtime_log_segment_name(entry.name)
        if segment is None:
            continue
        try:
            size_bytes = entry.stat().st_size
        except OSError:
            continue
        instance = (segment.runtime_id, segment.started_at_millis_text)
        size_bytes_by_instance[instance] = size_bytes_by_instance.get(instance, 0) + size_bytes

    return [
        RuntimeLogFile(
            runtime_id=runtime_id,
            started_at_millis=int(millis_text),
            path=log_directory / f"{runtime_id}-{millis_text}.jsonl",
            size_bytes=size_bytes,
        )
        for (runtime_id, millis_text), size_bytes in size_bytes_by_instance.items()
    ]


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
    if not isinstance(decoded, dict):
        print("warning: skipping JSONL line that is not a record object", file=errors)
        return None
    # Shape, not just JSON-validity: a record carrying `host_ts: null` or a
    # non-object `attrs` decodes fine and then takes the renderer down, which
    # would end the whole read over one bad line — the opposite of what
    # skipping a malformed line is for.
    if (
        not isinstance(decoded.get("host_ts"), int)
        or not isinstance(decoded.get("level"), str)
        or not isinstance(decoded.get("attrs", {}), dict)
    ):
        print(
            "warning: skipping JSONL line whose fields do not match the record schema",
            file=errors,
        )
        return None
    return decoded


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


class _SegmentLineReader:
    """Whole lines from one open segment, holding back a line still being written.

    A batch lands in the file across more than one write, so a reader at the live
    edge can see the front of a record before its newline; decoding that front
    would report a healthy record as malformed and lose it.
    """

    def __init__(self, opened: TextIO) -> None:
        self._opened = opened
        self._unfinished_line = ""

    def complete_lines(self) -> "Generator[str, None, None]":
        """Every line the segment holds whole right now."""
        while True:
            chunk = self._opened.readline()
            if not chunk:
                return
            if not chunk.endswith("\n"):
                self._unfinished_line += chunk
                continue
            yield self._unfinished_line + chunk
            self._unfinished_line = ""

    def unfinished_line(self) -> str:
        """The line left without its newline, handed over once the segment is done."""
        unfinished_line, self._unfinished_line = self._unfinished_line, ""
        return unfinished_line


def _open_active_segment_after_listing(
    active_segment_path: Path,
) -> "tuple[Optional[TextIO], list[int]]":
    """The active segment opened, beside the rotated segments that precede it.

    A rotation landing between the listing and the open would hand back an active
    segment the listing does not account for, so the listing is retaken until its
    newest sequence is unchanged across the open.
    """
    while True:
        listed = _rotated_segment_sequences(active_segment_path)
        try:
            opened = active_segment_path.open("r", encoding="utf-8", errors="replace")
        except FileNotFoundError:
            return None, listed
        if _rotated_segment_sequences(active_segment_path)[-1:] == listed[-1:]:
            return opened, listed
        opened.close()


def _held_segment_was_rotated_away(held: TextIO, active_segment_path: Path) -> bool:
    """Whether the active segment's name no longer points at the file `held` reads."""
    held_status = os.fstat(held.fileno())
    try:
        named_status = active_segment_path.stat()
    except FileNotFoundError:
        return True
    return (named_status.st_dev, named_status.st_ino) != (
        held_status.st_dev,
        held_status.st_ino,
    )


def _lines_of_rotated_segment(
    active_segment_path: Path, rotation_sequence: int, errors: TextIO
) -> "Generator[str, None, None]":
    """Every line of one rotated segment, or a note if retention removed it first."""
    rotated_path = _rotated_segment_path(active_segment_path, rotation_sequence)
    try:
        opened = rotated_path.open("r", encoding="utf-8", errors="replace")
    except FileNotFoundError:
        print(
            f"note: log segment {rotated_path.name} was removed by retention before "
            f"it was read; skipping.",
            file=errors,
        )
        return
    with opened:
        yield from opened


def _lines_of_runtime_log_instance(
    log_file: RuntimeLogFile, *, follow: bool, errors: TextIO
) -> "Generator[Optional[str], None, None]":
    """Every line of one runtime instance's log, oldest segment first.

    With `follow`, yields `None` each time the read reaches the live edge with
    nothing new, so the caller can look for a restart and wait. Rotation renames
    the active segment and reopens its name, which the held file is checked against
    at every edge: once they part, the held file is drained as the rotated segment
    after the last one read, the segments rotated since are read, and the new active
    segment is opened.
    """
    active_segment_path = log_file.path
    active, rotated_sequences = _open_active_segment_after_listing(active_segment_path)
    last_read_rotation_sequence = 0
    try:
        while True:
            for rotation_sequence in rotated_sequences:
                if rotation_sequence <= last_read_rotation_sequence:
                    continue
                yield from _lines_of_rotated_segment(
                    active_segment_path, rotation_sequence, errors
                )
                last_read_rotation_sequence = rotation_sequence

            if active is None:
                # Between a rotation's rename and its reopen, or never reopened.
                if not follow:
                    return
                yield None
            else:
                reader = _SegmentLineReader(active)
                rotated_away = False
                while not rotated_away:
                    yield from reader.complete_lines()
                    rotated_away = _held_segment_was_rotated_away(active, active_segment_path)
                    if rotated_away:
                        yield from reader.complete_lines()
                        last_read_rotation_sequence += 1
                    elif not follow:
                        break
                    else:
                        yield None
                unfinished_line = reader.unfinished_line()
                if unfinished_line:
                    yield unfinished_line
                active.close()
                active = None
                if not rotated_away:
                    return

            active, rotated_sequences = _open_active_segment_after_listing(
                active_segment_path
            )
    finally:
        if active is not None:
            active.close()


def read_log_file(
    log_file: RuntimeLogFile,
    filters: LogRecordFilters,
    *,
    follow: bool,
    errors: TextIO,
    log_directory: Path,
) -> "Generator[str, None, None]":
    """Yield rendered lines from `log_file`'s segments, optionally tailing forever.

    Drains the rotated segments oldest first and then the active one, carrying on
    across any rotation that lands mid-read. With `follow` it then polls for
    appended bytes. A restart under a pinned `STREAMLIB_RUNTIME_ID` writes a SECOND
    instance for the same runtime, so the tail switches to it and says so; without
    that the tail sits on a file that will never grow again and goes silently quiet.
    The caller owns the loop, so a `KeyboardInterrupt` stops the tail without
    unwinding through file handling.
    """
    current = log_file
    while True:
        lines = _lines_of_runtime_log_instance(current, follow=follow, errors=errors)
        switched_to_newer_instance = False
        try:
            for line in lines:
                if line is None:
                    newer = newest_log_file_for_runtime(log_directory, current.runtime_id)
                    if (
                        newer is not None
                        and newer.started_at_millis > current.started_at_millis
                    ):
                        print(
                            f"note: runtime '{current.runtime_id}' restarted into a newer "
                            f"log file; switching.",
                            file=errors,
                        )
                        current = newer
                        switched_to_newer_instance = True
                        break
                    time.sleep(_FOLLOW_POLL_SECONDS)
                    continue
                record = _decode_line(line, errors)
                if record is not None and filters.matches(record):
                    yield format_record_pretty(record)
        finally:
            lines.close()
        if not switched_to_newer_instance:
            return
