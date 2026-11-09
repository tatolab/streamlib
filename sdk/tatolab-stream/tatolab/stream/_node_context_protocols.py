# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The contexts and link endpoints a node is handed while it runs.

Each is a `typing.Protocol`: the runtime's native class is the only
implementation, held to it member for member by the wheel's conformance gate.
"""

from __future__ import annotations

from collections.abc import Buffer, Sequence
from types import TracebackType
from typing import TYPE_CHECKING, Any, Literal, Protocol, TypeVar, overload

from ._runtime_lend import runtime_backed_function, runtime_backed_protocol

if TYPE_CHECKING:
    from .clock import StreamClockReading
    from ._gpu_protocols import GpuContextFullAccess, GpuContextLimitedAccess
    from .stream_facts import PortStreamFacts
    from ._message import Message
    from ._port_family_declaration import Input, Output

_MessageReadTarget = TypeVar("_MessageReadTarget")
_MessageReadTarget_co = TypeVar("_MessageReadTarget_co", covariant=True)
_PortFamilyReadType = TypeVar("_PortFamilyReadType")


@runtime_backed_protocol
class PortPayloadBorrow(Protocol):
    """A read message's payload, held in the shared-memory sample it arrived in.

    A read-only buffer: `memoryview(payload)` and `numpy.frombuffer(payload)`
    read the sample itself, and `bytes(payload)` is the
    one copy. The sample counts against its link's borrow quota while the borrow
    or any `memoryview` or DLPack tensor over it lives.
    """

    def __len__(self) -> int: ...
    def __bytes__(self) -> bytes: ...
    def __buffer__(self, flags: int, /) -> memoryview: ...
    def __dlpack__(
        self,
        *,
        stream: Any | None = None,
        max_version: tuple[int, int] | None = None,
        dl_device: tuple[int, int] | None = None,
        copy: bool | None = None,
    ) -> Any:
        """The payload as a read-only 1-D `uint8` CPU tensor over the sample.

        `copy=True`, a device other than the CPU and a stream are refused.
        """
        ...

    def __dlpack_device__(self) -> tuple[int, int]: ...


@runtime_backed_protocol
class UnreadMessage(Protocol):
    """A message of any family a `@node.function` body hands on without reading it.

    Its body can only return it, or `None` to hand out nothing, and keep it in
    its state; the output takes the family of what is linked into the input it
    came from. A pooled GPU frame it names stays claimed while any copy lives.
    """

    def __copy__(self) -> UnreadMessage:
        """The message itself: it is immutable, so a copy is the same message."""
        ...

    def __deepcopy__(self, memo: Any) -> UnreadMessage:
        """The message itself, as a state record's deep copy takes it."""
        ...


@runtime_backed_protocol
class ChannelsBlock(Protocol):
    """A Channels block a read returned: planar samples borrowed in the sample they arrived in.

    `numpy.from_dlpack(block)` is a read-only `[channel][sample]` array over the
    sample, with no copy. The sample counts against its link's borrow quota
    while the block or any tensor over it lives.
    """

    @property
    def start_sample(self) -> int:
        """The stream-wide index of the block's first sample."""
        ...

    @property
    def sample_rate(self) -> int:
        """Samples per second, per channel; 0 on a control value, which has no regular rate."""
        ...

    @property
    def channel_count(self) -> int:
        """How many channels the block holds."""
        ...

    @property
    def samples_per_channel(self) -> int:
        """How many samples each channel holds."""
        ...

    @property
    def dtype(self) -> str:
        """The sample type, `"f32"` or `"f64"`."""
        ...

    @property
    def timestamp_ns(self) -> int:
        """The stream time the block was stamped with, in nanoseconds."""
        ...

    def __dlpack__(
        self,
        *,
        stream: Any | None = None,
        max_version: tuple[int, int] | None = None,
        dl_device: tuple[int, int] | None = None,
        copy: bool | None = None,
    ) -> Any:
        """The samples as a read-only `[channel][sample]` CPU tensor over the sample.

        `copy=True`, a device other than the CPU and a stream are refused.
        """
        ...

    def __dlpack_device__(self) -> tuple[int, int]: ...


