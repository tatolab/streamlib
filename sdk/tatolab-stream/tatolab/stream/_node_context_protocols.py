# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The contexts and link endpoints a node is handed while it runs.

Each is a `typing.Protocol`: the runtime's native class is the only
implementation, held to it member for member by the wheel's conformance gate.
"""

from __future__ import annotations

from collections.abc import Buffer, Sequence
from types import TracebackType
from typing import TYPE_CHECKING, Any, Literal, Protocol

from ._runtime_lend import runtime_backed_function, runtime_backed_protocol

if TYPE_CHECKING:
    from .clock import StreamClockReading
    from ._gpu_protocols import GpuContextFullAccess, GpuContextLimitedAccess
    from .stream_facts import PortStreamFacts
    from ._message import Message



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
    """A message of any family a node hands on without reading it.

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
class Audio(Protocol):
    """A stream of audio samples, as an Audio pin's read returns a block of it: planar samples
    borrowed in the sample they arrived in.

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

    @property
    def pin_type(self) -> Literal["audio"]:
        """The pin type an audio block is the read of: `"audio"`."""
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
class TensorCpuAttribute(Protocol):
    """One CPU attribute of a Tensor a read returned: a row-major tensor borrowed in the sample.

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
class EncodedMedia(Protocol):
    """A unit of encoded media a read returned: its bytes borrowed in the sample, with its stamp and codec.

    A read of an `EncodedVideo` pin returns it as an `EncodedVideo`, and of an `EncodedAudio`
    pin as an `EncodedAudio`.
    """

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

    @property
    def pin_type(self) -> Literal["encoded_video", "encoded_audio"]:
        """The pin type the unit is the read of: `"encoded_audio"` for a unit of an audio codec, `"encoded_video"` for any other."""
        ...


class EncodedVideo(EncodedMedia, Protocol):
    """Encoded video, one access unit per message, as an `EncodedVideo` pin's read returns it."""

    @property
    def pin_type(self) -> Literal["encoded_video"]:
        """The pin type an access unit is the read of: `"encoded_video"`."""
        ...


class EncodedAudio(EncodedMedia, Protocol):
    """Encoded audio, one packet per message, as an `EncodedAudio` pin's read returns it."""

    @property
    def pin_type(self) -> Literal["encoded_audio"]:
        """The pin type a packet is the read of: `"encoded_audio"`."""
        ...


@runtime_backed_protocol
class PortPayloadLoan(Protocol):
    """A sample loaned on an output port for its writer to fill in place.

    `with pin.loan(byte_len) as out:` hands out a writable buffer
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


@runtime_backed_protocol
class NodeLinkDataAccess(Protocol):
    """One node's links. The engine binds it; app code never builds one."""

    def declare_ports(
        self, input_port_names: Sequence[str], output_port_names: Sequence[str]
    ) -> None:
        """Name the class's ports ahead of any wiring.

        A declared port no link has reached reads as empty and sends nothing
        written to it; only an undeclared name is refused.
        """
        ...

    def keep_the_last_message_of_output_ports(self, output_port_names: Sequence[str]) -> None:
        """Have each named output port keep the last message written to it.

        The message is kept whether or not a link reaches the port, and sent
        again to a reader that connects after it. The parent names the ports
        whose pins flow as a Value.
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
        member_taken_at_the_read: dict[str, Any] | None = None,
        member_put_at_the_read: dict[str, Any] | None = None,
    ) -> None:
        """Open this node's subscriber for one link into `port_name`.

        `channel_service_name` is what this end subscribes to;
        `inbound_link_name` is what a read hands back as the link's name.

        Once a loss-count board is open, `loss_count_slot` and
        `wiring_generation` name where the link's losses are mirrored, and
        omitting either raises `ValueError`; with no board they are unused.
        `input_port_borrow_quota` is what each link's reader may hold beyond the
        mailbox, the engine's default when `None`. `member_taken_at_the_read`
        names the member of a bundle a link that starts at one takes from each
        message, and `member_put_at_the_read` the member of the input's bundle a
        link that ends at one puts each value into.
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

    def read_from_input_port(self, port_name: str) -> PortPayloadBorrow | None:
        """The next message's payload on `port_name` as the bytes it was sent, or `None`."""
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


@runtime_backed_protocol
class StoreScopedToThisNode(Protocol):
    """The values this node keeps in its project's store, by key, as `ctx.store()`.

    A value outlives the run and a restart of the runtime.
    """

    def read(self, key: str) -> Any | None:
        """The value kept under `key`, or `None` when the node keeps none."""
        ...

    def write(self, key: str, value: Any) -> None:
        """Keep `value`, anything JSON can hold, under `key`, replacing what was there."""
        ...

    def remove(self, key: str) -> None:
        """Forget the value kept under `key`; forgetting an absent key does nothing."""
        ...


