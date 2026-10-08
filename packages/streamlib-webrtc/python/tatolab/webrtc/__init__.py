# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""WHIP publish and WHEP play for StreamLib.

An extension wheel: the Rust is inside this package and the two `@node`
classes below are the binding. Nothing here links the engine — the wheel depends
on `tatolab-stream`, and each processor runs in its own processor interpreter
like any other Python processor. The transport stack comes up in that
interpreter the first time a session needs it.
"""

from .processors import WhepPlayer as WhepPlayer
from .processors import WhepPlayerConfig as WhepPlayerConfig
from .processors import WhipPublisher as WhipPublisher
from .processors import WhipPublisherConfig as WhipPublisherConfig

__all__ = ["WhepPlayer", "WhepPlayerConfig", "WhipPublisher", "WhipPublisherConfig"]