@runtime_backed_protocol
class ArrayFrameCpuAttribute(Protocol):
    """One CPU attribute of an Array frame a read returned: a row-major tensor borrowed in the sample.

    `numpy.from_dlpack(attribute)` reads it in place. The sample counts against
    its link's borrow quota while any attribute of the frame lives.
    """

    @property
    def dtype(self) -> str:
        """The element type's wire name, such as `"float32"`."""
        ...

    @property
    def shape(self) -> list[int]:
        """The dimensions, outermost first."""
        ...

    def __dlpack__(
        self,
        *,
        stream: Any | None = None,
        max_version: tuple[int, int] | None = None,
        dl_device: tuple[int, int] | None = None,
        copy: bool | None = None,
    ) -> Any:
        """The attribute as a read-only CPU tensor over the sample.

        `copy=True`, a device other than the CPU and a stream are refused.
        """
        ...

    def __dlpack_device__(self) -> tuple[int, int]: ...


@runtime_backed_protocol
class PacketsUnit(Protocol):
    """A Packets unit a read returned: its bytes borrowed in the sample, with its stamp and codec."""

    @property
    def payload(self) -> PortPayloadBorrow:
        """The unit's bytes, a borrow of the sample."""
        ...

    @property
    def pts(self) -> int:
        """The presentation stamp, in stream nanoseconds."""
        ...

    @property
    def starts_group(self) -> bool:
        """Whether a decoder may start at this unit: a keyframe, or any unit of a plain byte stream."""
        ...

    @property
    def codec(self) -> str:
        """The codec the bytes are encoded with, or `""` for a plain byte stream."""
        ...


@runtime_backed_protocol
class PortPayloadLoan(Protocol):
    """A sample loaned on an output port for its writer to fill in place.

    `with ctx.outputs.loan(port, byte_len) as out:` hands out a writable buffer
    over the sample. A clean exit publishes it; a raise gives it back
    unpublished. Leaving the block while a `memoryview` or DLPack tensor over it
    lives raises `BufferError` and publishes nothing, and the port stays loaned,
    refusing a write and a second loan, until the last export goes.
    """

    def __enter__(self) -> PortPayloadLoan: ...
    def __exit__(
        self,
        exception_type: type[BaseException] | None = None,
        exception: BaseException | None = None,
        traceback: TracebackType | None = None,
    ) -> Literal[False]: ...
    def __len__(self) -> int: ...
    def __buffer__(self, flags: int, /) -> memoryview: ...
    def __release_buffer__(self, buffer: memoryview, /) -> None: ...
    def __dlpack__(
        self,
        *,
        stream: Any | None = None,
        max_version: tuple[int, int] | None = None,
        dl_device: tuple[int, int] | None = None,
        copy: bool | None = None,
    ) -> Any:
        """The loan as a writable 1-D `uint8` CPU tensor over the sample.

        `copy=True`, a device other than the CPU and a stream are refused.
        """
        ...

    def __dlpack_device__(self) -> tuple[int, int]: ...


