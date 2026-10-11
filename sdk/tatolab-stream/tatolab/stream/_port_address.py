# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A port's address, `<machine>/<stream>/<node>/<port>`, read right-anchored.

Leading chunks left out mean here: `camera/video` is a port of this stream,
`main/camera/video` one of stream `main` on this machine. Pure Python, so it runs
where no engine is loaded. The engine's `PortAddress` reads an address the same
way, and both are held to
`runtime/streamlib-engine/tests/fixtures/port_address_cases.json`.
"""

from __future__ import annotations

from dataclasses import dataclass

from ._exposed_name_cast import (
    ExposedNameCastsToNothingError,
    cast_exposed_name_to_url_safe,
)

__all__ = [
    "PORT_ADDRESS_CHUNK_SEPARATOR",
    "InvalidPortAddressError",
    "PortAddress",
    "parse_port_address",
]

PORT_ADDRESS_CHUNK_SEPARATOR = "/"
"""What splits an address into its chunks."""

# Right-anchored: an address of n chunks names the last n of these.
_PARTS_OF_A_FULL_PORT_ADDRESS = ("machine", "stream", "node", "port")
_FEWEST_CHUNKS_OF_A_PORT_ADDRESS = 2


class InvalidPortAddressError(ValueError):
    """An address naming no port: not 2 to 4 chunks, or a chunk the name cast leaves nothing of."""


@dataclass(frozen=True, slots=True)
class PortAddress:
    """A port's address, each chunk cast; a machine or stream left out is `None`, meaning here."""

    machine_name: str | None
    stream_name: str | None
    node_name: str
    port_name: str

    def __str__(self) -> str:
        return PORT_ADDRESS_CHUNK_SEPARATOR.join(
            chunk
            for chunk in (
                self.machine_name,
                self.stream_name,
                self.node_name,
                self.port_name,
            )
            if chunk is not None
        )


def parse_port_address(address: str) -> PortAddress:
    """Read `address` right-anchored, each chunk cast; refuse it naming the part that names nothing."""
    chunks = address.split(PORT_ADDRESS_CHUNK_SEPARATOR)
    if not (
        _FEWEST_CHUNKS_OF_A_PORT_ADDRESS
        <= len(chunks)
        <= len(_PARTS_OF_A_FULL_PORT_ADDRESS)
    ):
        raise InvalidPortAddressError(
            f"the port address {address!r} has {len(chunks)} "
            f"chunk{'' if len(chunks) == 1 else 's'} split by "
            f"{PORT_ADDRESS_CHUNK_SEPARATOR!r}, and an address has "
            f"{_FEWEST_CHUNKS_OF_A_PORT_ADDRESS} to {len(_PARTS_OF_A_FULL_PORT_ADDRESS)}, "
            f"right-anchored: `<node>/<port>` in this stream, `<stream>/<node>/<port>` on "
            f"this machine, `<machine>/<stream>/<node>/<port>`"
        )
    cast_chunks_by_part = {
        part: _cast_chunk_of_the_address(address, part, chunk)
        for part, chunk in zip(_PARTS_OF_A_FULL_PORT_ADDRESS[-len(chunks) :], chunks)
    }
    return PortAddress(
        machine_name=cast_chunks_by_part.get("machine"),
        stream_name=cast_chunks_by_part.get("stream"),
        node_name=cast_chunks_by_part["node"],
        port_name=cast_chunks_by_part["port"],
    )


def _cast_chunk_of_the_address(address: str, part: str, chunk: str) -> str:
    try:
        return cast_exposed_name_to_url_safe(chunk)
    except ExposedNameCastsToNothingError as casts_to_nothing:
        why_it_names_nothing = (
            f"it is empty — a {PORT_ADDRESS_CHUNK_SEPARATOR!r} doubled, leading or trailing"
            if chunk == ""
            else str(casts_to_nothing)
        )
        raise InvalidPortAddressError(
            f"the {part} chunk {chunk!r} of the port address {address!r} cannot name a "
            f"{part}: {why_it_names_nothing}"
        ) from casts_to_nothing
