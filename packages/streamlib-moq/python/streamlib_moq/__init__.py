# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Media over QUIC publish and subscribe for StreamLib.

An extension wheel: the Rust is inside this package and the two `@processor`
classes below are the binding. Nothing here links the engine — the wheel depends
on `streamlib` as a binary, and each processor runs in its own helper process
like any other Python processor.
"""

from .processors import MoqBroadcastPublisher as MoqBroadcastPublisher
from .processors import MoqBroadcastSubscriber as MoqBroadcastSubscriber

__all__ = ["MoqBroadcastPublisher", "MoqBroadcastSubscriber"]