class CastFromMessagePayloadBytes(Protocol[_MessageReadTarget_co]):
    """What `read(port, into=…)` takes: anything whose `from_bytes` builds the read's result.

    A class with a `from_bytes` classmethod is one. It is handed the read's
    `PortPayloadBorrow`, so it takes a `Buffer`.
    """

    def from_bytes(self, payload: PortPayloadBorrow, /) -> _MessageReadTarget_co:
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

    def adopt_the_stream_clock_board(self, service_name: str) -> None:
        """Read the stream's clock off the board the parent named, from now on.

        `ctx.time` and every write with no stamp then follow the stream's clock.
        Raises `RuntimeError` for a second board.
        """
        ...

    def listen_for_each_stream_clock_board_publish(self, service_name: str) -> None:
        """Wake on each publish of the stream clock board the parent named.

        How a node in a stepped stream learns a step opened. Raises
        `RuntimeError` for a second board.
        """
        ...

    def stream_clock_board_publish_listener_fd(self) -> int | None:
        """The fd that turns readable once the stream clock is published, if
        this node listens for it."""
        ...

    def take_every_stream_clock_board_publish_noted(self) -> None:
        """Clear the stream clock listener's pending wake-ups."""
        ...

    def note_a_step_callback_started(self) -> None:
        """Note on the loss-count board that a callback started; the stream's
        step cannot close until it returns."""
        ...

    def note_a_step_callback_returned(
        self, finished_step_index: int | None = None
    ) -> None:
        """Note on the loss-count board that a callback returned, finishing step
        `finished_step_index` when one is named."""
        ...

    def open_loss_count_board(
        self,
        service_name: str,
        output_port_names: list[str],
        function_node_states: list[tuple[int, list[int] | None]] = ...,
        function_node_frame_interval_ns: int | None = None,
    ) -> None:
        """Open the loss-count board the parent created for this spawn.

        Every link wired afterwards mirrors its losses onto it as they are
        counted — an input link onto the slot its wiring names, an output port
        onto its entry under its channel's generation — which is how the parent
        renders this helper's `metrics` in `graph`. `output_port_names` is the
        board's output-port section in key order. `function_node_states` names
        each stateful function node's entry by breadcrumb number, beside the
        state it resumes from as f64 bit patterns, or `None` for its state
        record's defaults; each call's new state is written there. With
        `function_node_frame_interval_ns`, each function-node call is timed
        against that interval, and the first node to overrun it three calls
        running is named on the board for its parent. Raises `RuntimeError`
        for a second board.
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
        input_port_borrow_quota: int | None = None,
    ) -> None:
        """Open this node's subscriber for one link into `port_name`.

        `channel_service_name` is what this end subscribes to;
        `inbound_link_name` is what a read hands back as the link's name.

        Once a loss-count board is open, `loss_count_slot` and
        `wiring_generation` name where the link's losses are mirrored, and
        omitting either raises `ValueError`; with no board they are unused.
        `input_port_borrow_quota` is what each link's reader may hold beyond the
        mailbox, the engine's default when `None`.
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
    ) -> PortPayloadBorrow | None: ...
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
    ) -> tuple[PortPayloadBorrow, int] | tuple[None, None]:
        """The next message's payload on `port_name` with its timestamp, or `(None, None)`."""
        ...

    def input_port_has_data(self, port_name: str) -> bool:
        """Whether `port_name` has a message waiting."""
        ...

    def write_to_output_port(
        self,
        port_name: str,
        payload: Buffer,
        timestamp_ns: int | None = None,
        starts_group: bool = True,
    ) -> None:
        """Publish a copy of `payload`, any contiguous buffer, to every downstream link on `port_name`."""
        ...

    def loan_on_output_port(
        self,
        port_name: str,
        payload_byte_len: int,
        timestamp_ns: int | None = None,
        starts_group: bool = True,
    ) -> PortPayloadLoan:
        """Loan `payload_byte_len` bytes on `port_name` to fill in place and publish on exit."""
        ...


