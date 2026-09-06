# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two processors this wheel supplies.

Both sit on the encoded side of the codec blocks and touch no raw frame, no
surface and no GPU: `MoqBroadcastPublisher` consumes what `H264Encoder` and
`OpusEncoder` publish — and any other bag at all, as a data track beside them —
and `MoqBroadcastSubscriber` emits what `H264Decoder` and `OpusDecoder`
consume. Each runs in its own helper process, and each calls this wheel's own
Rust directly — the engine is never on the data path.
"""

from __future__ import annotations

import threading
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any, Literal, Protocol

from streamlib import (
    EncodedAudioPacket,
    EncodedVideoFrame,
    LinkOutputDataWriter,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    decode_msgpack_bytes_to_python_object,
    encode_bag_to_msgpack_bytes,
    input,
    log,
    output,
    processor,
)

from . import _native

TRACKS_INPUT_PORT = "tracks"
ENCODED_VIDEO_OUTPUT_PORT = "encoded_video"
ENCODED_AUDIO_OUTPUT_PORT = "encoded_audio"
DATA_BAGS_OUTPUT_PORT = "data_bags"

#: What a broadcast's objects are, and what its catalog declares them to be.
#:
#: `"cmaf"` is the default because interop is the point: it is the layout
#: `moq-pub` publishes, so `moq-js` and `moq-sub` can play the broadcast.
#: `"streamlib_bag"` carries the bag's own keys instead, which is the only way
#: the producer's ordering pair, stamp and multichannel Opus survive a hop
#: byte-exact — CMAF turns each of those into something else.
ContainerFormat = Literal["cmaf", "streamlib_bag"]
CONTAINER_FORMATS: "tuple[str, ...]" = ("cmaf", "streamlib_bag")

#: How long the subscriber's thread waits for an object before looking at
#: whether it has been asked to stop. Short enough that `stop()` returns well
#: inside the helper's five-second teardown budget.
SUBSCRIBER_POLL_TIMEOUT_MS = 200

#: How long `stop()` waits for the reading thread. The thread's own wait for an
#: object is bounded, so this only expires when it is still inside `connect()`.
READER_THREAD_JOIN_TIMEOUT_SECONDS = 1.0

#: The reconnect backoff, as the old MoQ path had it. It lives here rather than
#: in the session's Rust because a helper process has no `tracing` subscriber:
#: a retry loop down there would fail invisibly, and the operator would see a
#: subscriber that simply never produced.
FIRST_RECONNECT_DELAY_SECONDS = 0.5
LONGEST_RECONNECT_DELAY_SECONDS = 10.0

#: How often a publisher and a subscriber say they are still working. The
#: engine's own built-ins report on the same cadence.
BAGS_BETWEEN_PROGRESS_REPORTS = 300

#: A helper-placed link's per-bag ceiling (`streamlib-ipc-types`' untrusted
#: session ceiling). A bag past it is dropped at `debug` by the engine rather
#: than raised, which would look from here like a stream that simply stopped —
#: so the subscriber says so itself, once.
#:
#: The engine exports no constant for this, so nothing can check the two agree;
#: a change on the engine's side reaches this wheel as a warning at the wrong
#: size rather than as a failing test.
HELPER_LINK_PAYLOAD_CEILING_BYTES = 16 * 1024 * 1024

#: What the engine writes in front of every bag on a link — the port key, the
#: stamp and the payload length — and charges against the ceiling with it.
#: `streamlib-ipc-types`' `FRAME_HEADER_SIZE`, unexported for the same reason.
HELPER_LINK_FRAME_HEADER_BYTES = 76

#: How close to the ceiling a bag's `bitstream` alone must bring it before the
#: framed size is measured exactly. The exact measure is an encode of the whole
#: bag — a copy of the bitstream on the reader thread, per bag — so it is paid
#: only where it could change the answer: an encoded-media bag's other keys are
#: a few hundred bytes, so a bitstream further under the ceiling than this
#: cannot put the framed bag over it.
BYTES_UNDER_THE_CEILING_WITHIN_WHICH_THE_FRAMED_SIZE_IS_MEASURED = 64 * 1024

#: The three keys of a data track's object. The user's bag rides whole under
#: `bag`, so no name inside it is reserved; the other two are the publisher's —
#: a per-track count and the bag's own link stamp — for the subscriber to count
#: gaps by and to stamp its write with.
DATA_OBJECT_SEQUENCE_INDEX_KEY = "sequence_index"
DATA_OBJECT_TIMESTAMP_NS_KEY = "timestamp_ns"
DATA_OBJECT_BAG_KEY = "bag"

#: The kind of track a bag with no `bitstream` key lands on. The media kinds
#: are the two media names, settled by the bag's `codec`.
DATA_TRACK_KIND = "data"

#: What each bag's `codec` maps to. A codec absent from here is refused by name
#: rather than guessed at.
_TRACK_MEDIUM_BY_CODEC: "dict[str, str]" = {
    "h264": "video",
    "h265": "video",
    "opus": "audio",
}


def _required_container_format(container_format: Any, processor_name: str) -> str:
    if container_format not in CONTAINER_FORMATS:
        raise ValueError(
            f"{processor_name}: `container_format` must be one of "
            f"{', '.join(CONTAINER_FORMATS)}; got {container_format!r}"
        )
    return container_format


def _optional_delivery_deadline_ms(delivery_deadline_ms: Any) -> "int | None":
    """The deadline a caller configured, refusing anything that is not one.

    `bool` is an `int` in Python, so it is refused by name rather than read as
    a zero- or one-millisecond deadline.
    """
    if delivery_deadline_ms is None:
        return None
    if isinstance(delivery_deadline_ms, bool) or not isinstance(
        delivery_deadline_ms, int
    ):
        raise ValueError(
            f"MoqBroadcastPublisher: `delivery_deadline_ms` is how many "
            f"milliseconds old a bag may be and still be published, or None to "
            f"publish every bag however late it is; got {delivery_deadline_ms!r}"
        )
    if delivery_deadline_ms < 0:
        raise ValueError(
            f"MoqBroadcastPublisher: `delivery_deadline_ms` cannot be negative; "
            f"got {delivery_deadline_ms!r}"
        )
    return delivery_deadline_ms


def describe_the_delivery_deadline(delivery_deadline_ms: "int | None") -> str:
    """The deadline this publisher runs under, as an operator reads it."""
    if delivery_deadline_ms is None:
        return "no delivery deadline is configured"
    return f"the delivery deadline is {delivery_deadline_ms} ms"


def describe_what_the_delivery_deadline_shed(
    shed_by_inbound_link: "Sequence[tuple[str, int, int]]",
) -> str:
    """What a publisher says about its drops, including that it made none.

    A silently shed frame is the failure mode this wheel is careful about, so a
    run that dropped nothing says so rather than saying nothing.
    """
    if not shed_by_inbound_link:
        return "the delivery deadline shed nothing"
    per_link = ", ".join(
        f"{inbound_link}={objects} objects/{byte_count} bytes"
        for inbound_link, objects, byte_count in shed_by_inbound_link
    )
    return f"the delivery deadline shed {per_link}"


class UplinkBacklogOnOneTrack(Protocol):
    """What the native session reports for one inbound link's uplink backlog."""

    @property
    def inbound_link_name(self) -> str: ...
    @property
    def unforwarded_objects(self) -> "int | None": ...
    @property
    def sheds_the_backlog_began(self) -> int: ...
    @property
    def groups_abandoned(self) -> int: ...
    @property
    def objects_abandoned(self) -> int: ...
    @property
    def bytes_abandoned(self) -> int: ...


