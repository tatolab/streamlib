# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The name a virtual camera carries in every picker, stable across runs of its stream."""

from __future__ import annotations

import dataclasses

from .v4l2_output_device_abi import V4L2LOOPBACK_CARD_LABEL_BYTES

DEFAULT_CAMERA_NAME_PREFIX = "StreamLib Camera"

CAMERA_NAME_CAPACITY_BYTES = V4L2LOOPBACK_CARD_LABEL_BYTES - 1
"""The longest name the module's `card_label` holds beside its NUL."""

_FNV1A_64_OFFSET_BASIS = 0xCBF29CE484222325
_FNV1A_64_PRIME = 0x100000001B3
_UINT64_MASK = 2**64 - 1
_BASE36_ALPHABET = "0123456789abcdefghijklmnopqrstuvwxyz"
_STABLE_ID_CHARACTERS = 4


def fnv1a_64_hash_of(data: bytes) -> int:
    """FNV-1a over `data`: the engine's stable-id hash, the same on every run."""
    hash_value = _FNV1A_64_OFFSET_BASIS
    for byte in data:
        hash_value = ((hash_value ^ byte) * _FNV1A_64_PRIME) & _UINT64_MASK
    return hash_value


def four_base36_characters_of(hash_value: int) -> str:
    """Four base-36 characters of `hash_value`, low digits first."""
    characters = []
    for _ in range(_STABLE_ID_CHARACTERS):
        characters.append(_BASE36_ALPHABET[hash_value % 36])
        hash_value //= 36
    return "".join(characters)


@dataclasses.dataclass(frozen=True)
class TheNodeAVirtualCameraIsNamedFor:
    """What a camera's stable id is derived from: the same on every run of the same stream."""

    stream_project_directory: str
    stream_id: str
    node_name: str


def stable_camera_id(node: TheNodeAVirtualCameraIsNamedFor) -> str:
    """Four base-36 characters of a hash over the project directory, the stream's id and the node's name."""
    hash_value = fnv1a_64_hash_of(
        node.stream_project_directory.encode("utf-8", "surrogateescape")
    )
    for name in (node.stream_id, node.node_name):
        hash_value = fnv1a_64_hash_of(
            hash_value.to_bytes(8, "little") + name.encode("utf-8")
        )
    return four_base36_characters_of(hash_value)


def camera_name_for(
    configured_name: "str | None", node: TheNodeAVirtualCameraIsNamedFor
) -> str:
    """The configured name cut to the label's capacity, else the default prefix plus the stable id."""
    trimmed_name = (configured_name or "").strip()
    if not trimmed_name:
        return f"{DEFAULT_CAMERA_NAME_PREFIX} {stable_camera_id(node)}"
    name_bytes = trimmed_name.encode("utf-8")[:CAMERA_NAME_CAPACITY_BYTES]
    return name_bytes.decode("utf-8", errors="ignore").rstrip()