class _RuntimeContextMembersBothCapabilitiesShare(Protocol):
    """The runtime-context members the limited and the full context both carry."""

    @property
    def config(self) -> dict[str, Any]:
        """The node's configuration."""
        ...

    @property
    def time(self) -> int:
        """Stream time now in nanoseconds, on the clock the node's stream runs on.

        In a real-time stream it is `monotonic_now_ns()`; in a scaled or stepped
        stream it is the stream's own time, in the same monotonic epoch.
        """
        ...

    @property
    def stream_clock(self) -> StreamClockReading:
        """Which clock the node's stream runs on and where it stands."""
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

    @property
    def node_name(self) -> str:
        """This node's name in its stream's graph, the same on every run of the stream."""
        ...

    @property
    def stream_id(self) -> str:
        """The id of the stream this node runs in."""
        ...

    @property
    def stream_project_directory(self) -> str:
        """The project directory of the stream this node runs in."""
        ...

    def asset_content_path(self, asset: str, from_project: str | None = None) -> str:
        """Where the bytes of the asset with UUID `asset` sit: a file to open and read, never to write.

        The asset is the stream's project's own, or with `from_project` the asset of that
        dependency at its pin, as a config's `{"asset": ..., "from": ...}` names it.

        Raises `ValueError` for a string that is not a UUID, and `LookupError` when no such asset
        is held there or the project's content store lacks its bytes.
        """
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

    def open_module_host_table(
        self, *, output_ports: Sequence[str] = (), input_ports: Sequence[str] = ()
    ) -> object:
        """Lend `output_ports` and `input_ports` to the node's native code.

        Returns the module host table as a capsule named
        `tatolab.runtime.module_host_table`, which the native half reads with
        `tatolab-module-sdk` and reaches those ports through from its own
        threads, without the GIL. From here on a Python write to a lent output
        port, or a read of a lent input port, is refused by name. A node that
        lends an input port declares `execution="manual"`.
        """
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

    A port name is cast the way `@node.process` casts it — lowercased, accents dropped,
    anything outside a-z 0-9 - . _ ~ turned into `-` — so any spelling that
    casts to a declared port finds it; one casting to nothing raises `ValueError`.
    """

    @overload
    def read(
        self, port: Input[_PortFamilyReadType], *, into: None = None
    ) -> _PortFamilyReadType | None: ...
    @overload
    def read(
        self, port: str, *, into: None = None
    ) -> PortPayloadBorrow | None: ...
    @overload
    def read(
        self,
        port: str | Input[Any],
        *,
        into: CastFromMessagePayloadBytes[_MessageReadTarget],
    ) -> _MessageReadTarget | None: ...
    def read(
        self,
        port: str | Input[Any],
        *,
        into: CastFromMessagePayloadBytes[Any] | None = None,
    ) -> Any:
        """The next message's payload on `port`, or `None` when none is waiting.

        Read through an `Input` attribute — `read(self.audio)` — the payload is
        the port family's type, built from the message's descriptor:
        `ImageFrame`, `ArrayFrame`, `ChannelsBlock`, `PacketsUnit`, or a `dict`
        for Records. Read by name, it is a `PortPayloadBorrow` of the sample it
        arrived in. With
        `into`, the borrow is handed to `into.from_bytes` and its result
        returned, so a payload that does not fit raises here, at the consuming
        read.
        """
        ...

    @overload
    def read_with_timestamp(
        self, port: Input[_PortFamilyReadType]
    ) -> tuple[_PortFamilyReadType, int] | tuple[None, None]: ...
    @overload
    def read_with_timestamp(
        self, port: str
    ) -> tuple[PortPayloadBorrow, int] | tuple[None, None]: ...
    def read_with_timestamp(self, port: str | Input[Any]) -> Any:
        """The next message's payload on `port` with its timestamp, or `(None, None)`.

        The payload is typed as `read` types it.
        """
        ...

    @overload
    def read_from_inbound_link(
        self, port: Input[_PortFamilyReadType], *, into: None = None
    ) -> tuple[_PortFamilyReadType, str] | None: ...
    @overload
    def read_from_inbound_link(
        self, port: str, *, into: None = None
    ) -> tuple[PortPayloadBorrow, str] | None: ...
    @overload
    def read_from_inbound_link(
        self,
        port: str | Input[Any],
        *,
        into: CastFromMessagePayloadBytes[_MessageReadTarget],
    ) -> tuple[_MessageReadTarget, str] | None: ...
    def read_from_inbound_link(
        self,
        port: str | Input[Any],
        *,
        into: CastFromMessagePayloadBytes[Any] | None = None,
    ) -> Any:
        """The next message's payload on `port` with the link it arrived on, or `None`.

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
        self, port: Input[_PortFamilyReadType], *, into: None = None
    ) -> tuple[_PortFamilyReadType, str, int] | None: ...
    @overload
    def read_from_inbound_link_with_timestamp(
        self, port: str, *, into: None = None
    ) -> tuple[PortPayloadBorrow, str, int] | None: ...
    @overload
    def read_from_inbound_link_with_timestamp(
        self,
        port: str | Input[Any],
        *,
        into: CastFromMessagePayloadBytes[_MessageReadTarget],
    ) -> tuple[_MessageReadTarget, str, int] | None: ...
    def read_from_inbound_link_with_timestamp(
        self,
        port: str | Input[Any],
        *,
        into: CastFromMessagePayloadBytes[Any] | None = None,
    ) -> Any:
        """The next message's payload on `port` with its link and its timestamp.

        The stamp is the one the producer wrote — the source frame's instant,
        not the moment of the read.
        """
        ...

    @overload
    def read_message(
        self, port: Input[_PortFamilyReadType], *, into: None = None
    ) -> Message[_PortFamilyReadType] | None: ...
    @overload
    def read_message(
        self, port: str, *, into: None = None
    ) -> Message[PortPayloadBorrow] | None: ...
    @overload
    def read_message(
        self,
        port: str | Input[Any],
        *,
        into: CastFromMessagePayloadBytes[_MessageReadTarget],
    ) -> Message[_MessageReadTarget] | None: ...
    def read_message(
        self,
        port: str | Input[Any],
        *,
        into: CastFromMessagePayloadBytes[Any] | None = None,
    ) -> Any:
        """The next message on `port` with its stamp, group flag, losses and link, or `None`.

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

    def upstream_stream_facts(
        self, port_name: str, *, inbound_link: str | None = None
    ) -> PortStreamFacts | None:
        """The stream facts the output port feeding `port_name` carries now.

        `None` when nothing feeds the port or the upstream port states no
        facts. A port fed by more than one link names the one to read with
        `inbound_link`, the name a read reports it under; without it, such a
        port raises naming its links. Nothing checks a payload against them.
        """
        ...


@runtime_backed_protocol
class LinkOutputDataWriter(Protocol):
    """A node's output ports, as `ctx.outputs`.

    A port name is cast the way `@node.process` casts it — lowercased, accents dropped,
    anything outside a-z 0-9 - . _ ~ turned into `-` — so any spelling that
    casts to a declared port finds it; one casting to nothing raises `ValueError`.
    """

    @overload
    def write(
        self,
        port: str,
        payload: Buffer,
        timestamp_ns: int | None = None,
        starts_group: bool = True,
    ) -> None: ...
    @overload
    def write(
        self,
        port: Output[Any],
        payload: Any,
        timestamp_ns: int | None = None,
        starts_group: bool = True,
        *,
        start_sample: int | None = None,
    ) -> None: ...
    def write(
        self,
        port: str | Output[Any],
        payload: Any,
        timestamp_ns: int | None = None,
        starts_group: bool = True,
        *,
        start_sample: int | None = None,
    ) -> None:
        """Publish one message carrying a copy of `payload` to every downstream link on `port`.

        Written through an `Output` attribute, `payload` is the family's object
        and the runtime builds its descriptor: an Image write takes an
        `ImageFrame` this node read, an `ImageFrameToWrite`, or a texture it
        acquired from a pool, whose colour is then unspecified, an
        Array write a mapping of name to a C-contiguous array or a tensor
        surface, a Channels write a `[channel][sample]` array with
        `start_sample`, a Packets write the unit's bytes, and a Records write a
        `dict` of at most 64 KiB carrying every declared name.

        Written by name:

        `payload` is any contiguous buffer — `bytes`, a `memoryview`, a numpy
        array. Omitting `timestamp_ns` stamps stream time now.
        `starts_group=True` says the message stands alone; only encoded media
        passes `False`, for a message a decoder cannot enter at. A payload over
        the channel's ceiling is dropped and counted against this port, never
        raised.
        """
        ...

    def write_channels_block(
        self,
        port_name: str,
        samples: Buffer,
        *,
        start_sample: int,
        sample_rate: int,
        timestamp_ns: int | None = None,
    ) -> None:
        """Publish `samples`, a C-contiguous `[channel][sample]` float32 or float64 array, as one Channels block.

        For a Channels port whose rate comes from config, which its class
        cannot state.
        """
        ...

    def loan(
        self,
        port_name: str,
        payload_byte_len: int,
        timestamp_ns: int | None = None,
        starts_group: bool = True,
    ) -> PortPayloadLoan:
        """Loan `payload_byte_len` bytes on `port_name` to fill in place: `with ctx.outputs.loan(...) as out:`.

        The sample is published when the block exits cleanly, stamped
        `timestamp_ns` or stream time now, and given back when it raises. A
        write or a second loan on the port is refused while this one is open,
        so a port's order is its write order. A port no link has reached, or a
        payload over the channel's ceiling, loans bytes that publish nowhere.
        """
        ...

    def has_a_reader_the_last_write_missed(self, port: str | Output[Any]) -> bool:
        """Whether a reader now connected to `port` connected after its last write, so that write never reached it.

        A channel keeps no history, so a node that writes only on change
        writes again for such a reader. Every reader counts: a link's
        destination once its subscriber is open, a viewer, an island. A port
        no link has reached has no reader.
        """
        ...

    def set_stream_facts(self, port_name: str, stream_facts: PortStreamFacts) -> None:
        """Replace the stream facts `port_name` carries — what `graph` renders
        under the port and what a downstream reader reads off its inbound link.

        Facts of another family than the port's raise naming the port and both
        families, and the port keeps the facts it had.
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
    surface against producer reuse is what `ClaimedSurfacePixelAccess` does with
    it. Any class reachable through `into=` may call this; there is no
    registration, no marker and no privileged type.
    """
    ...
