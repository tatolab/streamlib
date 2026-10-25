# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The contexts and link endpoints a node is handed while it runs.

Each is a `typing.Protocol`: the runtime's native class is the only
implementation, held to it member for member by the wheel's conformance gate.
"""

from __future__ import annotations

from collections.abc import Sequence
from typing import TYPE_CHECKING, Any, Protocol, TypeVar, overload

from ._runtime_lend import runtime_backed_function, runtime_backed_protocol

if TYPE_CHECKING:
    from ._gpu_protocols import GpuContextFullAccess, GpuContextLimitedAccess
    from .catalog import CatalogEntry
    from ._message import Message

_MessageReadTarget = TypeVar("_MessageReadTarget")
_MessageReadTarget_co = TypeVar("_MessageReadTarget_co", covariant=True)


class CastFromMessagePayloadBytes(Protocol[_MessageReadTarget_co]):
    """What `read(port, into=…)` takes: anything whose `from_bytes` builds the read's result.

    A class with a `from_bytes` classmethod is one — `into=VideoFrame` reads a
    `VideoFrame` — and so is `msgpack_typed_dict_cast(SomeTypedDict)`.
    """

    def from_bytes(self, payload: bytes, /) -> _MessageReadTarget_co:
        """Build the read's result from one message's payload."""
        ...


@runtime_backed_protocol
class NodeLinkDataAccess(Protocol):
    """One node's links. The engine binds it; app code never builds one."""

    def declare_ports(
        self, input_port_names: Sequence[str], output_port_names: Sequence[str]
    ) -> None:
        """Name the class's ports ahead of any wiring.

        A declared port no link has reached reads as empty and drops what is
        written to it; only an undeclared name is refused.
        """
        ...

    def open_loss_count_board(
        self, service_name: str, output_port_names: list[str]
    ) -> None:
        """Open the loss-count board the parent created for this spawn.

        Every link wired afterwards mirrors its losses onto it as they are
        counted — an input link onto the slot its wiring names, an output port
        onto its entry under its channel's generation — which is how the parent
        renders this helper's `metrics` in `graph`. `output_port_names` is the
        board's output-port section in key order. Raises `RuntimeError` for a
        second board.
        """
        ...

    def wire_output_link(
        self,
        port_name: str,
        channel_service_name: str,
        dest_notify_service_name: str,
        expected_payload_bytes: int,
        max_payload_bytes_per_channel: int,
        channel_service_creation_depth: int,
        max_subscribers: int,
        notify_max_notifiers: int,
        link_id: str,
        output_port_wiring_generation: int | None = None,
    ) -> None:
        """Open this node's publisher and one notifier for a link out of
        `port_name`.

        Once a loss-count board is open, `output_port_wiring_generation` names
        the port's channel its refusals are mirrored under, and omitting it
        raises `ValueError`; with no board it is unused.
        """
        ...

    def wire_input_link(
        self,
        port_name: str,
        channel_service_name: str,
        inbound_link_name: str,
        notify_service_name: str,
        read_mode: str,
        channel_service_creation_depth: int,
        input_port_ring_depth: int,
        max_subscribers: int,
        notify_max_notifiers: int,
        link_id: str,
        audio_window: dict[str, Any] | None = None,
        loss_count_slot: int | None = None,
        wiring_generation: int | None = None,
    ) -> None:
        """Open this node's subscriber for one link into `port_name`.

        `channel_service_name` is what this end subscribes to;
        `inbound_link_name` is what a read hands back as the link's name.

        Once a loss-count board is open, `loss_count_slot` and
        `wiring_generation` name where the link's losses are mirrored, and
        omitting either raises `ValueError`; with no board they are unused.
        """
        ...

    def unwire_output_link(self, port_name: str, link_id: str) -> None:
        """Close this node's end of one link out of `port_name`."""
        ...

    def unwire_input_link(self, link_id: str) -> None:
        """Close this node's end of one inbound link."""
        ...

    def input_listener_fd(self) -> int | None:
        """The fd that turns readable when any input link has data, if one is open."""
        ...

    def drain_input_listener(self) -> None:
        """Clear the input listener's pending wake-ups."""
        ...

    def any_input_port_has_data(self) -> bool:
        """Whether any input port has a message waiting."""
        ...

    @overload
    def read_from_input_port(
        self, port_name: str, *, into: None = None
    ) -> bytes | None: ...
    @overload
    def read_from_input_port(
        self,
        port_name: str,
        *,
        into: CastFromMessagePayloadBytes[_MessageReadTarget],
    ) -> _MessageReadTarget | None: ...
    def read_from_input_port(
        self,
        port_name: str,
        *,
        into: CastFromMessagePayloadBytes[Any] | None = None,
    ) -> Any:
        """The next message's payload on `port_name`, cast by `into` when given, or `None`."""
        ...

    def read_from_input_port_with_timestamp(
        self, port_name: str
    ) -> tuple[bytes, int] | tuple[None, None]:
        """The next message's payload on `port_name` with its timestamp, or `(None, None)`."""
        ...

    def input_port_has_data(self, port_name: str) -> bool:
        """Whether `port_name` has a message waiting."""
        ...

    def write_to_output_port(
        self,
        port_name: str,
        payload: bytes,
        timestamp_ns: int | None = None,
        starts_group: bool = True,
    ) -> None:
        """Publish one message to every downstream link on `port_name`."""
        ...