class QuicUplinkReadings(Protocol):
    """What the native session reports about the QUIC path under it."""

    @property
    def round_trip_time_ms(self) -> float: ...
    @property
    def congestion_window_bytes(self) -> int: ...
    @property
    def lost_packets(self) -> int: ...
    @property
    def congestion_events(self) -> int: ...


def describe_the_uplink_backlog(
    backlog_by_inbound_link: Sequence[UplinkBacklogOnOneTrack],
    quic_readings: "QuicUplinkReadings | None",
) -> str:
    """What a publisher says about its uplink: the QUIC path as it stands, and
    per link what is unforwarded now and what the backlog has cost.

    Every link is named, a zero included — the backlog's absence is what an
    operator most wants to read — and a link nothing is forwarding says so
    rather than reading as caught up.
    """
    if quic_readings is None:
        return "the uplink is not connected"
    path = (
        f"the uplink reads rtt={quic_readings.round_trip_time_ms:.1f} ms "
        f"cwnd={quic_readings.congestion_window_bytes} bytes "
        f"lost_packets={quic_readings.lost_packets} "
        f"congestion_events={quic_readings.congestion_events}"
    )
    per_link = ", ".join(
        f"{track.inbound_link_name}: "
        + (
            "no forwarder"
            if track.unforwarded_objects is None
            else f"{track.unforwarded_objects} objects unforwarded"
        )
        + f", {track.sheds_the_backlog_began} sheds begun on the backlog, "
        f"{track.groups_abandoned} groups abandoned "
        f"({track.objects_abandoned} objects/{track.bytes_abandoned} bytes)"
        for track in backlog_by_inbound_link
    )
    return f"{path}; {per_link}" if per_link else path


def _required_relay_url(relay_url: Any, processor_name: str) -> str:
    if not isinstance(relay_url, str) or not relay_url:
        raise ValueError(
            f"{processor_name}: `relay_url` is required and must be the relay's "
            f"address. A Cloudflare draft-16 relay is provisioned per account and "
            f"carries its token in the path, so it reads "
            f"`https://draft-16.cloudflare.mediaoverquic.com/<token>`; got {relay_url!r}"
        )
    return relay_url


def track_medium_of_codec(codec: Any, inbound_link: str) -> str:
    """Which medium an encoded bag belongs to, refusing a codec this wheel does
    not carry.

    Named rather than guessed: a graph that wired something other than an
    encoder into a publisher is a wiring mistake, and the message is the only
    place it can be caught. A bag reaches here by carrying a `bitstream` key,
    so the refusal also says what that key means — a data bag that happened to
    spell one is refused as media, and its author renames the key.
    """
    medium = _TRACK_MEDIUM_BY_CODEC.get(codec) if isinstance(codec, str) else None
    if medium is None:
        raise ValueError(
            f"MoqBroadcastPublisher: a bag on `{inbound_link}` carries a "
            f"`bitstream` key, which marks encoded media, and names codec "
            f"{codec!r}, which this broadcast does not carry — it carries "
            f"{', '.join(sorted(_TRACK_MEDIUM_BY_CODEC))}. A data bag must not "
            f"spell a key `bitstream`."
        )
    return medium


def track_kind_of_bag(bag: Mapping[str, Any], inbound_link: str) -> str:
    """Which kind of track a bag belongs on: a medium by its `codec`, or data.

    `bitstream` is the encoded wire contract's defining key — both media bags
    require it — so a bag carrying one is media and takes the typed path with
    its refusals, and a bag without one is data, whatever else it carries.
    """
    if "bitstream" not in bag:
        return DATA_TRACK_KIND
    return track_medium_of_codec(bag.get("codec"), inbound_link)


