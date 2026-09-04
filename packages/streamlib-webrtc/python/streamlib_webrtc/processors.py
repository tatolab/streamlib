# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two processors this wheel supplies.

Both sit on the encoded side of the codec blocks and touch no raw frame, no
surface and no GPU: `WhipPublisher` consumes what `H264Encoder` and
`OpusEncoder` publish, and `WhepPlayer` emits what `H264Decoder` and
`OpusDecoder` consume. Each runs in its own helper process, and each calls this
wheel's own Rust directly — the engine is never on the data path.
"""

from __future__ import annotations

import threading
from collections.abc import Mapping
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

#: One WHIP session carries at most one video track and one audio track, so a
#: publisher's inbound links cannot outnumber them.
HIGHEST_TRACKS_IN_ONE_SESSION = 2

#: How long the player's thread waits for a bag before looking at whether it has
#: been asked to stop. Short enough that `stop()` returns well inside the
#: helper's five-second teardown budget.
PLAYER_POLL_TIMEOUT_MS = 200

#: How long `stop()` waits for the reading thread. The thread's own media wait
#: is bounded, so this only expires when it is still inside `connect()`.
READER_THREAD_JOIN_TIMEOUT_SECONDS = 1.0

#: How often a publisher and a player say they are still working. The engine's
#: own built-ins report on the same cadence, and this wheel's Rust cannot: its
#: `tracing` has no subscriber in a helper process, so anything a session wants
#: understood has to be said from Python.
BAGS_BETWEEN_PROGRESS_REPORTS = 300

#: A helper-placed link's per-bag ceiling
#: (`streamlib-ipc-types`' untrusted-session ceiling). A bag past it is dropped
#: at `debug` by the engine rather than raised, which would look from here like
#: a stream that simply stopped — so the player says so itself, once. Three
#: orders of magnitude above an H.264 access unit at any sane bitrate, so this
#: firing at all means something upstream is wrong.
HELPER_LINK_PAYLOAD_CEILING_BYTES = 16 * 1024 * 1024

VideoOrAudio = Literal["video", "audio"]


class ReceivedAccessUnit(Protocol):
    """What spelling a video bag reads off one received access unit."""

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


#: What each bag's `codec` maps to. A codec absent from here is refused by name
#: rather than guessed at — this session offers H.264 and Opus and nothing else.
_TRACK_KIND_BY_CODEC: "dict[str, VideoOrAudio]" = {"h264": "video", "opus": "audio"}


def resolve_track_kind(
    codec: Any,
    inbound_link: str,
    kind_by_inbound_link: "Mapping[str, VideoOrAudio]",
) -> VideoOrAudio:
    """Which track a bag belongs on, given what the links have already claimed.

    A link's medium is fixed by its first bag and every refusal names the links
    it is about, because a graph with two encoders wired into one publisher is
    a wiring mistake and the message is the only place it can be caught.
    """
    kind = _TRACK_KIND_BY_CODEC.get(codec) if isinstance(codec, str) else None
    if kind is None:
        raise ValueError(
            f"WhipPublisher: a bag on `{inbound_link}` names codec {codec!r}, "
            f"which this session does not carry — it offers "
            f"{', '.join(sorted(_TRACK_KIND_BY_CODEC))}."
        )

    already = kind_by_inbound_link.get(inbound_link)
    if already is not None:
        if already != kind:
            raise ValueError(
                f"WhipPublisher: `{inbound_link}` published {already} and is "
                f"now publishing {kind}; one link is one RTP track, and a track "
                f"does not change medium."
            )
        return kind

    claimed_by = next(
        (
            link
            for link, claimed in kind_by_inbound_link.items()
            if claimed == kind
        ),
        None,
    )
    if claimed_by is not None:
        raise ValueError(
            f"WhipPublisher: `{inbound_link}` and `{claimed_by}` are both "
            f"publishing {kind}, but one WHIP session carries one {kind} track. "
            f"Use one publisher per session."
        )
    return kind


#: RFC 7587 defines an RTP payload format for mono and stereo Opus only. The
#: engine's `OpusEncoder` legally produces mapping-family-1 multistream packets
#: for 3–8 channels, which no WHIP relay can carry.
HIGHEST_CHANNELS_RTP_CARRIES = 2


def refuse_audio_rtp_cannot_carry(channels: Any, inbound_link: str) -> None:
    """Refuse a multichannel Opus packet by name rather than send it anyway.

    `Mp4Sink` set the precedent: a track it cannot write honestly is refused,
    not written broken. Forwarded here, the far end would decode garbage and
    nothing upstream would say why.
    """
    if isinstance(channels, int) and channels > HIGHEST_CHANNELS_RTP_CARRIES:
        raise ValueError(
            f"WhipPublisher: a bag on `{inbound_link}` carries {channels}-channel "
            f"Opus, and RFC 7587 defines an RTP payload format for mono and "
            f"stereo only. Encode at {HIGHEST_CHANNELS_RTP_CARRIES} channels or "
            f"fewer to publish this stream."
        )


def _required_url(url: Any, processor_name: str) -> str:
    if not isinstance(url, str) or not url:
        raise ValueError(
            f"{processor_name}: `url` is required and must be the endpoint's "
            f"address; got {url!r}"
        )
    return url


def _optional_bearer_token(bearer_token: Any) -> "str | None":
    return bearer_token if isinstance(bearer_token, str) and bearer_token else None


@processor(
    description=(
        "Publishes encoded video and audio to a WHIP endpoint, "
        "one RTP track per inbound link"
    ),
)
class WhipPublisher:
    """Encoded bags in, one WHIP session out.

    The `Mp4Sink` shape: one fan-in input, and each inbound link is one track
    whose medium the link's first bag settles by its `codec`. Its settings are
    ordinary constructor parameters — `url`, and an optional `bearer_token` —
    which is what `rt.add(WhipPublisher, config={"url": ...})` passes.

    The session opens on the first bag rather than in `setup()`, because a
    relay round trip inside `setup()` spends the helper's start-up budget and a
    relay outage there takes the whole graph down with it.

    One known offset: video reaches the far end presenting roughly one frame
    interval later than audio. The RTP payloader applies a sample's duration to
    the frame *after* it, so a publisher that cannot see the next frame without
    delaying this one numbers video one frame behind real time; audio takes its
    duration from each packet's own sample count and is exact. At 30 fps that
    is about 33 ms of video lag, inside the ITU-R BT.1359 comfort bound but not
    zero.
    """

    def __init__(self, url: str, bearer_token: "str | None" = None) -> None:
        self._url = _required_url(url, "WhipPublisher")
        self._bearer_token = _optional_bearer_token(bearer_token)
        self._session: "_native.WhipSession | None" = None
        self._inbound_links: "list[str]" = []
        self._kind_by_inbound_link: "dict[str, VideoOrAudio]" = {}
        self._bags_published = 0

    @input(delivery_profile="ordered")
    def tracks(self) -> None:
        """Encoded video or audio bags; each inbound link becomes one track."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Links are wired before setup() runs, so the track count is knowable
        # here — before a bag has arrived, and before anything is offered.
        self._inbound_links = ctx.inputs.inbound_link_names(TRACKS_INPUT_PORT)
        if not self._inbound_links:
            raise ValueError(
                "WhipPublisher: nothing is connected to `tracks`, so there is "
                "no media to publish. Connect an H264Encoder or an OpusEncoder "
                "output to this port."
            )
        if len(self._inbound_links) > HIGHEST_TRACKS_IN_ONE_SESSION:
            raise ValueError(
                f"WhipPublisher: {len(self._inbound_links)} links feed `tracks` "
                f"({', '.join(sorted(self._inbound_links))}), but one WHIP "
                f"session carries at most one video and one audio track. Use "
                f"one publisher per session."
            )

        self._session = _native.WhipSession(self._url, self._bearer_token)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        read = ctx.inputs.read_from_inbound_link_with_timestamp(TRACKS_INPUT_PORT)
        if read is None:
            return
        bag, inbound_link, timestamp_ns = read

        kind = self._track_kind_of(bag, inbound_link)
        if kind == "audio":
            # Before the connect, not after: refusing a stream once a session
            # is already open leaves exactly the phantom publisher — live at
            # the relay, permanently silent — that refusing exists to prevent.
            refuse_audio_rtp_cannot_carry(bag.get("channels"), inbound_link)
        self._connect_on_the_first_bag(kind, bag)

        session = self._connected_session()
        if kind == "video":
            frame = EncodedVideoFrame(**bag)
            session.write_video_access_unit(
                frame.annex_b_access_unit_bytes, timestamp_ns
            )
        else:
            packet = EncodedAudioPacket(**bag)
            session.write_audio_packet(packet.opus_packet_bytes, packet.sample_count)
        self._report_progress()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        if self._session is not None:
            self._session.close()
            self._session = None
        log.info(f"WhipPublisher: teardown, bags_published={self._bags_published}")

    def _report_progress(self) -> None:
        self._bags_published += 1
        if self._bags_published == 1:
            log.info("WhipPublisher: first bag published to the session")
        elif self._bags_published % BAGS_BETWEEN_PROGRESS_REPORTS == 0:
            log.info(f"WhipPublisher: bags_published={self._bags_published}")

    def _track_kind_of(
        self, bag: "dict[str, Any]", inbound_link: str
    ) -> VideoOrAudio:
        kind = resolve_track_kind(
            bag.get("codec"), inbound_link, self._kind_by_inbound_link
        )
        self._kind_by_inbound_link[inbound_link] = kind
        return kind

    def _connect_on_the_first_bag(
        self, kind: VideoOrAudio, bag: "dict[str, Any]"
    ) -> None:
        """Open the session, offering the media the wiring settled.

        With one link the offer carries the medium that link just declared.
        With two it carries both, because a second link of either medium is
        already refused — so two links are one video and one audio by
        construction, whichever of them produced first.
        """
        session = self._connected_session()
        if session.is_connected:
            return

        carries_both = len(self._inbound_links) == HIGHEST_TRACKS_IN_ONE_SESSION
        audio_channels = bag.get("channels") if kind == "audio" else None
        video = carries_both or kind == "video"
        audio = carries_both or kind == "audio"
        log.info(
            f"WhipPublisher: opening a session on the first bag "
            f"(video={video}, audio={audio}, links={len(self._inbound_links)})"
        )
        session.connect(
            video=video,
            audio=audio,
            audio_channels=audio_channels if isinstance(audio_channels, int) else None,
        )
        log.info("WhipPublisher: the relay accepted the session")

    def _connected_session(self) -> "_native.WhipSession":
        if self._session is None:
            raise RuntimeError(
                "WhipPublisher: process() ran before setup() opened the session"
            )
        return self._session


