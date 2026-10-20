# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two processors this wheel supplies.

Both sit on the encoded side of the codec blocks and touch no raw frame, no
surface and no GPU: `WhipPublisher` consumes the media frames `H264Encoder` and
`OpusEncoder` publish, and `WhepPlayer` writes the media frames `H264Decoder`
and `OpusDecoder` consume — hang's legacy container, a QUIC varint timestamp in
microseconds and then the Annex-B access unit or Opus packet. The codec, the
coded extent and the channel count ride the ports' catalog entries. Each runs
in its own helper process, and each calls this wheel's own Rust directly — the
engine is never on the data path.
"""

from __future__ import annotations

import dataclasses
import threading
from collections.abc import Mapping
from typing import Annotated, Any, Literal, NamedTuple, Protocol

from tatolab.stream import (
    LinkInputDataReader,
    LinkOutputDataWriter,
    Message,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)
from tatolab.stream.catalog import AudioEntry, CatalogEntry, VideoEntry

from . import _native

TRACKS_INPUT_PORT = "tracks"
ENCODED_VIDEO_OUTPUT_PORT = "encoded_video"
ENCODED_AUDIO_OUTPUT_PORT = "encoded_audio"

#: One WHIP session carries at most one video track and one audio track, so a
#: publisher's inbound links cannot outnumber them.
HIGHEST_TRACKS_IN_ONE_SESSION = 2

#: How long the player's thread waits for media before looking at whether it has
#: been asked to stop. Short enough that `stop()` returns well inside the
#: helper's five-second teardown budget.
PLAYER_POLL_TIMEOUT_MS = 200

#: How long `stop()` waits for the reading thread. The thread's own media wait
#: is bounded, so this only expires when it is still inside `connect()`.
READER_THREAD_JOIN_TIMEOUT_SECONDS = 1.0

#: The reconnect backoff. A WHEP endpoint answers `409 Conflict` while the live
#: input it fronts has not started publishing yet, which is the ordinary state
#: of a player brought up beside its publisher rather than after it — so a
#: single failed connect cannot be the end of the stream. It lives here rather
#: than in the session's Rust because a helper process has no `tracing`
#: subscriber: a retry loop below the boundary would fail invisibly, and the
#: operator would see a player that simply never produced.
FIRST_RECONNECT_DELAY_SECONDS = 0.5
LONGEST_RECONNECT_DELAY_SECONDS = 10.0

#: How often a publisher and a player say they are still working. The engine's
#: own built-ins report on the same cadence, and this wheel's Rust cannot: its
#: `tracing` has no subscriber in a helper process, so anything a session wants
#: understood has to be said from Python.
MEDIA_FRAMES_BETWEEN_PROGRESS_REPORTS = 300

#: A helper-placed link's per-message ceiling
#: (`streamlib-ipc-types`' untrusted-session ceiling). A message past it is
#: dropped at `debug` by the engine rather than raised, which would look from
#: here like a stream that simply stopped — so the player says so itself, once.
#: Three orders of magnitude above an H.264 access unit at any sane bitrate, so
#: this firing at all means something upstream is wrong.
HELPER_LINK_PAYLOAD_CEILING_BYTES = 16 * 1024 * 1024

#: The only rate an Opus RTP stream is clocked at (RFC 7587 §4.1).
OPUS_RTP_SAMPLE_RATE_HZ = 48_000

#: The WebCodecs codec-string prefixes of H.264: `avc1` with parameter sets out
#: of band, `avc3` inline. RTP carries the access unit either way.
H264_CODEC_STRING_PREFIXES = ("avc1.", "avc3.")
OPUS_CODEC_STRING = "opus"

VideoOrAudio = Literal["video", "audio"]


class LinkOutputWriteFailure(Exception):
    """A media frame or catalog entry this player wrote was refused, carrying
    the port that refused it.

    Distinguishes the one failure a reconnect cannot fix from every failure it
    can: the endpoint is reachable and what was written is wrong, so the port
    has to reach the log and the retry loop has to stop.
    """

    def __init__(self, port: str) -> None:
        super().__init__(f"writing to `{port}` failed")
        self.port = port


class ReceivedAccessUnit(Protocol):
    """What the player reads off one received access unit."""

    @property
    def media_frame(self) -> bytes: ...
    @property
    def is_sync_point(self) -> bool: ...
    @property
    def codec(self) -> str: ...
    @property
    def coded_width(self) -> int: ...
    @property
    def coded_height(self) -> int: ...
    @property
    def timestamp_ns(self) -> int: ...


class ReceivedPacket(Protocol):
    """What the player reads off one received Opus packet."""

    @property
    def media_frame(self) -> bytes: ...
    @property
    def channels(self) -> int: ...
    @property
    def opus_identification_header_hex(self) -> str: ...
    @property
    def timestamp_ns(self) -> int: ...


class PublishedTrack(NamedTuple):
    """The RTP track one inbound link publishes as, read off its catalog entry."""

    kind: VideoOrAudio
    channels: "int | None"


def published_track_from_catalog_entry(
    catalog_entry: "Mapping[str, Any] | None", inbound_link: str
) -> PublishedTrack:
    """The track `inbound_link`'s upstream catalog entry describes, refused by
    name when it describes nothing this session carries.

    The entry is `{"section": ..., "entry": {...}}` as
    `inbound_link_catalog_entry` returns it. A codec absent from what this
    session offers is refused rather than guessed at.
    """
    if catalog_entry is None:
        raise ValueError(
            f"WhipPublisher: `{inbound_link}` carries no catalog entry, and each "
            f"track's codec is read from it. Connect an H264Encoder or an "
            f"OpusEncoder output, or a node that states its port's entry."
        )
    section = catalog_entry.get("section")
    fields = catalog_entry.get("entry") or {}
    codec = fields.get("codec")
    if (
        section == "video"
        and isinstance(codec, str)
        and codec.startswith(H264_CODEC_STRING_PREFIXES)
    ):
        return PublishedTrack(kind="video", channels=None)
    if section == "audio" and codec == OPUS_CODEC_STRING:
        channels = fields.get("numberOfChannels")
        return PublishedTrack(
            kind="audio", channels=channels if isinstance(channels, int) else None
        )
    raise ValueError(
        f"WhipPublisher: `{inbound_link}`'s catalog entry names {section} codec "
        f"{codec!r}, which this session does not carry — it offers H.264 "
        f"({' or '.join(prefix.rstrip('.') for prefix in H264_CODEC_STRING_PREFIXES)}) "
        f"video and {OPUS_CODEC_STRING} audio."
    )


def resolve_track_kind(
    kind: VideoOrAudio,
    inbound_link: str,
    kind_by_inbound_link: "Mapping[str, VideoOrAudio]",
) -> VideoOrAudio:
    """`kind`, once checked against the media the other links already claimed.

    Every refusal names the links it is about, because a graph with two
    encoders of one medium wired into one publisher is a wiring mistake and
    the message is the only place it can be caught.
    """
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


def refuse_audio_rtp_cannot_carry(channels: "int | None", inbound_link: str) -> None:
    """Refuse a multichannel Opus stream by name rather than send it anyway.

    Forwarded, the far end would decode garbage and nothing upstream would say
    why.
    """
    if channels is not None and channels > HIGHEST_CHANNELS_RTP_CARRIES:
        raise ValueError(
            f"WhipPublisher: `{inbound_link}` carries {channels}-channel "
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


@dataclasses.dataclass
class WhipPublisherConfig:
    """What a `WhipPublisher` is configured with."""

    url: Annotated[str, "The WHIP endpoint to publish to."]
    bearer_token: Annotated[
        "str | None", "Sent as `Authorization: Bearer` when the endpoint wants one."
    ] = None


@node(
    description=(
        "Publishes encoded video and audio to a WHIP endpoint, "
        "one RTP track per inbound link"
    ),
)
class WhipPublisher:
    """Media frames in, one WHIP session out.

    One fan-in input, and each inbound link is one track whose medium the
    upstream port's catalog entry names. Its settings are a
    `WhipPublisherConfig` — `url`, and an optional `bearer_token` — which is
    what `rt.add(WhipPublisher, config={"url": ...})` is constructed into.

    Each link enters at a message that starts a group — a keyframe, or any
    Opus packet — and after a read reports a loss it discards up to the next
    one, so the far end is never sent a picture whose references it lacks.

    The session opens on the first media frame rather than in `setup()`,
    because a relay round trip inside `setup()` spends the helper's start-up
    budget and a relay outage there takes the whole graph down with it.

    One known offset: video reaches the far end presenting roughly one frame
    interval later than audio. The RTP payloader applies a sample's duration to
    the frame *after* it, so a publisher that cannot see the next frame without
    delaying this one numbers video one frame behind real time; audio takes its
    duration from each packet's own sample count and is exact. At 30 fps that
    is about 33 ms of video lag, inside the ITU-R BT.1359 comfort bound but not
    zero.
    """

    def __init__(self, config: WhipPublisherConfig) -> None:
        self._url = _required_url(config.url, "WhipPublisher")
        self._bearer_token = _optional_bearer_token(config.bearer_token)
        self._session: "_native.WhipSession | None" = None
        self._inbound_links: "list[str]" = []
        self._track_by_inbound_link: "dict[str, PublishedTrack]" = {}
        self._inbound_links_awaiting_a_group_start: "set[str]" = set()
        self._media_frames_published = 0
        self._media_frames_discarded_awaiting_a_group_start = 0

    @node.input(delivery_profile="ordered")
    def tracks(self) -> None:
        """H.264 or Opus media frames; each inbound link becomes one track."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Links are wired before setup() runs, so the track count is knowable
        # here — before a message has arrived, and before anything is offered.
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
        self._inbound_links_awaiting_a_group_start = set(self._inbound_links)

        self._session = _native.WhipSession(self._url, self._bearer_token)

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        message = ctx.inputs.read_message(TRACKS_INPUT_PORT)
        if message is None:
            return

        track = self._track_of(ctx.inputs, message.inbound_link)
        if track.kind == "audio":
            # Before the connect, not after: refusing a stream once a session
            # is already open leaves exactly the phantom publisher — live at
            # the relay, permanently silent — that refusing exists to prevent.
            refuse_audio_rtp_cannot_carry(track.channels, message.inbound_link)
        if not self._enters_or_continues_a_group(message):
            return
        self._connect_on_the_first_media_frame(track)

        session = self._connected_session()
        try:
            if track.kind == "video":
                session.write_video_media_frame(message.payload, message.timestamp_ns)
            else:
                session.write_audio_media_frame(message.payload)
        except ValueError as refused_media_frame:
            raise ValueError(
                f"WhipPublisher: a message on `{message.inbound_link}` is "
                f"refused: {refused_media_frame}"
            ) from refused_media_frame
        self._report_progress()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        if self._session is not None:
            self._session.close()
            self._session = None
        log.info(
            f"WhipPublisher: teardown, "
            f"media_frames_published={self._media_frames_published}, "
            f"media_frames_discarded_awaiting_a_group_start="
            f"{self._media_frames_discarded_awaiting_a_group_start}"
        )

    def _report_progress(self) -> None:
        self._media_frames_published += 1
        if self._media_frames_published == 1:
            log.info("WhipPublisher: first media frame published to the session")
        elif self._media_frames_published % MEDIA_FRAMES_BETWEEN_PROGRESS_REPORTS == 0:
            log.info(
                f"WhipPublisher: media_frames_published={self._media_frames_published}"
            )

    def _track_of(
        self, inputs: LinkInputDataReader, inbound_link: str
    ) -> PublishedTrack:
        """The track `inbound_link` publishes as, read off its upstream catalog
        entry on its first message and fixed for the session."""
        track = self._track_by_inbound_link.get(inbound_link)
        if track is not None:
            return track
        track = published_track_from_catalog_entry(
            inputs.inbound_link_catalog_entry(
                TRACKS_INPUT_PORT, inbound_link=inbound_link
            ),
            inbound_link,
        )
        resolve_track_kind(
            track.kind,
            inbound_link,
            {link: known.kind for link, known in self._track_by_inbound_link.items()},
        )
        self._track_by_inbound_link[inbound_link] = track
        return track

    def _enters_or_continues_a_group(self, message: "Message[bytes]") -> bool:
        """Whether `message` is sent: a link is entered at a group start, and
        re-entered at one after a read reports a loss."""
        inbound_link = message.inbound_link
        if message.lost_since_last_read:
            log.warn(
                f"WhipPublisher: `{inbound_link}` lost "
                f"{message.lost_since_last_read} messages; discarding to the "
                f"next group start"
            )
            self._inbound_links_awaiting_a_group_start.add(inbound_link)
        if inbound_link not in self._inbound_links_awaiting_a_group_start:
            return True
        if not message.starts_group:
            self._media_frames_discarded_awaiting_a_group_start += 1
            return False
        self._inbound_links_awaiting_a_group_start.discard(inbound_link)
        return True

    def _connect_on_the_first_media_frame(self, track: PublishedTrack) -> None:
        """Open the session, offering the media the wiring settled.

        With one link the offer carries the medium that link's entry names.
        With two it carries both, because a second link of either medium is
        already refused — so two links are one video and one audio by
        construction, whichever of them produced first.
        """
        session = self._connected_session()
        if session.is_connected:
            return

        carries_both = len(self._inbound_links) == HIGHEST_TRACKS_IN_ONE_SESSION
        video = carries_both or track.kind == "video"
        audio = carries_both or track.kind == "audio"
        log.info(
            f"WhipPublisher: opening a session on the first media frame "
            f"(video={video}, audio={audio}, links={len(self._inbound_links)})"
        )
        session.connect(
            video=video,
            audio=audio,
            audio_channels=track.channels if track.kind == "audio" else None,
        )
        log.info("WhipPublisher: the relay accepted the session")

    def _connected_session(self) -> "_native.WhipSession":
        if self._session is None:
            raise RuntimeError(
                "WhipPublisher: process() ran before setup() opened the session"
            )
        return self._session