def data_track_object_bytes(
    bag: Mapping[str, Any], sequence_index: int, timestamp_ns: int
) -> bytes:
    """One data track object, encoded with the engine's own bag codec.

    The user's bag is nested whole under `bag`, never flattened, so no name in
    it is reserved. The codec cannot refuse a bag that arrived over a link —
    the same codec encoded it on the way in — so nothing here is caught.
    """
    return encode_bag_to_msgpack_bytes(
        {
            DATA_OBJECT_SEQUENCE_INDEX_KEY: sequence_index,
            DATA_OBJECT_TIMESTAMP_NS_KEY: timestamp_ns,
            DATA_OBJECT_BAG_KEY: bag,
        }
    )


def framed_bag_byte_count(bag: Mapping[str, Any]) -> int:
    """What a helper link charges for one bag against its ceiling: the frame
    header plus the encoded bag — never the bitstream's length alone, which
    under-reports by the header and every other key."""
    return HELPER_LINK_FRAME_HEADER_BYTES + len(encode_bag_to_msgpack_bytes(bag))


def the_bitstream_alone_puts_the_bag_near_the_link_ceiling(bag: Mapping[str, Any]) -> bool:
    """Whether an encoded-media bag is close enough to the ceiling that only
    the exact framed size can say which side of it the bag falls."""
    bitstream = bag.get("bitstream")
    bitstream_byte_count = len(bitstream) if isinstance(bitstream, bytes) else 0
    return (
        HELPER_LINK_FRAME_HEADER_BYTES
        + bitstream_byte_count
        + BYTES_UNDER_THE_CEILING_WITHIN_WHICH_THE_FRAMED_SIZE_IS_MEASURED
        > HELPER_LINK_PAYLOAD_CEILING_BYTES
    )


#: The most a wire value grows when the engine's codec decodes and re-encodes
#: it. Two forms grow: an `f32` decodes to a Python float and re-encodes as an
#: `f64`, five bytes to nine; a `str` that is not UTF-8 decodes to `bytes` and
#: re-encodes as a `bin`, one length byte longer, two bytes to three at its
#: shortest. Nothing else does — the codec re-emits its ext passthrough map as
#: the ext it came from, and every other form it writes is already the
#: shortest. Nine fifths is what this has to clear.
WIDEST_THE_ENGINE_RE_ENCODES_A_WIRE_VALUE = 2


def the_envelope_alone_could_put_the_bag_past_the_link_ceiling(
    envelope_byte_count: int,
) -> bool:
    """Whether a data object is large enough that only the exact framed size
    can say which side of the ceiling its bag falls.

    The bag is not handed on as the bytes that arrived: the write decodes and
    re-encodes it through the engine's codec, which widens a value by at most
    `WIDEST_THE_ENGINE_RE_ENCODES_A_WIRE_VALUE`. So an envelope that fits
    under the ceiling with the frame header even at that widening carries a
    bag that does too, and no second encode is spent to say so.
    """
    return (
        HELPER_LINK_FRAME_HEADER_BYTES
        + envelope_byte_count * WIDEST_THE_ENGINE_RE_ENCODES_A_WIRE_VALUE
        > HELPER_LINK_PAYLOAD_CEILING_BYTES
    )


@dataclass(frozen=True)
class DataObjectEnvelope:
    """One data track object, decoded: the publisher's per-track count, the
    bag's own stamp, and the user's bag whole."""

    sequence_index: int
    timestamp_ns: int
    bag: "dict[str, Any]"


def data_object_envelope_of(payload: bytes) -> DataObjectEnvelope:
    """Decode one data object, refusing by name whatever is not the envelope.

    The shape is the change file's, not the publisher's code: the publisher
    writes it and this reads it, and the two meet only on the wire. A far end
    that wrote something else is refused with the key it left out or mistyped,
    so an operator can read which side drifted.
    """
    try:
        decoded = decode_msgpack_bytes_to_python_object(payload)
    except ValueError as failure:
        raise ValueError(
            f"the object is not msgpack the engine's codec can decode: {failure}"
        ) from failure
    if not isinstance(decoded, dict):
        raise ValueError(
            f"the object is a {type(decoded).__name__}, not the map of "
            f"`{DATA_OBJECT_SEQUENCE_INDEX_KEY}`, `{DATA_OBJECT_TIMESTAMP_NS_KEY}` "
            f"and `{DATA_OBJECT_BAG_KEY}` a data object is"
        )
    for key in (
        DATA_OBJECT_SEQUENCE_INDEX_KEY,
        DATA_OBJECT_TIMESTAMP_NS_KEY,
        DATA_OBJECT_BAG_KEY,
    ):
        if key not in decoded:
            raise ValueError(
                f"the object carries no `{key}` key; it carries "
                f"{', '.join(f'`{present}`' for present in decoded) or 'nothing'}"
            )
    sequence_index = decoded[DATA_OBJECT_SEQUENCE_INDEX_KEY]
    timestamp_ns = decoded[DATA_OBJECT_TIMESTAMP_NS_KEY]
    for key, value in (
        (DATA_OBJECT_SEQUENCE_INDEX_KEY, sequence_index),
        (DATA_OBJECT_TIMESTAMP_NS_KEY, timestamp_ns),
    ):
        # `bool` is an `int` in Python and a distinct type on the wire.
        if isinstance(value, bool) or not isinstance(value, int):
            raise ValueError(f"the object's `{key}` is {value!r}, not an int")
    bag = decoded[DATA_OBJECT_BAG_KEY]
    if not isinstance(bag, dict):
        raise ValueError(
            f"the object's `{DATA_OBJECT_BAG_KEY}` is a {type(bag).__name__}, not "
            f"the map a bag is"
        )
    unwritable_key = _the_first_key_the_engine_cannot_write_in(bag, DATA_OBJECT_BAG_KEY)
    if unwritable_key is not None:
        raise ValueError(
            f"the object's `{DATA_OBJECT_BAG_KEY}` carries {unwritable_key}; the "
            f"engine's codec writes a named map, so every key at every level "
            f"must be a str"
        )
    return DataObjectEnvelope(sequence_index, timestamp_ns, bag)


