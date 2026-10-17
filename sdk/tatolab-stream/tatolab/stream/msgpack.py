# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Msgpack message payloads: pack a mapping, unpack bytes, and cast bytes into a type.

A message's payload is bytes, and the runtime never encodes or decodes it. These
helpers are the encoding the built-ins' records use — a msgpack map with string
keys, `bytes` as msgpack `bin` — for a node that writes or reads one:
`ctx.outputs.write(port, pack(record))` and
`ctx.inputs.read(port, into=VideoFrame)`.
"""

from __future__ import annotations

import dataclasses
from collections.abc import Mapping
from typing import TYPE_CHECKING, Any, Generic, TypeVar, cast

import msgpack

if TYPE_CHECKING:
    from typing_extensions import Self

__all__ = [
    "MsgpackCast",
    "MsgpackTypedDictCast",
    "msgpack_typed_dict_cast",
    "pack",
    "unpack",
]

_TypedDictReadTarget = TypeVar("_TypedDictReadTarget")

# Field metadata naming the message key a dataclass field is written under,
# for a cast whose attribute is not spelled like the wire key it reads.
_MESSAGE_KEY_FIELD_METADATA = "tatolab.stream.msgpack.message_key"


# The bound the runtime's tap decoder keeps, so whatever packs can be tapped.
MAXIMUM_NESTED_CONTAINER_DEPTH = 128


def _describe_container_path(container_path: tuple[str | int, ...]) -> str:
    if not container_path:
        return "at the top level"
    return "under " + " > ".join(
        repr(step) if isinstance(step, str) else str(step) for step in container_path
    )


def _refuse_what_no_reader_can_read(
    value: Any, container_path: tuple[str | int, ...], container_depth: int
) -> None:
    if not isinstance(value, (Mapping, list, tuple)):
        return
    if container_depth > MAXIMUM_NESTED_CONTAINER_DEPTH:
        raise ValueError(
            f"pack() refuses containers that nest more than "
            f"{MAXIMUM_NESTED_CONTAINER_DEPTH} deep; this one does "
            f"{_describe_container_path(container_path)}"
        )
    if isinstance(value, Mapping):
        for key, element in value.items():
            if not isinstance(key, str):
                raise TypeError(
                    f"pack() encodes maps with string keys at every level; got the "
                    f"{type(key).__name__} key {key!r} "
                    f"{_describe_container_path(container_path)}"
                )
            _refuse_what_no_reader_can_read(
                element, (*container_path, key), container_depth + 1
            )
    else:
        for index, element in enumerate(value):
            _refuse_what_no_reader_can_read(
                element, (*container_path, index), container_depth + 1
            )


def pack(mapping: Mapping[str, Any]) -> bytes:
    """Encode `mapping` as msgpack bytes, `bytes` values as msgpack `bin`.

    Every map has string keys and containers nest at most 128 deep, the record
    itself the outermost; anything else is refused at the write, by its path.
    """
    if not isinstance(mapping, Mapping):
        raise TypeError(
            f"pack() encodes a mapping with string keys; got {type(mapping).__name__}"
        )
    _refuse_what_no_reader_can_read(mapping, (), 1)
    packed = msgpack.packb(
        mapping if isinstance(mapping, dict) else dict(mapping), use_bin_type=True
    )
    return cast(bytes, packed)


def unpack(payload: bytes) -> Any:
    """Decode msgpack bytes into ordinary Python data, msgpack `str` as `str`.

    A map's keys come back as written, so a Rust producer's integer-keyed map reads.
    """
    return msgpack.unpackb(payload, raw=False, strict_map_key=False)


def _message_value_of(value: Any) -> Any:
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        return _message_entries_of(value)
    if isinstance(value, (list, tuple)):
        return [_message_value_of(element) for element in value]
    if isinstance(value, dict):
        return {key: _message_value_of(element) for key, element in value.items()}
    return value


def _message_entries_of(dataclass_object: Any) -> dict[str, Any]:
    return {
        declared_field.metadata.get(
            _MESSAGE_KEY_FIELD_METADATA, declared_field.name
        ): _message_value_of(getattr(dataclass_object, declared_field.name))
        for declared_field in dataclasses.fields(dataclass_object)
    }


class MsgpackCast:
    """A mixin that reads a class from a msgpack map of its keys and writes it back as one.

    `from_bytes` is the class's own keyword construction over the unpacked map,
    so its validation runs at the read. `to_bytes` packs a dataclass's fields,
    nested dataclasses as nested maps, and reads back equal through `from_bytes`.
    """

    @classmethod
    def from_bytes(cls, payload: bytes) -> Self:
        """Unpack `payload` and construct this class from the map's keys."""
        message_entries = unpack(payload)
        if not isinstance(message_entries, dict):
            raise TypeError(
                f"a {cls.__name__} is read from a msgpack map; the payload holds a "
                f"{type(message_entries).__name__}"
            )
        return cls(**message_entries)

    def to_bytes(self) -> bytes:
        """This object's dataclass fields as the msgpack map `from_bytes` reads."""
        if not dataclasses.is_dataclass(self):
            raise TypeError(
                f"MsgpackCast.to_bytes packs a dataclass's fields; {type(self).__name__} "
                f"is not a dataclass, so it defines its own to_bytes"
            )
        return pack(_message_entries_of(self))


class MsgpackTypedDictCast(Generic[_TypedDictReadTarget]):
    """The `into=` target for a TypedDict: the unpacked map, typed and unvalidated."""

    def __init__(self, typed_dict_class: type[_TypedDictReadTarget]) -> None:
        self.typed_dict_class = typed_dict_class

    def from_bytes(self, payload: bytes) -> _TypedDictReadTarget:
        """Unpack `payload` and hand the map back as the TypedDict, checking nothing."""
        return cast(_TypedDictReadTarget, unpack(payload))


def msgpack_typed_dict_cast(
    typed_dict_class: type[_TypedDictReadTarget],
) -> MsgpackTypedDictCast[_TypedDictReadTarget]:
    """Read a msgpack map as `typed_dict_class`: `read(port, into=msgpack_typed_dict_cast(T))`.

    A TypedDict cannot take the `MsgpackCast` mixin, so this is its read target.
    """
    return MsgpackTypedDictCast(typed_dict_class)