@dataclasses.dataclass
class WhepPlayerConfig:
    """What a `WhepPlayer` is configured with."""

    url: Annotated[str, "The WHEP endpoint to play from."]
    bearer_token: Annotated[
        "str | None", "Sent as `Authorization: Bearer` when the endpoint wants one."
    ] = None


@node(
    execution="manual",
    description="Plays encoded video and audio back from a WHEP endpoint",
)
class WhepPlayer:
    """One WHEP session in, media frames out.

    Two output ports rather than one per track: ports are declared statically,
    and a decoder downstream wants a port it can name when the graph is wired.

    A keyframe opens a group on `encoded_video`, and every Opus packet opens
    its own on `encoded_audio`. Each port's catalog entry is stated before the
    first media frame it describes, and stated again when the stream changes,
    from the stream itself: the codec string and coded extent from the SPS,
    and the channel count from the answer's `sprop-stereo` (falling back to the
    first packet's TOC when the answer states no Opus fmtp), latched constant
    for the stream. The `OpusHead` in the audio entry's `description` carries a
    pre-skip of 0 because RTP states no encoder lookahead, so a decoder trims
    nothing.
    """

    def __init__(self, config: WhepPlayerConfig) -> None:
        self._url = _required_url(config.url, "WhepPlayer")
        self._bearer_token = _optional_bearer_token(config.bearer_token)
        self._stop = threading.Event()
        self._reader: "threading.Thread | None" = None
        self._reported_an_oversized_media_frame = False
        self._media_frames_written: "dict[str, int]" = {}
        self._catalog_entry_stated_by_port: "dict[str, CatalogEntry]" = {}

    @node.output()
    def encoded_video(self) -> None:
        """H.264 access units, as media frames."""

    @node.output()
    def encoded_audio(self) -> None:
        """Opus packets, as media frames."""

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        """Hand the outputs to a thread this processor owns.

        Connecting happens on that thread and not in `setup()`, so an endpoint
        that is slow or down cannot spend the helper's start-up budget.
        """
        outputs = ctx.outputs
        self._reader = threading.Thread(
            target=lambda: self._play_until_stopped(outputs), daemon=True
        )
        self._reader.start()

    def _play_until_stopped(self, outputs: LinkOutputDataWriter) -> None:
        """Connect, drain, and reconnect for as long as this processor runs.

        A session per attempt, never one reused: a WHEP session is a peer
        connection, and a closed one cannot be dialled again. This thread owns
        each session's whole life, close included, so a teardown arriving while
        a connect is outstanding waits for the thread rather than contending
        with it.
        """
        delay_seconds = FIRST_RECONNECT_DELAY_SECONDS
        while not self._stop.is_set():
            session = None
            try:
                log.info("WhepPlayer: opening the session")
                session = _native.WhepSession(self._url, self._bearer_token)
                session.connect()
                log.info("WhepPlayer: the relay accepted the session")
                delay_seconds = FIRST_RECONNECT_DELAY_SECONDS
                self._drain_until_stopped(session, outputs)
            except LinkOutputWriteFailure as write_failure:
                # Not the endpoint's fault and not retryable: what this player
                # wrote is something the engine refused, so reconnecting would
                # spend an endpoint's session forever on a write that will be
                # refused again. Named, with its port, and the thread ends.
                log.error(
                    f"WhepPlayer: writing to `{write_failure.port}` "
                    f"failed, and the reading thread is ending: "
                    f"{write_failure.__cause__}"
                )
                return
            except Exception as failure:
                log.warn(
                    f"WhepPlayer: the session ended ({failure}); retrying in "
                    f"{delay_seconds:.1f}s"
                )
            finally:
                if session is not None:
                    session.close()
            if self._stop.is_set():
                return
            # `wait` rather than `sleep`: a stop arriving mid-backoff should end
            # the thread now, not after the longest delay.
            self._stop.wait(delay_seconds)
            delay_seconds = min(delay_seconds * 2, LONGEST_RECONNECT_DELAY_SECONDS)

    def _drain_until_stopped(
        self, session: "_native.WhepSession", outputs: LinkOutputDataWriter
    ) -> None:
        while not self._stop.is_set():
            media = session.next_media(PLAYER_POLL_TIMEOUT_MS)
            if media is None:
                continue
            message = _message_for(media)
            self._state_catalog_entry_when_it_changes(
                outputs, message.port, message.catalog_entry
            )
            self._report_a_media_frame_the_link_will_drop(message.port, message.payload)
            try:
                outputs.write(
                    message.port,
                    message.payload,
                    timestamp_ns=media.timestamp_ns,
                    starts_group=message.starts_group,
                )
            except Exception as write_failure:
                raise LinkOutputWriteFailure(message.port) from write_failure
            self._report_progress(message.port)

    def _state_catalog_entry_when_it_changes(
        self, outputs: LinkOutputDataWriter, port: str, catalog_entry: CatalogEntry
    ) -> None:
        if self._catalog_entry_stated_by_port.get(port) == catalog_entry:
            return
        try:
            outputs.set_catalog_entry(port, catalog_entry)
        except Exception as entry_refusal:
            raise LinkOutputWriteFailure(port) from entry_refusal
        self._catalog_entry_stated_by_port[port] = catalog_entry
        log.info(f"WhepPlayer: `{port}` states {catalog_entry}")

    def _report_progress(self, port: str) -> None:
        written = self._media_frames_written.get(port, 0) + 1
        self._media_frames_written[port] = written
        if written == 1:
            log.info(f"WhepPlayer: first media frame written on `{port}`")
        elif written % MEDIA_FRAMES_BETWEEN_PROGRESS_REPORTS == 0:
            log.info(f"WhepPlayer: `{port}` media_frames_written={written}")

    def _report_a_media_frame_the_link_will_drop(
        self, port: str, payload: bytes
    ) -> None:
        if len(payload) <= HELPER_LINK_PAYLOAD_CEILING_BYTES:
            return
        if self._reported_an_oversized_media_frame:
            return
        self._reported_an_oversized_media_frame = True
        log.error(
            f"WhepPlayer: a {len(payload)}-byte media frame on `{port}` is past the "
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
        self._stop.set()


class MessageThePlayerWritesToOneOutputPort(NamedTuple):
    """One received item as the player publishes it, with the entry its port states."""

    port: str
    payload: bytes
    starts_group: bool
    catalog_entry: CatalogEntry


def _message_for(
    media: "_native.PlayedVideoAccessUnit | _native.PlayedOpusPacket",
) -> MessageThePlayerWritesToOneOutputPort:
    """The port, media frame, group flag and catalog entry one received item is published with.

    A decoder enters a video stream only at a sync point, so only one opens a
    group; every Opus packet is one.
    """
    if isinstance(media, _native.PlayedOpusPacket):
        return MessageThePlayerWritesToOneOutputPort(
            port=ENCODED_AUDIO_OUTPUT_PORT,
            payload=media.media_frame,
            starts_group=True,
            catalog_entry=audio_catalog_entry_of(media),
        )
    return MessageThePlayerWritesToOneOutputPort(
        port=ENCODED_VIDEO_OUTPUT_PORT,
        payload=media.media_frame,
        starts_group=media.is_sync_point,
        catalog_entry=video_catalog_entry_of(media),
    )


def video_catalog_entry_of(access_unit: ReceivedAccessUnit) -> VideoEntry:
    """The `encoded_video` entry one access unit's stream states.

    No `description`: the parameter sets ride inline in the access units.
    """
    return VideoEntry(
        codec=access_unit.codec,
        codedWidth=access_unit.coded_width,
        codedHeight=access_unit.coded_height,
    )


def audio_catalog_entry_of(packet: ReceivedPacket) -> AudioEntry:
    """The `encoded_audio` entry one Opus packet's stream states."""
    return AudioEntry(
        codec=OPUS_CODEC_STRING,
        sampleRate=OPUS_RTP_SAMPLE_RATE_HZ,
        numberOfChannels=packet.channels,
        description=packet.opus_identification_header_hex,
    )