def _the_first_key_the_engine_cannot_write_in(bag: "dict[Any, Any]", path: str) -> "str | None":
    """Where a decoded bag first carries a key the engine's codec refuses at
    the write — an int, or a non-UTF-8 string the decoder hands back as
    `bytes` — or None when every key at every level is a `str`.

    Wire-legal msgpack a non-StreamLib publisher can send; refused here rather
    than at the write, where the codec's refusal would end the subscription
    over one object.
    """
    for key, value in bag.items():
        if not isinstance(key, str):
            return f"a key that is not a str at `{path}`: {key!r} ({type(key).__name__})"
        found = _the_first_key_the_engine_cannot_write_within(value, f"{path}.{key}")
        if found is not None:
            return found
    return None


def _the_first_key_the_engine_cannot_write_within(value: Any, path: str) -> "str | None":
    if isinstance(value, dict):
        return _the_first_key_the_engine_cannot_write_in(value, path)
    if isinstance(value, list):
        for index, element in enumerate(value):
            found = _the_first_key_the_engine_cannot_write_within(element, f"{path}[{index}]")
            if found is not None:
                return found
    return None


class DataTrackSequenceGapCount:
    """What a data track's `sequence_index` says was lost between the objects
    that arrived — kept for the log, because the engine offers no lossless link
    and a gap is said, never raised."""

    def __init__(self) -> None:
        self.gaps = 0
        self.objects_missed = 0
        self._last_sequence_index: "int | None" = None

    def account(self, sequence_index: int) -> None:
        last = self._last_sequence_index
        self._last_sequence_index = sequence_index
        # A first object has nothing to be a gap from. An index at or below the
        # last is a publisher that restarted its count, not a loss and not a
        # reorder: the session's drain hands a track's objects out in
        # publication order, finishing what arrived of an old group before a
        # new one replaces it, so a backward step never comes from the wire.
        if last is None or sequence_index <= last:
            return
        missed = sequence_index - last - 1
        if missed:
            self.gaps += 1
            self.objects_missed += missed

    def forget_the_closed_broadcasts_count(self) -> None:
        """A new session may be a new publisher, whose count starts at its
        own zero; what was lost before stays counted."""
        self._last_sequence_index = None

    def describe(self) -> str:
        return f"sequence_gaps={self.gaps} objects_missed={self.objects_missed}"


def _refuse_track_names_no_broadcast_can_serve(
    track_names_by_config: "Sequence[tuple[str, str | None]]",
) -> None:
    """Refuse an empty track name and one name given to two ports.

    The wheel's Rust refuses both too, but it is constructed on the reading
    thread, where a refusal is caught and retried with backoff — so a config
    mistake would read from outside as a subscriber that never connects. Said
    here, it fails the graph at `rt.add`.
    """
    named = [
        (config, name) for config, name in track_names_by_config if name is not None
    ]
    for config, name in named:
        if not isinstance(name, str) or not name:
            raise ValueError(
                f"MoqBroadcastSubscriber: `{config}` names a track on the relay, so it "
                f"must be a non-empty str; leave it unset to subscribe to none; got "
                f"{name!r}"
            )
    for index, (config, name) in enumerate(named):
        for other_config, other_name in named[index + 1 :]:
            if name == other_name:
                raise ValueError(
                    f"MoqBroadcastSubscriber: `{config}` and `{other_config}` are both "
                    f"{name!r}, and one track is one kind, so every object on it "
                    f"would be read twice under two different contracts."
                )


def _optional_track_names(track_names: Any) -> "list[str] | None":
    """The names an app chose for its tracks, or None for the links' own.

    Only the shape is settled here. Whether the count matches the links, and
    whether the container admits names at all, is refused by name at `setup()`,
    where the links are known.
    """
    if track_names is None:
        return None
    if isinstance(track_names, (str, bytes)) or not isinstance(track_names, Sequence):
        raise ValueError(
            f"MoqBroadcastPublisher: `track_names` is a sequence of one name per "
            f"inbound link, in wiring order — not a single name; got {track_names!r}"
        )
    names = list(track_names)
    for name in names:
        if not isinstance(name, str):
            raise ValueError(
                f"MoqBroadcastPublisher: `track_names` names each track with a "
                f"str; got {name!r}"
            )
    return names


