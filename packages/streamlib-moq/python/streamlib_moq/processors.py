# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two processors this wheel supplies.

Both sit on the encoded side of the codec blocks and touch no raw frame, no
surface and no GPU: `MoqBroadcastPublisher` consumes what `H264Encoder` and
`OpusEncoder` publish, and `MoqBroadcastSubscriber` emits what `H264Decoder`
and `OpusDecoder` consume. Each runs in its own helper process, and each calls
this wheel's own Rust directly — the engine is never on the data path.
"""

from __future__ import annotations

import threading
from collections.abc import Sequence
from typing import Any, Literal, Protocol

from streamlib import (
    EncodedAudioPacket,
    EncodedVideoFrame,
    LinkOutputDataWriter,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    input,
    log,
    output,
    processor,
)

from . import _native

TRACKS_INPUT_PORT = "tracks"
ENCODED_VIDEO_OUTPUT_PORT = "encoded_video"
ENCODED_AUDIO_OUTPUT_PORT = "encoded_audio"

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
    """Which medium a bag belongs to, refusing a codec this wheel does not carry.

    Named rather than guessed: a graph that wired something other than an
    encoder into a publisher is a wiring mistake, and the message is the only
    place it can be caught.
    """
    medium = _TRACK_MEDIUM_BY_CODEC.get(codec) if isinstance(codec, str) else None
    if medium is None:
        raise ValueError(
            f"MoqBroadcastPublisher: a bag on `{inbound_link}` names codec "
            f"{codec!r}, which this broadcast does not carry — it carries "
            f"{', '.join(sorted(_TRACK_MEDIUM_BY_CODEC))}."
        )
    return medium


@processor(
    description=(
        "Publishes encoded video and audio to a MoQ broadcast, "
        "one track per inbound link"
    ),
)
class MoqBroadcastPublisher:
    """Encoded bags in, one MoQ broadcast out.

    The `Mp4Sink` shape: one fan-in input, and each inbound link is one track
    whose medium the link's first bag settles by its `codec`. Its settings are
    ordinary constructor parameters — `relay_url`, `broadcast` and
    `container_format` — which is what
    `rt.add(MoqBroadcastPublisher, config={"relay_url": ...})` passes.

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
    is, which is what keeps audio out of the policy's reach — every Opus packet
    is one.

    The stamp ages on the way to this publisher — capture, encode, the link
    into the helper — and not on the way out: the transport's writer never
    blocks, so a congested uplink leaves the stamp untouched and this deadline
    does not fire on it. Absent is the shipped behaviour and the baseline a
    measurement is read against: every bag is written however late it is.
    """

    def __init__(
        self,
        relay_url: str,
        broadcast: "str | None" = None,
        container_format: ContainerFormat = "cmaf",
        delivery_deadline_ms: "int | None" = None,
    ) -> None:
        self._relay_url = _required_relay_url(relay_url, "MoqBroadcastPublisher")
        self._broadcast = broadcast
        self._container_format = _required_container_format(
            container_format, "MoqBroadcastPublisher"
        )
        self._delivery_deadline_ms = _optional_delivery_deadline_ms(delivery_deadline_ms)
        self._session: "_native.MoqBroadcastPublishingSession | None" = None
        self._medium_by_inbound_link: "dict[str, str]" = {}
        self._bags_handed_over = 0
        self._bags_published = 0

    @input(delivery_profile="ordered")
    def tracks(self) -> None:
        """Encoded video or audio bags; each inbound link becomes one MoQ track."""

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
        self._session.declare_tracks(inbound_links)
        log.info(
            f"MoqBroadcastPublisher: broadcast={broadcast} "
            f"container_format={self._container_format} tracks={len(inbound_links)} "
            f"{describe_the_delivery_deadline(self._delivery_deadline_ms)}"
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        read = ctx.inputs.read_from_inbound_link_with_timestamp(TRACKS_INPUT_PORT)
        if read is None:
            return
        bag, inbound_link, timestamp_ns = read

        medium = self._track_medium_of(bag, inbound_link)
        session = self._declared_session()
        if medium == "video":
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
        self._record_one_bag(reaches_the_transport)

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        shed = "the delivery deadline shed nothing"
        if self._session is not None:
            shed = self._what_the_delivery_deadline_shed()
            # The wheel's Rust reaches no `tracing` dispatcher in a helper, so
            # media it discarded is only on the record if it is said here.
            discarded = self._session.close()
            if discarded is not None:
                log.warn(f"MoqBroadcastPublisher: {discarded}")
            self._session = None
        log.info(
            f"MoqBroadcastPublisher: teardown, bags_published={self._bags_published}, "
            f"{shed}"
        )

    def _record_one_bag(self, reaches_the_transport: bool) -> None:
        # The cadence counts every bag handed over, or a run shedding
        # everything after its sync points would say nothing until teardown.
        self._bags_handed_over += 1
        if reaches_the_transport:
            self._bags_published += 1
            if self._bags_published == 1:
                # "Accepted", not "published": on `cmaf` the first bags are
                # held for the init segment and reach the transport with the
                # flush, and the running log must not claim a write the hold
                # has not made.
                log.info("MoqBroadcastPublisher: first bag accepted by the broadcast")
        if self._bags_handed_over % BAGS_BETWEEN_PROGRESS_REPORTS == 0:
            log.info(
                f"MoqBroadcastPublisher: bags_published={self._bags_published}, "
                f"{self._what_the_delivery_deadline_shed()}"
            )

    def _what_the_delivery_deadline_shed(self) -> str:
        return describe_what_the_delivery_deadline_shed(
            self._declared_session().objects_the_delivery_deadline_shed()
        )

    def _track_medium_of(self, bag: "dict[str, Any]", inbound_link: str) -> str:
        medium = track_medium_of_codec(bag.get("codec"), inbound_link)
        already = self._medium_by_inbound_link.get(inbound_link)
        if already is not None and already != medium:
            raise ValueError(
                f"MoqBroadcastPublisher: `{inbound_link}` published {already} "
                f"and is now publishing {medium}; one link is one track, and a "
                f"track does not change medium."
            )
        self._medium_by_inbound_link[inbound_link] = medium
        return medium

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
    description="Plays encoded video and audio back from a MoQ broadcast",
)
class MoqBroadcastSubscriber:
    """One MoQ broadcast in, encoded bags out.

    Two output ports rather than one per track: ports are declared statically,
    and a decoder downstream wants a port it can name when the graph is wired.
    Which MoQ track feeds which port is config — `video_track` and
    `audio_track` — and a port whose track is unnamed simply never produces.

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
    ) -> None:
        self._relay_url = _required_relay_url(relay_url, "MoqBroadcastSubscriber")
        if not isinstance(broadcast, str) or not broadcast:
            raise ValueError(
                "MoqBroadcastSubscriber: `broadcast` is required and names the "
                f"namespace to subscribe to; got {broadcast!r}"
            )
        if video_track is None and audio_track is None:
            raise ValueError(
                "MoqBroadcastSubscriber: name at least one of `video_track` and "
                "`audio_track`; a subscriber with neither would subscribe to "
                "nothing and produce nothing."
            )
        self._broadcast = broadcast
        self._video_track = video_track
        self._audio_track = audio_track
        self._container_format = _required_container_format(
            container_format, "MoqBroadcastSubscriber"
        )
        self._stop = threading.Event()
        self._reader: "threading.Thread | None" = None
        self._reported_an_oversized_bag = False
        self._bags_written: "dict[str, int]" = {}

    @output()
    def encoded_video(self) -> None:
        """H.264 or H.265 access units, as `EncodedVideoFrame` bags."""

    @output()
    def encoded_audio(self) -> None:
        """Opus packets, as `EncodedAudioPacket` bags."""

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
                )
                session.connect()
                log.info(
                    f"MoqBroadcastSubscriber: subscribed to {self._broadcast}"
                )
                delay_seconds = FIRST_RECONNECT_DELAY_SECONDS
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
            media = session.next_media(SUBSCRIBER_POLL_TIMEOUT_MS)
            if media is None:
                continue
            port, bag = _bag_for(media)
            self._report_a_bag_the_link_will_drop(port, bag["bitstream"])
            outputs.write(port, bag, timestamp_ns=media.timestamp_ns)
            self._report_progress(port)

    def _report_progress(self, port: str) -> None:
        written = self._bags_written.get(port, 0) + 1
        self._bags_written[port] = written
        if written == 1:
            log.info(f"MoqBroadcastSubscriber: first bag written on `{port}`")
        elif written % BAGS_BETWEEN_PROGRESS_REPORTS == 0:
            log.info(f"MoqBroadcastSubscriber: `{port}` bags_written={written}")

    def _report_a_bag_the_link_will_drop(self, port: str, bitstream: bytes) -> None:
        if len(bitstream) <= HELPER_LINK_PAYLOAD_CEILING_BYTES:
            return
        if self._reported_an_oversized_bag:
            return
        self._reported_an_oversized_bag = True
        log.error(
            f"MoqBroadcastSubscriber: a {len(bitstream)}-byte bag on `{port}` is "
            f"past the {HELPER_LINK_PAYLOAD_CEILING_BYTES}-byte link ceiling and "
            f"will be dropped without reaching the decoder. Reported once."
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop.set()
        if self._reader is None:
            return
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
