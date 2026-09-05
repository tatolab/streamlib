# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The native half of `streamlib-moq`, as the wheel's Python sees it.

Hand-written and gated against the compiled module by stubtest, the same way
the engine wheel's own stub is. The engine never calls anything here — a
processor extension's per-frame work is its own package's Rust, reached from
its own Python.

Everything about a container format lives below this line: which boxes a
fragment carries, how a catalog is spelled, what a group boundary is. Python
hands over one bag's keys and reads one object's keys back.
"""

from collections.abc import Sequence
from typing import final

# pyo3 builds this from what the module registers, so the stub has to carry it
# too or stubtest reports the module as under-described.
__all__ = [
    "MoqBroadcastPublishingSession",
    "MoqBroadcastSubscribingSession",
    "ReceivedOpusPacket",
    "ReceivedVideoAccessUnit",
    "bring_up_the_transport_stack",
]

def bring_up_the_transport_stack() -> None:
    """Start the tokio runtime and install the TLS provider, once per process.

    What `extension.py:load` calls. Cheap and does no I/O.
    """

@final
class MoqBroadcastPublishingSession:
    """One broadcast: one QUIC connection, one namespace, one track per link."""

    def __new__(
        cls,
        relay_url: str,
        broadcast: str,
        container_format: str,
        delivery_deadline_ms: "int | None" = None,
    ) -> MoqBroadcastPublishingSession:
        """Construct without connecting — the first bag is what connects.

        `relay_url` carries the relay's auth token as its path: draft-16
        provisions relays per account and there is nowhere else to put it.

        `delivery_deadline_ms` is how old a bag may be, by its own monotonic
        stamp, and still be published. Absent is the shipped behaviour: every
        bag is written however late it is.
        """

    def declare_tracks(self, inbound_link_names: Sequence[str]) -> None:
        """Fix the broadcast's tracks, in the order their links were wired.

        The order is load-bearing on `cmaf`: a subscriber zips the catalog's
        entries against the init segment's tracks positionally, so a track's
        place here is its identity there.
        """

    def publish_video_access_unit(
        self,
        inbound_link_name: str,
        codec: str,
        annex_b_access_unit: bytes,
        is_sync_point: bool,
        group_index: int,
        sequence_index: int,
        width: int,
        height: int,
        color: "dict[str, str] | None",
        timestamp_ns: int,
    ) -> None:
        """Publish one access unit on the track that link owns.

        On `cmaf` a sync point is what cuts a new MoQ group, and it cuts one on
        every track at once — the reference publisher's rule, and what makes a
        group a GOP across audio and video alike.
        """

    def publish_audio_packet(
        self,
        inbound_link_name: str,
        opus_packet: bytes,
        is_sync_point: bool,
        group_index: int,
        sequence_index: int,
        sample_rate: int,
        channels: int,
        sample_count: int,
        pre_skip: int,
        timestamp_ns: int,
    ) -> None:
        """Publish one Opus packet on the track that link owns.

        On `cmaf`, 3–8 channels are refused by name: the `dOps` box encodes
        ChannelMappingFamily 0 only, so a multichannel stream has no honest
        representation there. `streamlib_bag` carries it.
        """

    def close(self) -> "str | None":
        """Finish every open group and drop the connection, bounded.

        Hands back what a broadcast that never became playable threw away, or
        `None` when nothing was held. The caller logs it: this wheel's Rust
        reaches no `tracing` dispatcher inside a helper process, so a loss
        reported only there is reported to nobody.
        """

    @property
    def is_connected(self) -> bool: ...
    def objects_the_delivery_deadline_shed(self) -> "list[tuple[str, int, int]]":
        """What the deadline has shed so far: `(inbound_link_name, objects, bytes)`.

        A link that shed nothing is left out, so an empty list is a broadcast
        that dropped nothing. Read back rather than logged below the boundary:
        this wheel's Rust reaches no `tracing` dispatcher inside a helper
        process.
        """

@final
class MoqBroadcastSubscribingSession:
    """One subscription: the named tracks of one broadcast, draining into a
    queue."""

    def __new__(
        cls,
        relay_url: str,
        broadcast: str,
        container_format: str,
        video_track: "str | None" = None,
        audio_track: "str | None" = None,
    ) -> MoqBroadcastSubscribingSession: ...
    def connect(self) -> None:
        """Connect and begin draining. Called from the processor's own thread."""

    def next_media(
        self, timeout_ms: int
    ) -> "ReceivedVideoAccessUnit | ReceivedOpusPacket | None":
        """The next object, or `None` if none arrived in `timeout_ms`.

        The timeout is what lets a reading thread notice it has been asked to
        stop without waiting on a broadcast that may never send again. Raises
        once the broadcast has ended or the connection is gone — which is the
        only signal there is, since a dead QUIC connection unblocks no reader
        on its own.
        """

    def close(self) -> None:
        """Stop draining and drop the connection."""

    @property
    def is_connected(self) -> bool: ...

@final
class ReceivedVideoAccessUnit:
    """One access unit off a MoQ broadcast, described by the stream itself."""

    @property
    def codec(self) -> str:
        """`"h264"` or `"h265"`, as the producer named it."""

    @property
    def bitstream(self) -> bytes:
        """The Annex-B access unit, start codes included.

        On `cmaf` the parameter sets live in the init segment rather than in
        the samples, so they are put back in front of every sync point here —
        a bag's `bitstream` must stand on its own.
        """

    @property
    def is_sync_point(self) -> bool: ...
    @property
    def group_index(self) -> int:
        """The producer's own group index on `streamlib_bag`; this
        subscriber's own count of sync points on `cmaf`, which carries no
        such field."""

    @property
    def sequence_index(self) -> int:
        """The producer's own publication index on `streamlib_bag`; this
        subscriber's own count on `cmaf`. Never the MoQ object id, which is a
        per-subgroup counter the transport does not even carry across the
        wire."""

    @property
    def width(self) -> int:
        """Coded width — the codec-aligned extent before the conformance crop,
        which is what the encoded-video wire contract means by `width`."""

    @property
    def height(self) -> int: ...
    @property
    def timestamp_ns(self) -> int:
        """Monotonic. The producer's own stamp on `streamlib_bag`; on `cmaf`,
        the fragment's decode time placed against this subscriber's clock."""

    @property
    def color(self) -> "dict[str, str] | None":
        """The bag's `color` sub-map, or `None` where none was described.

        Carried through on `streamlib_bag`. Always `None` on `cmaf`, which
        keeps colour in the bitstream's VUI rather than beside it.
        """

@final
class ReceivedOpusPacket:
    """One Opus packet off a MoQ broadcast, described by the stream itself."""

    @property
    def bitstream(self) -> bytes: ...
    @property
    def is_sync_point(self) -> bool:
        """Always `true` — a decoder enters an Opus stream at any packet."""

    @property
    def group_index(self) -> int: ...
    @property
    def sequence_index(self) -> int: ...
    @property
    def sample_rate(self) -> int: ...
    @property
    def channels(self) -> int: ...
    @property
    def sample_count(self) -> int: ...
    @property
    def pre_skip(self) -> int:
        """The encoder lookahead a decoder trims. Carried through on
        `streamlib_bag`; read back from the `dOps` box on `cmaf`."""

    @property
    def timestamp_ns(self) -> int: ...
