# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""What more than one helper placement test file reads a running stream with."""

from __future__ import annotations

from collections.abc import Callable

from conftest import TatolabdUnderTest


StartTatolabdRunningStream = Callable[..., TatolabdUnderTest]


def the_first_value_batch_read_after(
    tatolabd: TatolabdUnderTest, monotonic_ns: int, *, from_occurrence: int
) -> "tuple[dict, int]":
    """The first batch the sink started reading after `monotonic_ns`, and its occurrence."""
    occurrence = from_occurrence
    while True:
        batch = tatolabd.await_marker("FUNCTION_NODE_VALUE_BATCH_READ", occurrence=occurrence)
        if batch["started_at_monotonic_ns"] > monotonic_ns:
            return batch, occurrence
        occurrence += 1
