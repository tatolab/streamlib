# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The contexts and link endpoints a node is handed while it runs.

Each is a `typing.Protocol`: the runtime's native class is the only
implementation, held to it member for member by the wheel's conformance gate.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping, Sequence
from typing import TYPE_CHECKING, Any, Protocol, TypeVar, overload

from ._runtime_lend import runtime_backed_function, runtime_backed_protocol

if TYPE_CHECKING:
    from ._gpu_protocols import GpuContextFullAccess, GpuContextLimitedAccess

_BagReadTarget = TypeVar("_BagReadTarget")


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
        """Whether any input port has a bag waiting."""
        ...

    @overload
    def read_from_input_port(
        self, port_name: str, *, into: None = None
    ) -> Any | None: ...
    @overload
    def read_from_input_port(
        self, port_name: str, *, into: Callable[..., _BagReadTarget]
    ) -> _BagReadTarget | None: ...
    def read_from_input_port(
        self, port_name: str, *, into: Callable[..., Any] | None = None
    ) -> Any:
        """The next bag on `port_name`, read into `into` when given, or `None`."""
        ...

    def read_from_input_port_with_timestamp(
        self, port_name: str
    ) -> tuple[Any, int] | tuple[None, None]:
        """The next bag on `port_name` with its timestamp, or `(None, None)`."""
        ...

    def input_port_has_data(self, port_name: str) -> bool:
        """Whether `port_name` has a bag waiting."""
        ...

    def write_to_output_port(
        self,
        port_name: str,
        bag: Mapping[str, Any],
        timestamp_ns: int | None = None,
    ) -> None:
        """Publish one bag to every downstream link on `port_name`."""
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
    def read(self, port_name: str, *, into: None = None) -> Any | None: ...
    @overload
    def read(
        self, port_name: str, *, into: Callable[..., _BagReadTarget]
    ) -> _BagReadTarget | None: ...
    def read(self, port_name: str, *, into: Callable[..., Any] | None = None) -> Any:
        """The next bag on `port_name`, read into `into`.

        The opt-in strictness dial. A TypedDict casts for free — the bag
        arrives as itself, unvalidated. A dataclass or pydantic model is
        constructed from the bag's entries, so a bag that does not fit raises
        here, at the consuming read.
        """
        ...

    def read_with_timestamp(
        self, port_name: str
    ) -> tuple[Any, int] | tuple[None, None]:
        """The next bag on `port_name` with its timestamp, or `(None, None)`."""
        ...

    @overload
    def read_from_inbound_link(
        self, port_name: str, *, into: None = None
    ) -> tuple[Any, str] | None: ...
    @overload
    def read_from_inbound_link(
        self, port_name: str, *, into: Callable[..., _BagReadTarget]
    ) -> tuple[_BagReadTarget, str] | None: ...
    def read_from_inbound_link(
        self, port_name: str, *, into: Callable[..., Any] | None = None
    ) -> Any:
        """The next bag on `port_name` with the link it arrived on, or `None`.

        Any number of links may enter one input port, and each one is a
        separate producer. This is how a many-input node tells them
        apart: the name is the source channel the link subscribed to —
        `{source node id}/{source output port}`, the name `graph` and
        `tap` show. The engine knows it and a producer cannot misstate it.

        Bags from one link arrive in that link's order. Nothing is promised
        about how two links interleave, so a reader that needs time order
        reasons per link.

        `into` is the same strictness dial `read` carries.
        """
        ...

    @overload
    def read_from_inbound_link_with_timestamp(
        self, port_name: str, *, into: None = None
    ) -> tuple[Any, str, int] | None: ...
    @overload
    def read_from_inbound_link_with_timestamp(
        self, port_name: str, *, into: Callable[..., _BagReadTarget]
    ) -> tuple[_BagReadTarget, str, int] | None: ...
    def read_from_inbound_link_with_timestamp(
        self, port_name: str, *, into: Callable[..., Any] | None = None
    ) -> Any:
        """The next bag on `port_name` with its link and its timestamp.

        The fan-in read and the timestamped read at once, which a many-track
        sink needs together: the link names the producer, and the stamp is the
        one that producer wrote — the source frame's instant, not the moment of
        the read. Restating a producer's timing downstream needs both.
        """
        ...

    def inbound_link_names(self, port_name: str) -> list[str]:
        """Every link feeding `port_name`, in wiring order.

        Readable in `setup()` — links are wired before it runs — which is how
        a sink learns how many producers it owes before the first bag
        arrives. A port nothing is connected to lists none.
        """
        ...

    def has_data(self, port_name: str) -> bool:
        """Whether `port_name` has a bag waiting."""
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
        bag: Mapping[str, Any],
        timestamp_ns: int | None = None,
    ) -> None:
        """Publish one bag to every downstream link on `port_name`.

        A bag over the channel's payload ceiling is refused here and counted
        against this port, never raised. Loss from a consumer that cannot keep
        up is a separate mechanism and lands at the consuming port.
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
