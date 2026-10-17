# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One message read off an input port, as `ctx.inputs.read_message` returns it."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Generic, TypeVar

__all__ = ["Message"]

_MessagePayload_co = TypeVar("_MessagePayload_co", covariant=True)


@dataclass(frozen=True)
class Message(Generic[_MessagePayload_co]):
    """One message read off an input port, with what the engine knows about it.

    `payload` is the bytes the producer wrote, or what `into.from_bytes` built
    from them. `timestamp_ns` is the producer's stamp. `starts_group` is the
    flag the producer wrote, so a reader can resume at a group's first message.
    `lost_since_last_read` counts the messages `inbound_link` lost between the
    previous message read from it and this one; each loss is counted once, and
    the cumulative count `graph` renders is never zeroed. `inbound_link` names
    the link the message arrived on, `{source node id}/{source output port}`.
    """

    payload: _MessagePayload_co
    timestamp_ns: int
    starts_group: bool
    lost_since_last_read: int
    inbound_link: str