@processor(
    execution="manual",
    description="Plays encoded video and audio back from a WHEP endpoint",
)
class WhepPlayer:
    """One WHEP session in, encoded bags out.

    Two output ports rather than one per track: ports are declared statically,
    and a decoder downstream wants a port it can name when the graph is wired.

    Every key each bag carries comes from the stream itself — the coded extent
    and the colour from the SPS, the ordering pair from this player's own
    counters, the sync point from the access unit, the Opus sample count from
    each packet's TOC byte, and the channel count from the answer's
    `sprop-stereo` (falling back to the first packet's TOC when the answer
    states no Opus fmtp), latched constant for the stream. `pre_skip` is 0
    because RTP carries no `OpusHead` to state an encoder lookahead, so a
    decoder trims nothing.
    """

    def __init__(self, url: str, bearer_token: "str | None" = None) -> None:
        self._url = _required_url(url, "WhepPlayer")
        self._bearer_token = _optional_bearer_token(bearer_token)
        self._session: "_native.WhepSession | None" = None
        self._stop = threading.Event()
        self._reader: "threading.Thread | None" = None
        self._reported_an_oversized_bag = False
        self._bags_written: "dict[str, int]" = {}

    @output()
    def encoded_video(self) -> None:
        """H.264 access units, as `EncodedVideoFrame` bags."""

    @output()
    def encoded_audio(self) -> None:
        """Opus packets, as `EncodedAudioPacket` bags."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._session = _native.WhepSession(self._url, self._bearer_token)

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        """Hand the outputs to a thread this processor owns.

        Connecting happens on that thread and not here, so a relay that is slow
        or down cannot spend the helper's start-up budget.
        """
        if self._session is None:
            raise RuntimeError("WhepPlayer: start() ran before setup()")
        session = self._session
        outputs = ctx.outputs

        def play_until_stopped() -> None:
            # This thread owns the session's whole life, close included. Only
            # one thread ever touches it, so a teardown arriving while the
            # connect is still outstanding waits for the thread rather than
            # contending with it.
            try:
                try:
                    log.info("WhepPlayer: opening the session")
                    session.connect()
                    log.info("WhepPlayer: the relay accepted the session")
                except Exception as connect_failure:
                    log.error(
                        f"WhepPlayer: the session did not open: {connect_failure}"
                    )
                    return
                self._drain_until_stopped(session, outputs)
            finally:
                session.close()

        self._reader = threading.Thread(target=play_until_stopped, daemon=True)
        self._reader.start()

    def _drain_until_stopped(
        self, session: "_native.WhepSession", outputs: LinkOutputDataWriter
    ) -> None:
        while not self._stop.is_set():
            media = session.next_media(PLAYER_POLL_TIMEOUT_MS)
            if media is None:
                continue
            port, bag = _bag_for(media)
            try:
                self._report_a_bag_the_link_will_drop(port, bag["bitstream"])
                outputs.write(port, bag, timestamp_ns=media.timestamp_ns)
            except Exception as write_failure:
                # This thread's death is otherwise silent from outside — the
                # session closes and the ports go quiet with nothing saying
                # which one failed or why.
                log.error(
                    f"WhepPlayer: writing a bag on `{port}` failed, and the "
                    f"reading thread is ending: {write_failure}"
                )
                raise
            self._report_progress(port)

    def _report_progress(self, port: str) -> None:
        written = self._bags_written.get(port, 0) + 1
        self._bags_written[port] = written
        if written == 1:
            log.info(f"WhepPlayer: first bag written on `{port}`")
        elif written % BAGS_BETWEEN_PROGRESS_REPORTS == 0:
            log.info(f"WhepPlayer: `{port}` bags_written={written}")

    def _report_a_bag_the_link_will_drop(self, port: str, bitstream: bytes) -> None:
        if len(bitstream) <= HELPER_LINK_PAYLOAD_CEILING_BYTES:
            return
        if self._reported_an_oversized_bag:
            return
        self._reported_an_oversized_bag = True
        log.error(
            f"WhepPlayer: a {len(bitstream)}-byte bag on `{port}` is past the "
            f"{HELPER_LINK_PAYLOAD_CEILING_BYTES}-byte link ceiling and will be "
            f"dropped without reaching the decoder. Reported once."
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop.set()
        if self._reader is None:
            return
        # Bounded well inside the helper's five-second teardown budget: the
        # thread's own wait for media is bounded at PLAYER_POLL_TIMEOUT_MS.
        # A thread still inside `connect()` against an unresponsive relay
        # outlives this, and closes the session itself when the connect
        # returns — which is the most a stop can honestly promise.
        self._reader.join(timeout=READER_THREAD_JOIN_TIMEOUT_SECONDS)
        if self._reader.is_alive():
            log.warn(
                "WhepPlayer: the reading thread is still connecting; the session "
                "will close when that connect returns"
            )
        self._reader = None

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._session = None


def _bag_for(
    media: "_native.PlayedVideoAccessUnit | _native.PlayedOpusPacket",
) -> "tuple[str, dict[str, Any]]":
    """The port and the bag literal one received item is published on."""
    if isinstance(media, _native.PlayedOpusPacket):
        return ENCODED_AUDIO_OUTPUT_PORT, encoded_audio_packet_bag(media)
    return ENCODED_VIDEO_OUTPUT_PORT, encoded_video_frame_bag(media)


def encoded_video_frame_bag(access_unit: "ReceivedAccessUnit") -> "dict[str, Any]":
    """One access unit, spelled against the encoded-video wire contract.

    Spelled here rather than in Rust so the keys sit beside the cast that reads
    them back, and there is one spelling rather than two that can drift.
    """
    bag: "dict[str, Any]" = {
        "codec": "h264",
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


def encoded_audio_packet_bag(packet: "ReceivedPacket") -> "dict[str, Any]":
    """One Opus packet, spelled against the encoded-audio wire contract."""
    return {
        "codec": "opus",
        "bitstream": packet.bitstream,
        # Every Opus packet is a sync point — a decoder enters the stream at
        # any of them — so each packet is its own group.
        "is_sync_point": True,
        "group_index": packet.group_index,
        "sequence_index": packet.sequence_index,
        "sample_rate": packet.sample_rate,
        "channels": packet.channels,
        "sample_count": packet.sample_count,
        "pre_skip": packet.pre_skip,
    }
