# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""An output port's catalog entry, in the layout the MoQ players read.

One dataclass per catalog section. Each field is named as hang's catalog
spells it (`codedWidth`, `sampleRate`, `numberOfChannels`) because the entry
reaches every player that reads the broadcast's catalog unchanged — a port
describes itself in the players' words, never the runtime's. Declare one with
`@node.output(catalog=...)` and replace it while running with
`ctx.outputs.set_catalog_entry(port_name, entry)`. The runtime parses every
entry with hang's own types and refuses one that does not parse, naming the
port and the field.
"""

from __future__ import annotations

import dataclasses
from typing import Any, ClassVar, Literal, TypeAlias

__all__ = [
    "AudioEntry",
    "BinaryEntry",
    "CatalogEntry",
    "JsonEntry",
    "TextEntry",
    "VideoEntry",
]


def _fields_stated(entry: Any) -> "dict[str, Any]":
    return {
        field.name: getattr(entry, field.name)
        for field in dataclasses.fields(entry)
        if getattr(entry, field.name) is not None
    }


@dataclasses.dataclass(frozen=True)
class VideoEntry:
    """A `video` entry: one rendition a player may pick."""

    section: ClassVar[str] = "video"

    codec: str
    """The WebCodecs codec string, e.g. `avc1.64001f`."""
    codedWidth: "int | None" = None
    codedHeight: "int | None" = None
    description: "str | None" = None
    """A decoder's out-of-band setup, hex-encoded."""

    def to_dict(self) -> "dict[str, Any]":
        """The entry's fields as hang spells them, the unstated left out."""
        return _fields_stated(self)


@dataclasses.dataclass(frozen=True)
class AudioEntry:
    """An `audio` entry: one rendition a player may pick."""

    section: ClassVar[str] = "audio"

    codec: str
    """The WebCodecs codec string, e.g. `opus`."""
    sampleRate: int
    numberOfChannels: int
    description: "str | None" = None
    """A decoder's out-of-band setup, hex-encoded."""

    def to_dict(self) -> "dict[str, Any]":
        """The entry's fields as hang spells them, the unstated left out."""
        return _fields_stated(self)


@dataclasses.dataclass(frozen=True)
class TextEntry:
    """A `text` entry: a timed-text track."""

    section: ClassVar[str] = "text"

    format: str
    """The cue format, e.g. `vtt`."""

    def to_dict(self) -> "dict[str, Any]":
        """The entry's fields as hang spells them, the unstated left out."""
        return _fields_stated(self)


@dataclasses.dataclass(frozen=True)
class JsonEntry:
    """A `json` entry: a data track of JSON documents."""

    section: ClassVar[str] = "json"

    mode: Literal["snapshot", "stream"]
    """`snapshot`: each group supersedes the last. `stream`: one append log."""

    def to_dict(self) -> "dict[str, Any]":
        """The entry's fields as hang spells them, the unstated left out."""
        return _fields_stated(self)


@dataclasses.dataclass(frozen=True)
class BinaryEntry:
    """A `binary` entry: a data track of opaque payloads."""

    section: ClassVar[str] = "binary"

    mode: Literal["snapshot", "stream"]
    """`snapshot`: each group supersedes the last. `stream`: one append log."""
    mime: "str | None" = None
    """The payload's media type, e.g. `application/msgpack`."""

    def to_dict(self) -> "dict[str, Any]":
        """The entry's fields as hang spells them, the unstated left out."""
        return _fields_stated(self)


CatalogEntry: TypeAlias = "VideoEntry | AudioEntry | TextEntry | JsonEntry | BinaryEntry"
"""Any one section's entry."""