@processor(
    description=(
        "Publishes encoded video, encoded audio and data bags to a MoQ "
        "broadcast, one track per inbound link"
    ),
)
class MoqBroadcastPublisher:
    """Bags in, one MoQ broadcast out.

    The `Mp4Sink` shape: one fan-in input, and each inbound link is one track
    whose kind the link's first bag settles — encoded media by its `codec`, or,
    for a bag with no `bitstream` key, data. A data track carries any bag at
    all, nested whole inside an object beside the publisher's own
    `sequence_index` and the bag's stamp, under `streamlib_bag` only. Its
    settings are ordinary constructor parameters — `relay_url`, `broadcast`,
    `container_format` and `track_names` — which is what
    `rt.add(MoqBroadcastPublisher, config={"relay_url": ...})` passes.

    `track_names`, under `streamlib_bag`, names the tracks positionally in
    wiring order — the order `runtime.connect` ran — so a subscriber in
    another node can name what it wants; absent, each track is its link's own
    channel name. Under `cmaf` the names are the interop contract and cannot
    be chosen.

    The session opens on the first bag rather than in `setup()`, because a
    relay round trip inside `setup()` spends the helper's start-up budget and a
    relay outage there takes the whole graph down with it.

    On `"cmaf"` the broadcast cannot be described until every track has been:
    the init segment carries one `moov` describing all of them and is published
    once. Bags arriving before the last track has spoken are held, under a byte
    budget the session refuses past rather than growing without bound.

    `delivery_deadline_ms` is how old a bag may be, by its own monotonic stamp,
    and still be published. A bag past it is never written and the rest of its
    group goes with it, because a decoder cannot use a frame whose reference
    was shed; the shed ends at the next sync point. So the unit of loss is the
    rest of a GOP, not a frame: one frame a millisecond late costs the video
    until the encoder's next IDR, and a stream that emits no further sync
    point stays shed until it ends — the deadline is only meaningful beside
    the encoder's keyframe interval. A sync point is published however late it
    is, which is what keeps audio out of the shed's reach — every Opus packet
    is one. The shed decides what is written; a superseded audio group the
    uplink is behind on is still abandoned at the cut, like video's.

    The deadline reads two things. The stamp ages on the way to this
    publisher — capture, encode, the link into the helper. The uplink backlog
    says how far the transport is behind: the wheel's vendored `moq-transport`
    keeps the forwarder's cursor where the writer can read it, so a frame on
    time by its own stamp is still shed while the forwarder is stuck on an
    object of its group older than the deadline, and a sync point's cut
    abandons the superseded group the uplink is behind on with a stream reset
    the relay sees, rather than finishing it. A data track is never abandoned.
    Both are counted per link and said at the progress cadence and at
    teardown, beside the QUIC path's round trip, congestion window and loss
    counters. Absent is the shipped behaviour and the baseline a measurement
    is read against: every bag is written however late it is, and no group is
    ever abandoned.

    Deadline or not, the session bounds its QUIC send window to 512 KiB — a
    few round trips of a 1080p stream — because a backlog the transport
    absorbs silently is one the publisher cannot read. That is a ceiling on
    throughput of about 40 Mbit/s at a 100 ms round trip to the relay.
    """

    def __init__(
        self,
        relay_url: str,
        broadcast: "str | None" = None,
        container_format: ContainerFormat = "cmaf",
        delivery_deadline_ms: "int | None" = None,
        track_names: "Sequence[str] | None" = None,
    ) -> None:
        self._relay_url = _required_relay_url(relay_url, "MoqBroadcastPublisher")
        self._broadcast = broadcast
        self._container_format = _required_container_format(
            container_format, "MoqBroadcastPublisher"
        )
        self._delivery_deadline_ms = _optional_delivery_deadline_ms(delivery_deadline_ms)
        self._track_names = _optional_track_names(track_names)
        self._session: "_native.MoqBroadcastPublishingSession | None" = None
        self._kind_by_inbound_link: "dict[str, str]" = {}
        self._next_data_sequence_index_by_inbound_link: "dict[str, int]" = {}
        self._bags_handed_over = 0
        self._bags_published = 0
        self._data_objects_published = 0

    @input(delivery_profile="ordered")
    def tracks(self) -> None:
        """Encoded video or audio bags, or any bag as data; each inbound link
        becomes one MoQ track."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Links are wired before setup() runs, so the track count and their
        # order are knowable here — and the order is load-bearing on `"cmaf"`,
        # where a subscriber zips the catalog's entries against the moov's
        # tracks positionally.
        inbound_links = ctx.inputs.inbound_link_names(TRACKS_INPUT_PORT)
        if not inbound_links:
            raise ValueError(
                "MoqBroadcastPublisher: nothing is connected to `tracks`, so "
                "there is no media to publish. Connect an H264Encoder or an "
                "OpusEncoder output to this port."
            )
        broadcast = self._broadcast or f"streamlib/{ctx.runtime_id}"
        self._session = _native.MoqBroadcastPublishingSession(
            self._relay_url,
            broadcast,
            self._container_format,
            self._delivery_deadline_ms,
        )
        self._session.declare_tracks(inbound_links, self._track_names)
        the_track_names_this_broadcast_was_given = (
            f"track_names={self._track_names} "
            if self._track_names is not None
            else ""
        )
        log.info(
            f"MoqBroadcastPublisher: broadcast={broadcast} "
            f"container_format={self._container_format} tracks={len(inbound_links)} "
            f"{the_track_names_this_broadcast_was_given}"
            f"{describe_the_delivery_deadline(self._delivery_deadline_ms)}"
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        read = ctx.inputs.read_from_inbound_link_with_timestamp(TRACKS_INPUT_PORT)
        if read is None:
            return
        bag, inbound_link, timestamp_ns = read

        kind = self._track_kind_of(bag, inbound_link)
        session = self._declared_session()
        if kind == DATA_TRACK_KIND:
            session.publish_data_object(
                inbound_link, self._next_data_object_bytes(inbound_link, bag, timestamp_ns)
            )
            self._record_one_bag(reaches_the_transport=True, is_a_data_object=True)
            return
        if kind == "video":
            frame = EncodedVideoFrame(**bag)
            reaches_the_transport = session.publish_video_access_unit(
                inbound_link,
                frame.codec,
                frame.annex_b_access_unit_bytes,
                frame.is_sync_point,
                frame.group_index,
                frame.sequence_index,
                frame.width,
                frame.height,
                _color_axes_of(frame),
                timestamp_ns,
            )
        else:
            packet = EncodedAudioPacket(**bag)
            reaches_the_transport = session.publish_audio_packet(
                inbound_link,
                packet.opus_packet_bytes,
                packet.is_sync_point,
                packet.group_index,
                packet.sequence_index,
                packet.sample_rate,
                packet.channels,
                packet.sample_count,
                packet.pre_skip,
                timestamp_ns,
            )
        self._record_one_bag(reaches_the_transport, is_a_data_object=False)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        shed = "the delivery deadline shed nothing"
        uplink = "the uplink is not connected"
        if self._session is not None:
            shed = self._what_the_delivery_deadline_shed()
            # Read before the close: an abandon's count survives it, but the
            # QUIC path's readings do not.
            uplink = self._describe_the_uplink_backlog()
            # The wheel's Rust reaches no `tracing` dispatcher in a helper, so
            # media it discarded is only on the record if it is said here.
            discarded = self._session.close()
            if discarded is not None:
                log.warn(f"MoqBroadcastPublisher: {discarded}")
            self._session = None
        log.info(
            f"MoqBroadcastPublisher: teardown, {self._describe_what_was_published()}, "
            f"{shed}, {uplink}"
        )

    def _record_one_bag(self, reaches_the_transport: bool, is_a_data_object: bool) -> None:
        # The cadence counts every bag handed over, or a run shedding
        # everything after its sync points would say nothing until teardown.
        self._bags_handed_over += 1
        if reaches_the_transport:
            self._bags_published += 1
            if is_a_data_object:
                self._data_objects_published += 1
            if self._bags_published == 1:
                # "Accepted", not "published": on `cmaf` the first bags are
                # held for the init segment and reach the transport with the
                # flush, and the running log must not claim a write the hold
                # has not made.
                log.info("MoqBroadcastPublisher: first bag accepted by the broadcast")
        if self._bags_handed_over % BAGS_BETWEEN_PROGRESS_REPORTS == 0:
            log.info(
                f"MoqBroadcastPublisher: {self._describe_what_was_published()}, "
                f"{self._what_the_delivery_deadline_shed()}, "
                f"{self._describe_the_uplink_backlog()}"
            )

    def _describe_what_was_published(self) -> str:
        return (
            f"bags_published={self._bags_published}, "
            f"data_objects_published={self._data_objects_published}"
        )

    def _next_data_object_bytes(
        self, inbound_link: str, bag: "Mapping[str, Any]", timestamp_ns: int
    ) -> bytes:
        # Spent before the publish rather than after: an object the transport
        # refused never reached the wire, and the subscriber's gap count is
        # the honest record of that.
        sequence_index = self._next_data_sequence_index_by_inbound_link.get(inbound_link, 0)
        self._next_data_sequence_index_by_inbound_link[inbound_link] = sequence_index + 1
        return data_track_object_bytes(bag, sequence_index, timestamp_ns)

    def _what_the_delivery_deadline_shed(self) -> str:
        return describe_what_the_delivery_deadline_shed(
            self._declared_session().objects_the_delivery_deadline_shed()
        )

    def _describe_the_uplink_backlog(self) -> str:
        session = self._declared_session()
        return describe_the_uplink_backlog(
            session.uplink_backlog_by_track(), session.quic_uplink_readings()
        )

    def _track_kind_of(self, bag: "Mapping[str, Any]", inbound_link: str) -> str:
        kind = track_kind_of_bag(bag, inbound_link)
        already = self._kind_by_inbound_link.get(inbound_link)
        if already is not None and already != kind:
            raise ValueError(
                f"MoqBroadcastPublisher: `{inbound_link}` published {already} "
                f"and is now publishing {kind}; one link is one track, and a "
                f"track does not change kind."
            )
        self._kind_by_inbound_link[inbound_link] = kind
        return kind

    def _declared_session(self) -> "_native.MoqBroadcastPublishingSession":
        if self._session is None:
            raise RuntimeError(
                "MoqBroadcastPublisher: process() ran before setup() declared "
                "the broadcast's tracks"
            )
        return self._session


def _color_axes_of(frame: EncodedVideoFrame) -> "dict[str, str] | None":
    """The bag's `color` sub-map as plain strings, or `None` for unspecified.

    Absent means unspecified, so a stream that described no colour carries no
    axes rather than a map of nulls.
    """
    color = frame.color
    if color is None:
        return None
    axes = {
        "primaries": color.primaries,
        "transfer": color.transfer,
        "matrix": color.matrix,
        "range": color.range,
    }
    stated = {name: value for name, value in axes.items() if value is not None}
    return stated or None


@processor(
    execution="manual",
    description=(
        "Plays encoded video, encoded audio and data bags back from a MoQ "
        "broadcast"
    ),
)
class MoqBroadcastSubscriber:
    """One MoQ broadcast in; encoded bags and data bags out.

    Three output ports rather than one per track: ports are declared
    statically, and a downstream wants a port it can name when the graph is
    wired. Which MoQ track feeds which port is config — `video_track`,
    `audio_track` and `data_track` — and a port whose track is unnamed simply
    never produces. One data track per subscriber; a second is a second
    subscriber, because a demux key written into the bag would be pollution.

    A data track's object is the publisher's envelope around a user's bag.
    `data_bags` carries that bag verbatim, stamped as its producer stamped it,
    and the envelope's `sequence_index` never enters it: it is what gaps are
    counted by, and the count is said through the log at the progress cadence
    rather than raised, because the engine offers no lossless link. Data rides
    `"streamlib_bag"` only, so `data_track` under `"cmaf"` is refused.

    On `"streamlib_bag"` every bag key crosses byte-exact, the producer's
    ordering pair and stamp included. On `"cmaf"` the container carries neither,
    so the pair is minted here and the stamp is the fragment's own decode time
    against this subscriber's monotonic clock — which is what a CMAF player
    would do with the same bytes.
    """

    def __init__(
        self,
        relay_url: str,
        broadcast: str,
        video_track: "str | None" = None,
        audio_track: "str | None" = None,
        container_format: ContainerFormat = "cmaf",
        data_track: "str | None" = None,
    ) -> None:
        self._relay_url = _required_relay_url(relay_url, "MoqBroadcastSubscriber")
        if not isinstance(broadcast, str) or not broadcast:
            raise ValueError(
                "MoqBroadcastSubscriber: `broadcast` is required and names the "
                f"namespace to subscribe to; got {broadcast!r}"
            )
        if video_track is None and audio_track is None and data_track is None:
            raise ValueError(
                "MoqBroadcastSubscriber: name at least one of `video_track`, "
                "`audio_track` and `data_track`; a subscriber naming none would "
                "subscribe to nothing and produce nothing."
            )
        _refuse_track_names_no_broadcast_can_serve(
            (("video_track", video_track), ("audio_track", audio_track), ("data_track", data_track))
        )
        self._container_format = _required_container_format(
            container_format, "MoqBroadcastSubscriber"
        )
        if data_track is not None and self._container_format == "cmaf":
            raise ValueError(
                f"MoqBroadcastSubscriber: `data_track` names a data track "
                f"({data_track!r}), and the `cmaf` container has no packaging for "
                f"one; a data track rides `container_format=\"streamlib_bag\"` only."
            )
        self._broadcast = broadcast
        self._video_track = video_track
        self._audio_track = audio_track
        self._data_track = data_track
        self._stop = threading.Event()
        self._reader: "threading.Thread | None" = None
        self._reported_an_oversized_bag = False
        self._bags_written: "dict[str, int]" = {}
        self._data_sequence_gaps = DataTrackSequenceGapCount()
        self._data_objects_refused = 0

    @output()
    def encoded_video(self) -> None:
        """H.264 or H.265 access units, as `EncodedVideoFrame` bags."""

    @output()
    def encoded_audio(self) -> None:
        """Opus packets, as `EncodedAudioPacket` bags."""

    @output()
    def data_bags(self) -> None:
        """The data track's bags, each exactly as its producer wrote it and
        stamped as its producer stamped it."""

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        """Hand the outputs to a thread this processor owns.

        Connecting happens on that thread and not in `setup()`, so a relay that
        is slow or down cannot spend the helper's start-up budget.
        """
        outputs = ctx.outputs
        self._reader = threading.Thread(
            target=lambda: self._subscribe_until_stopped(outputs), daemon=True
        )
        self._reader.start()

    def _subscribe_until_stopped(self, outputs: LinkOutputDataWriter) -> None:
        """Connect, drain, and reconnect for as long as this processor runs.

        The backoff is here rather than in the session's Rust because this is
        where a failure can still be said out loud: a helper process has no
        `tracing` subscriber, so a retry loop below the boundary would be
        silent.
        """
        delay_seconds = FIRST_RECONNECT_DELAY_SECONDS
        while not self._stop.is_set():
            session = None
            try:
                session = _native.MoqBroadcastSubscribingSession(
                    self._relay_url,
                    self._broadcast,
                    self._container_format,
                    self._video_track,
                    self._audio_track,
                    self._data_track,
                )
                session.connect()
                log.info(
                    f"MoqBroadcastSubscriber: subscribed to {self._broadcast}"
                )
                delay_seconds = FIRST_RECONNECT_DELAY_SECONDS
                self._data_sequence_gaps.forget_the_closed_broadcasts_count()
                self._drain_until_stopped(session, outputs)
            except Exception as failure:
                log.warn(
                    f"MoqBroadcastSubscriber: the subscription ended "
                    f"({failure}); retrying in {delay_seconds:.1f}s"
                )
            finally:
                if session is not None:
                    session.close()
            if self._stop.is_set():
                return
            # `wait` rather than `sleep`: a stop arriving mid-backoff should
            # end the thread now, not after the longest delay.
            self._stop.wait(delay_seconds)
            delay_seconds = min(delay_seconds * 2, LONGEST_RECONNECT_DELAY_SECONDS)

    def _drain_until_stopped(
        self, session: "_native.MoqBroadcastSubscribingSession", outputs: LinkOutputDataWriter
    ) -> None:
        while not self._stop.is_set():
            received = session.next_media(SUBSCRIBER_POLL_TIMEOUT_MS)
            if received is None:
                continue
            if isinstance(received, _native.ReceivedDataObject):
                self._write_a_data_object(received, outputs)
            else:
                self._write_a_media_bag(received, outputs)

    def _write_a_media_bag(
        self,
        media: "_native.ReceivedVideoAccessUnit | _native.ReceivedOpusPacket",
        outputs: LinkOutputDataWriter,
    ) -> None:
        port, bag = _bag_for(media)
        self._report_a_bag_the_link_will_drop(
            port, bag, the_bitstream_alone_puts_the_bag_near_the_link_ceiling(bag)
        )
        outputs.write(port, bag, timestamp_ns=media.timestamp_ns)
        self._report_progress(port)

    def _write_a_data_object(
        self, received: "_native.ReceivedDataObject", outputs: LinkOutputDataWriter
    ) -> None:
        """The user's bag verbatim under the producer's stamp — or nothing, for
        an object that is not the envelope, which is said and dropped."""
        # Read once: the getter copies the whole envelope on every read.
        payload = received.payload
        try:
            envelope = data_object_envelope_of(payload)
        except ValueError as refusal:
            self._report_a_refused_data_object(received.track_name, refusal)
            return
        self._data_sequence_gaps.account(envelope.sequence_index)
        self._report_a_bag_the_link_will_drop(
            DATA_BAGS_OUTPUT_PORT,
            envelope.bag,
            the_envelope_alone_could_put_the_bag_past_the_link_ceiling(len(payload)),
        )
        outputs.write(DATA_BAGS_OUTPUT_PORT, envelope.bag, timestamp_ns=envelope.timestamp_ns)
        self._report_progress(DATA_BAGS_OUTPUT_PORT)

    def _report_a_refused_data_object(self, track_name: str, refusal: ValueError) -> None:
        # The first in full, then at the cadence: a far end writing the wrong
        # shape writes it on every object, and a line per object would bury
        # the log of a stream that never recovers.
        self._data_objects_refused += 1
        if self._data_objects_refused == 1:
            log.warn(
                f"MoqBroadcastSubscriber: an object on data track `{track_name}` is "
                f"not a data object and was dropped: {refusal}. The next such "
                f"drops are counted, and the count is said at the progress cadence."
            )
        elif self._data_objects_refused % BAGS_BETWEEN_PROGRESS_REPORTS == 0:
            log.warn(
                f"MoqBroadcastSubscriber: objects_refused={self._data_objects_refused} "
                f"on data track `{track_name}`, the latest because {refusal}"
            )

    def _report_progress(self, port: str) -> None:
        written = self._bags_written.get(port, 0) + 1
        self._bags_written[port] = written
        if written == 1:
            log.info(f"MoqBroadcastSubscriber: first bag written on `{port}`")
        elif written % BAGS_BETWEEN_PROGRESS_REPORTS == 0:
            lost = (
                f", {self._describe_what_the_data_track_lost()}"
                if port == DATA_BAGS_OUTPUT_PORT
                else ""
            )
            log.info(f"MoqBroadcastSubscriber: `{port}` bags_written={written}{lost}")

    def _describe_what_the_data_track_lost(self) -> str:
        return (
            f"{self._data_sequence_gaps.describe()} "
            f"objects_refused={self._data_objects_refused} on `{self._data_track}`"
        )

    def _report_a_bag_the_link_will_drop(
        self,
        port: str,
        bag: "Mapping[str, Any]",
        the_cheap_bound_reaches_the_ceiling: bool,
    ) -> None:
        if self._reported_an_oversized_bag:
            return
        if not the_cheap_bound_reaches_the_ceiling:
            return
        framed_byte_count = framed_bag_byte_count(bag)
        if framed_byte_count <= HELPER_LINK_PAYLOAD_CEILING_BYTES:
            return
        self._reported_an_oversized_bag = True
        log.error(
            f"MoqBroadcastSubscriber: a bag on `{port}` is {framed_byte_count} "
            f"bytes framed — the link's header and the encoded bag — which is "
            f"past the {HELPER_LINK_PAYLOAD_CEILING_BYTES}-byte link ceiling, so "
            f"the link will drop it without reaching the decoder. Reported once."
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop.set()
        if self._reader is not None:
            # Bounded well inside the helper's five-second teardown budget: the
            # thread's own wait for an object is bounded at
            # SUBSCRIBER_POLL_TIMEOUT_MS, and its backoff waits on the same event.
            self._reader.join(timeout=READER_THREAD_JOIN_TIMEOUT_SECONDS)
            if self._reader.is_alive():
                log.warn(
                    "MoqBroadcastSubscriber: the reading thread is still connecting; "
                    "the session will close when that connect returns"
                )
            self._reader = None
        if self._data_track is not None:
            # The cadence line only fires on a written bag, so a run that lost
            # or refused more than it wrote would otherwise end unsaid.
            log.info(
                f"MoqBroadcastSubscriber: stop, `{DATA_BAGS_OUTPUT_PORT}` "
                f"bags_written={self._bags_written.get(DATA_BAGS_OUTPUT_PORT, 0)}, "
                f"{self._describe_what_the_data_track_lost()}"
            )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop.set()


class ReceivedAccessUnit(Protocol):
    """What spelling a video bag reads off one received access unit.

    Named as a protocol rather than the concrete `_native` class so the check
    survives: `ReceivedVideoAccessUnit` carries no Python constructor, so a
    test cannot build one, and typing the parameter `Any` to admit a stand-in
    would stop pyright checking the reads at the one call site that passes the
    real object.
    """

    @property
    def codec(self) -> str: ...
    @property
    def bitstream(self) -> bytes: ...
    @property
    def is_sync_point(self) -> bool: ...
    @property
    def group_index(self) -> int: ...
    @property
    def sequence_index(self) -> int: ...
    @property
    def width(self) -> int: ...
    @property
    def height(self) -> int: ...
    @property
    def color(self) -> "dict[str, str] | None": ...


class ReceivedPacket(Protocol):
    """What spelling an audio bag reads off one received Opus packet."""

    @property
    def bitstream(self) -> bytes: ...
    @property
    def is_sync_point(self) -> bool: ...
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
    def pre_skip(self) -> int: ...


def _bag_for(
    media: "_native.ReceivedVideoAccessUnit | _native.ReceivedOpusPacket",
) -> "tuple[str, dict[str, Any]]":
    """The port and the bag literal one received object is published on."""
    if isinstance(media, _native.ReceivedOpusPacket):
        return ENCODED_AUDIO_OUTPUT_PORT, encoded_audio_packet_bag(media)
    return ENCODED_VIDEO_OUTPUT_PORT, encoded_video_frame_bag(media)


def encoded_video_frame_bag(access_unit: ReceivedAccessUnit) -> "dict[str, Any]":
    """One access unit, spelled against the encoded-video wire contract.

    Spelled here rather than in Rust so the keys sit beside the cast that reads
    them back, and there is one spelling rather than two that can drift.
    """
    bag: "dict[str, Any]" = {
        "codec": access_unit.codec,
        "bitstream": access_unit.bitstream,
        "is_sync_point": access_unit.is_sync_point,
        "group_index": access_unit.group_index,
        "sequence_index": access_unit.sequence_index,
        "width": access_unit.width,
        "height": access_unit.height,
    }
    # Absent means unspecified, so a stream that described no colour carries no
    # `color` key rather than a map of nulls.
    if access_unit.color is not None:
        bag["color"] = access_unit.color
    return bag


def encoded_audio_packet_bag(packet: ReceivedPacket) -> "dict[str, Any]":
    """One Opus packet, spelled against the encoded-audio wire contract."""
    return {
        "codec": "opus",
        "bitstream": packet.bitstream,
        "is_sync_point": packet.is_sync_point,
        "group_index": packet.group_index,
        "sequence_index": packet.sequence_index,
        "sample_rate": packet.sample_rate,
        "channels": packet.channels,
        "sample_count": packet.sample_count,
        "pre_skip": packet.pre_skip,
    }
