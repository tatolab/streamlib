# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Joins bags from several ports that all descend from one camera frame.

Every stream in this app carries the source frame's `timestamp_ns` forward —
the camera stamps it, the effects keep it, the pose bag copies it — so the
stamp is a join key, not just an ordering. The join holds early arrivals per
stream until every stream has answered for a stamp, then hands the complete
row out; stamps that never complete age out instead of pooling.
"""

from __future__ import annotations

from typing import Any

__all__ = ["TimestampStreamJoin"]

# How many distinct stamps a stream may run ahead before the oldest
# incomplete row is abandoned — a lagging stream costs those rows, never
# unbounded memory.
DEFAULT_PENDING_ROW_LIMIT = 90


class TimestampStreamJoin:
    """Complete rows across named streams, keyed by `timestamp_ns`."""

    def __init__(
        self,
        stream_names: "tuple[str, ...]",
        pending_row_limit: int = DEFAULT_PENDING_ROW_LIMIT,
    ) -> None:
        if len(stream_names) < 2:
            raise ValueError(
                f"a join across {len(stream_names)} stream is not a join — "
                f"give it two or more"
            )
        self._stream_names = stream_names
        self._pending_row_limit = pending_row_limit
        self._pending: "dict[int, dict[str, Any]]" = {}
        self.rows_abandoned = 0

    def offer(
        self, stream_name: str, timestamp_ns: int, value: Any
    ) -> "dict[str, Any] | None":
        """Add one arrival; the completed row when this one completes it.

        A row completes at most once — completing removes it — and a second
        arrival for a stamp a stream already answered replaces the first,
        which `latest`-profile re-reads make ordinary rather than an error.
        """
        row = self._pending.setdefault(timestamp_ns, {})
        row[stream_name] = value
        if len(row) == len(self._stream_names):
            del self._pending[timestamp_ns]
            return row

        # Oldest-first eviction, so one stalled stream cannot pool rows.
        while len(self._pending) > self._pending_row_limit:
            oldest = min(self._pending)
            del self._pending[oldest]
            self.rows_abandoned += 1
        return None