class _RuntimeContextMembersBothCapabilitiesShare(Protocol):
    """The runtime-context members the limited and the full context both carry."""

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
    def gpu_limited_access(self) -> GpuContextLimitedAccess:
        """The non-allocating GPU capability."""
        ...

    def store(self) -> StoreScopedToThisNode:
        """The values this node keeps in its project's store, by key."""
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

        The asset is the stream's project's own, or with `from_project` the asset of that import,
        its file inside the import's folder, as a config's `{"asset": ..., "from": ...}` names it.

        Raises `ValueError` for a string that is not a UUID, and `LookupError` when no such asset
        is held there, or no file is at its path.
        """
        ...

    def is_paused(self) -> bool:
        """Whether the node is paused."""
        ...

    def should_process(self) -> bool:
        """Whether the node should process now."""
        ...

    def report_not_ready(
        self, reason: str, *, image_width: int | None = None, image_height: int | None = None
    ) -> None:
        """Report the node running but not ready, in `reason`'s words — a setting left unset, a
        file that is missing — in place of raising.

        The stream keeps running and the graph shows `reason` on the node until `report_ready`.
        Meanwhile the runtime sends a transparent frame of `image_width` by `image_height`
        (1920 by 1080 when unnamed) on each Image output at the node's pace, Array, Channels and
        Records outputs send nothing, and `process()` is still called, so the node can report
        ready from it. Raises `ValueError` for an empty `reason` or a side of zero pixels.
        """
        ...

    def report_ready(self) -> None:
        """Report the node ready again after `report_not_ready`."""
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
    """The input ports of a node the wheel hosts, which its input pins read through.

    A port name is cast the way a node's pins cast it — lowercased, accents dropped,
    anything outside a-z 0-9 - . _ ~ turned into `-` — so any spelling that
    casts to a declared port finds it; one casting to nothing raises `ValueError`.
    `families` names, by wire name, the families a read takes: `"image"`, `"array"`,
    `"channels"`, `"packets"` or `"records"`, the first the one the port declares.
    """

    def read(self, port_name: str, *, families: Sequence[str] | None = None) -> Any:
        """The next message's payload on `port_name`, or `None` when none is waiting.

        Naming `families`, the payload is the type of the family it arrived as,
        built from the message's descriptor: `Texture`, `Tensor`,
        `Audio`, `EncodedVideo`, `EncodedAudio`, or a `dict` for Records. Naming none,
        it is a `PortPayloadBorrow` of the sample it arrived in.
        """
        ...

    def read_message(
        self, port_name: str, *, families: Sequence[str] | None = None
    ) -> Message[Any] | None:
        """The next message on `port_name` with its stamp, group flag, losses and link, or `None`.

        `lost_since_last_read` counts the messages its link lost since the
        previous message read from that link, so a reader that needs every
        message of a group — a decoder — discards until the next message whose
        `starts_group` is set. Its payload is read as `read` reads it. The link
        is the source channel the link subscribed to — `{source node id}/{source
        output port}`, the name `graph` shows — and the stamp the one its
        producer wrote. A message no link delivered is refused by name, and the
        refusal consumes it.
        """
        ...

    def count_the_message_last_read_as_lost(self, port_name: str) -> None:
        """Count the message last read from `port_name` as lost on the link that delivered it.

        For a message whose frame is gone by the time it is read — its producer
        recycled the frame before this node's claim. The loss lands in the link's dropped
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
    """The output ports of a node the wheel hosts, which its output pins write through.

    A port name is cast the way a node's pins cast it — lowercased, accents dropped,
    anything outside a-z 0-9 - . _ ~ turned into `-` — so any spelling that
    casts to a declared port finds it; one casting to nothing raises `ValueError`.
    """

    def write(
        self,
        port_name: str,
        payload: Any,
        timestamp_ns: int | None = None,
        starts_group: bool = True,
        *,
        family: str | None = None,
        codec: str | None = None,
        formats: Sequence[str] | None = None,
        names: Sequence[str] | None = None,
        sample_type: str | None = None,
        residency: str | None = None,
        start_sample: int | None = None,
        sample_rate: int | None = None,
    ) -> None:
        """Publish one message carrying a copy of `payload` to every downstream link on `port_name`.

        Naming `family` by its wire name, `payload` is the family's object and
        the runtime builds its descriptor: an Image write takes a `Texture`
        this node read, a `TextureToWrite`, or a texture it acquired from a
        pool, whose colour is then unspecified, in one of `formats` when any is
        named; an Array write a mapping of `names` to a C-contiguous array or a
        tensor surface, its tensors where `residency` (`"gpu"` or `"cpu"`) says;
        a Channels write a `[channel][sample]` array of `sample_type` (`"f32"`
        or `"f64"`) from `start_sample` at `sample_rate`; a Packets write the
        unit's bytes as `codec`; and a Records write a `dict` of at most 64 KiB
        carrying every one of `names`.

        Naming no family, `payload` is any contiguous buffer — `bytes`, a
        `memoryview`, a numpy array. A keyword the named family does not take,
        or any of them with no family, raises `TypeError` naming it.

        Omitting `timestamp_ns` stamps stream time now. `starts_group=True` says
        the message stands alone; only encoded media passes `False`, for a
        message a decoder cannot enter at. A payload over the channel's ceiling
        is dropped and counted against this port, never raised.
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
        """Loan `payload_byte_len` bytes on `port_name` to fill in place: `with pin.loan(...) as out:`.

        The sample is published when the block exits cleanly, stamped
        `timestamp_ns` or stream time now, and given back when it raises. A
        write or a second loan on the port is refused while this one is open,
        so a port's order is its write order. A port no link has reached, or a
        payload over the channel's ceiling, loans bytes that publish nowhere.
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
    """The GPU capability of the read currently building a family type, an
    `Texture` or an Array frame's GPU attribute, or `None` when no read is.

    The same capability as `ctx.gpu_limited_access`, offered so the type can do
    per-frame work at construction that needs the engine — claiming the frame's
    surface against producer reuse is what `ClaimedSurfacePixelAccess` does with
    it.
    """
    ...