class _RuntimeContextMembersBothCapabilitiesShare(Protocol):
    """The runtime-context members the limited and the full context both carry."""

    @property
    def config(self) -> dict[str, Any]:
        """The node's configuration."""
        ...

    @property
    def time(self) -> int:
        """The runtime's monotonic time in nanoseconds."""
        ...

    @property
    def inputs(self) -> LinkInputDataReader:
        """The node's input ports."""
        ...

    @property
    def outputs(self) -> LinkOutputDataWriter:
        """The node's output ports."""
        ...

    @property
    def gpu_limited_access(self) -> GpuContextLimitedAccess:
        """The non-allocating GPU capability."""
        ...

    @property
    def runtime_id(self) -> str:
        """The id of the runtime running this node."""
        ...

    @property
    def node_id(self) -> str:
        """This node's id, the one `graph` renders on the node."""
        ...

    def is_paused(self) -> bool:
        """Whether the node is paused."""
        ...

    def should_process(self) -> bool:
        """Whether the node should process now."""
        ...


@runtime_backed_protocol
class RuntimeContextFullAccess(_RuntimeContextMembersBothCapabilitiesShare, Protocol):
    """Privileged runtime context handed to `setup` / `teardown` / `start` / `stop`.

    Built by the runtime in the interpreter it starts for the node; a node never
    constructs one.
    """

    @property
    def gpu_full_access(self) -> GpuContextFullAccess:
        """The privileged GPU capability."""
        ...


@runtime_backed_protocol
class RuntimeContextLimitedAccess(_RuntimeContextMembersBothCapabilitiesShare, Protocol):
    """Restricted runtime context handed to `process` / `on_pause` / `on_resume`.

    `gpu_full_access` is deliberately absent — reaching for it raises
    `AttributeError`, mirroring the Rust capability split.
    """


