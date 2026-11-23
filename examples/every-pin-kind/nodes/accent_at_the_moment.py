# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`AccentAtTheMoment`, the sample's own conversion from a Boolean into a String that reads the
stream's time.

It states that it converts, but its body takes the stream's time, which no stream gives when a
Graph Input is retyped with no stream started: a Graph Input saving a Boolean that is retyped
to a String gives way to the String's default, and the retype's answer says why.
"""

from __future__ import annotations

from tatolab.stream import Time, node

NANOSECONDS_PER_SECOND = 1_000_000_000


@node.meta(
    category="Every pin kind",
    description="Whether a beat is accented, as the moment of the stream it was read at.",
    conversion=True,
)
@node
def AccentAtTheMoment(accent: bool = False, *, now: Time) -> str:
    """`accent`, as the second of the stream it was read at, or nothing when it is off."""
    if not accent:
        return ""
    return f"accented at {now // NANOSECONDS_PER_SECOND}s"