@runtime_backed_protocol
class LinkInputDataReader(Protocol):
    """A node's input ports, as `ctx.inputs`.

    A port name is cast the way `@node` casts it — lowercased, accents dropped,
    anything outside a-z 0-9 - . _ ~ turned into `-` — so any spelling that
    casts to a declared port finds it; one casting to nothing raises `ValueError`.
    """

    @overload
    def read(self, port_name: str, *, into: None = None) -> bytes | None: ...
    @overload
    def read(
        self,
        port_name: str,
        *,
        into: CastFromMessagePayloadBytes[_MessageReadTarget],
    ) -> _MessageReadTarget | None: ...
    def read(
        self,
        port_name: str,
        *,
        into: CastFromMessagePayloadBytes[Any] | None = None,
    ) -> Any:
        """The next message's payload on `port_name`, or `None` when none is waiting.

        With `into`, the payload is handed to `into.from_bytes` and its result
        returned, so a payload that does not fit raises here, at the consuming
        read. `into=VideoFrame` constructs and validates;
        `into=msgpack_typed_dict_cast(SomeTypedDict)` types the map and checks
        nothing.
        """
        ...

    def read_with_timestamp(
        self, port_name: str
    ) -> tuple[bytes, int] | tuple[None, None]:
        """The next message's payload on `port_name` with its timestamp, or `(None, None)`."""
        ...

    @overload
    def read_from_inbound_link(
        self, port_name: str, *, into: None = None
    ) -> tuple[bytes, str] | None: ...
    @overload
    def read_from_inbound_link(
        self,
        port_name: str,
        *,
        into: CastFromMessagePayloadBytes[_MessageReadTarget],
    ) -> tuple[_MessageReadTarget, str] | None: ...
    def read_from_inbound_link(
        self,
        port_name: str,
        *,
        into: CastFromMessagePayloadBytes[Any] | None = None,
    ) -> Any:
        """The next message's payload on `port_name` with the link it arrived on, or `None`.

        Any number of links may enter one input port, each a separate producer.
        The name is the source channel the link subscribed to —
        `{source node id}/{source output port}`, the name `graph` shows — which
        the engine knows and a producer cannot misstate.

        Messages from one link arrive in that link's order; nothing is promised
        about how two links interleave. `into` casts as it does for `read`.
        """
        ...

    @overload
    def read_from_inbound_link_with_timestamp(
        self, port_name: str, *, into: None = None
    ) -> tuple[bytes, str, int] | None: ...
    @overload
    def read_from_inbound_link_with_timestamp(
        self,
        port_name: str,
        *,
        into: CastFromMessagePayloadBytes[_MessageReadTarget],
    ) -> tuple[_MessageReadTarget, str, int] | None: ...
    def read_from_inbound_link_with_timestamp(
        self,
        port_name: str,
        *,
        into: CastFromMessagePayloadBytes[Any] | None = None,
    ) -> Any:
        """The next message's payload on `port_name` with its link and its timestamp.

        The stamp is the one the producer wrote — the source frame's instant,
        not the moment of the read.
        """
        ...

    @overload
    def read_message(
        self, port_name: str, *, into: None = None
    ) -> Message[bytes] | None: ...
    @overload
    def read_message(
        self,
        port_name: str,
        *,
        into: CastFromMessagePayloadBytes[_MessageReadTarget],
    ) -> Message[_MessageReadTarget] | None: ...
    def read_message(
        self,
        port_name: str,
        *,
        into: CastFromMessagePayloadBytes[Any] | None = None,
    ) -> Any:
        """The next message on `port_name` with its stamp, group flag, losses and link, or `None`.

        `lost_since_last_read` counts the messages its link lost since the
        previous message read from that link, so a reader that needs every
        message of a group — a decoder — discards until the next message whose
        `starts_group` is set. `into` casts the payload as it does for `read`.
        A message no link delivered is refused by name, and the refusal
        consumes it.
        """
        ...

    def count_the_message_last_read_as_lost(self, port_name: str) -> None:
        """Count the message last read from `port_name` as lost on the link that delivered it.

        For a message whose frame is gone by the time it is read — its producer
        recycled the frame before this node's claim, which the read's `into`
        cast raises naming the recycling. The loss lands in the link's dropped
        count `graph` renders and in the next message's `lost_since_last_read`,
        never as an error. Each message counts once; raises `RuntimeError` when
        nothing read from the port since its last count came over a link.
        """
        ...

    def inbound_link_names(self, port_name: str) -> list[str]:
        """Every link feeding `port_name`, in wiring order.

        Readable in `setup()` — links are wired before it runs — so a sink
        learns how many producers it owes before the first message arrives. A
        port nothing is connected to lists none.
        """
        ...

    def has_data(self, port_name: str) -> bool:
        """Whether `port_name` has a message waiting."""
        ...

    def inbound_link_catalog_entry(
        self, port_name: str, *, inbound_link: str | None = None
    ) -> dict[str, Any] | None:
        """The catalog entry the output port feeding `port_name` carries now.

        `{"section": ..., "entry": {...}}` in hang's layout, or `None` when
        nothing feeds the port or the upstream port carries no entry. A port fed
        by more than one link names the one to read with `inbound_link`, the
        name a read reports it under; without it, such a port raises naming its
        links. The entry is documentation — nothing checks a payload against it.
        """
        ...


@runtime_backed_protocol
class LinkOutputDataWriter(Protocol):
    """A node's output ports, as `ctx.outputs`.

    A port name is cast the way `@node` casts it — lowercased, accents dropped,
    anything outside a-z 0-9 - . _ ~ turned into `-` — so any spelling that
    casts to a declared port finds it; one casting to nothing raises `ValueError`.
    """

    def write(
        self,
        port_name: str,
        payload: bytes,
        timestamp_ns: int | None = None,
        starts_group: bool = True,
    ) -> None:
        """Publish one message carrying `payload` to every downstream link on `port_name`.

        `payload` is `bytes` and nothing else — `tatolab.stream.msgpack.pack`
        encodes a record. Omitting `timestamp_ns` stamps the monotonic now.
        `starts_group=True` says the message stands alone; only encoded media
        passes `False`, for a message a decoder cannot enter at. A payload over
        the channel's ceiling is dropped and counted against this port, never
        raised.
        """
        ...

    def set_catalog_entry(self, port_name: str, entry: CatalogEntry) -> None:
        """Replace the catalog entry `port_name` carries — what `graph` renders
        under the port and what a downstream reader reads off its inbound link.

        `entry` is one of `tatolab.stream.catalog`'s entries. One that hang's
        catalog types do not read raises naming the port and the field, and
        the port keeps the entry it had.
        """
        ...


@runtime_backed_function()
def gpu_limited_access_of_the_typed_read_in_progress() -> (
    GpuContextLimitedAccess | None
):
    """The GPU capability of the `read(port, into=T)` currently constructing an
    object, or `None` when nothing is being read into a type.

    The same capability as `ctx.gpu_limited_access`, offered so a type can do
    per-frame work at construction that needs the engine — claiming the frame's
    surface against producer reuse is what the shipped `VideoFrame` does with
    it. Any class reachable through `into=` may call this; there is no
    registration, no marker and no privileged type.
    """
    ...
