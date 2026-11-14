# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The two processors this wheel supplies.

Both sit on the encoded side of the codec blocks and touch no raw frame, no
surface and no GPU: `WhipPublisher` consumes the Annex-B access units and Opus
packets `H264Encoder` and `OpusEncoder` publish, and `WhepPlayer` writes the
ones `H264Decoder` and `OpusDecoder` consume, each bare and stamped on its
message. The codec, the
coded extent and the channel count ride the ports' stream facts. Each runs
in its own helper process. The publisher calls this wheel's own Rust from its
`process()`; the player lends its two output ports to a thread of this wheel's
Rust through the module host table, which writes every frame with no Python
attached. The engine is never on the data path.
"""

from __future__ import annotations

import dataclasses
from collections.abc import Mapping
from typing import Annotated, Any, Literal, NamedTuple

from tatolab.stream import (
    Input,
    LinkInputDataReader,
    Message,
    Output,
    Packets,
    PortPayloadBorrow,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)
from tatolab.stream.stream_facts import PacketsStreamFacts, PortStreamFacts

from . import _native

TRACKS_INPUT_PORT = "tracks"
ENCODED_VIDEO_OUTPUT_PORT = "encoded_video"
ENCODED_AUDIO_OUTPUT_PORT = "encoded_audio"

#: One WHIP session carries at most one video track and one audio track, so a
#: publisher's inbound links cannot outnumber them.
HIGHEST_TRACKS_IN_ONE_SESSION = 2

#: How long `stop()` waits for the reading thread. The thread's own media wait
#: is bounded, so this only expires when it is still inside a connect.
READER_THREAD_JOIN_TIMEOUT_SECONDS = 1.0

#: How often the publisher says it is still working, on the cadence the player's
#: reading thread reports on too.
MEDIA_FRAMES_BETWEEN_PROGRESS_REPORTS = 300

#: The WebCodecs codec-string prefixes of H.264: `avc1` with parameter sets out
#: of band, `avc3` inline. RTP carries the access unit either way.
H264_CODEC_STRING_PREFIXES = ("avc1.", "avc3.")
OPUS_CODEC_STRING = "opus"

VideoOrAudio = Literal["video", "audio"]


class PublishedTrack(NamedTuple):
    """The RTP track one inbound link publishes as, read off its upstream stream facts."""

    kind: VideoOrAudio
    channels: "int | None"


def published_track_from_upstream_stream_facts(
    upstream_stream_facts: "PortStreamFacts | None", inbound_link: str
) -> PublishedTrack:
    """The track `inbound_link`'s upstream stream facts describe, refused by
    name when they describe nothing this session carries.

    A codec absent from what this session offers is refused rather than
    guessed at.
    """
    if not isinstance(upstream_stream_facts, PacketsStreamFacts):
        raise ValueError(
            f"WhipPublisher: `{inbound_link}` carries no Packets stream facts, and each "
            f"track's codec is read from them. Connect an H264Encoder or an "
            f"OpusEncoder output, or a node that states its port's stream facts."
        )
    codec = upstream_stream_facts.rfc6381_codec
    if isinstance(codec, str) and codec.startswith(H264_CODEC_STRING_PREFIXES):
        return PublishedTrack(kind="video", channels=None)
    if codec == OPUS_CODEC_STRING:
        return PublishedTrack(kind="audio", channels=upstream_stream_facts.channel_count)
    raise ValueError(
        f"WhipPublisher: `{inbound_link}`'s stream facts name codec "
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
#: `tatolab.opus.OpusEncoder` legally produces mapping-family-1 multistream packets
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
            f"{processor_name}: `url` is not set; it takes the endpoint's address, "
            f"got {url!r}"
        )
    return url


def _optional_bearer_token(bearer_token: Any) -> "str | None":
    return bearer_token if isinstance(bearer_token, str) and bearer_token else None


@dataclasses.dataclass
class WhipPublisherConfig:
    """What a `WhipPublisher` is configured with."""

    url: Annotated[str, "The WHIP endpoint to publish to; the node fails, naming `url`, until it is set."] = ""
    bearer_token: Annotated[
        "str | None", "Sent as `Authorization: Bearer` when the endpoint wants one."
    ] = None


@node.meta(
    description=(
        "Publishes encoded video and audio to a WHIP endpoint, "
        "one RTP track per inbound link"
    ),
)
@node.process(paced_by_real_time=True)
class WhipPublisher:
    """Media frames in, one WHIP session out.

    One fan-in input, and each inbound link is one track whose medium the
    upstream port's stream facts name. Its settings are a
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

    tracks = Input(
        Packets,
        delivery="ordered",
        requires_codec=("h264", "opus"),
        description="H.264 access units or Opus packets; each inbound link becomes one track",
    )

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
                session.write_video_access_unit(message.payload, message.timestamp_ns)
            else:
                session.write_audio_opus_packet(message.payload)
        except ValueError as refused_unit:
            raise ValueError(
                f"WhipPublisher: a message on `{message.inbound_link}` is "
                f"refused: {refused_unit}"
            ) from refused_unit
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
        """The track `inbound_link` publishes as, read off its upstream stream
        facts on its first message and fixed for the session."""
        track = self._track_by_inbound_link.get(inbound_link)
        if track is not None:
            return track
        track = published_track_from_upstream_stream_facts(
            inputs.upstream_stream_facts(
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

    def _enters_or_continues_a_group(self, message: "Message[PortPayloadBorrow]") -> bool:
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

    url: Annotated[str, "The WHEP endpoint to play from; the node fails, naming `url`, until it is set."] = ""
    bearer_token: Annotated[
        "str | None", "Sent as `Authorization: Bearer` when the endpoint wants one."
    ] = None


@node.meta(description="Plays encoded video and audio back from a WHEP endpoint")
@node.process(execution="manual")
class WhepPlayer:
    """One WHEP session in, access units and Opus packets out.

    Two output ports rather than one per track: ports are declared statically,
    and a decoder downstream wants a port it can name when the graph is wired.

    A keyframe opens a group on `encoded_video`, and every Opus packet opens
    its own on `encoded_audio`. Each port's stream facts are stated before the
    first media frame they describe, and stated again when the stream changes,
    from the stream itself: the codec string and coded extent from the SPS,
    and the channel count from the answer's `sprop-stereo` (falling back to the
    first packet's TOC when the answer states no Opus fmtp), latched constant
    for the stream. The `OpusHead` in the audio facts' `codec_configuration_hex` carries a
    pre-skip of 0 because RTP states no encoder lookahead, so a decoder trims
    nothing.
    """

    def __init__(self, config: WhepPlayerConfig) -> None:
        self._url = _required_url(config.url, "WhepPlayer")
        self._bearer_token = _optional_bearer_token(config.bearer_token)
        self._reading_thread: "_native.WhepPlayerThread | None" = None

    encoded_video = Output(
        Packets, codec="h264", description="H.264 access units, Annex-B, one per message"
    )
    encoded_audio = Output(Packets, codec="opus", description="Opus packets, one per message")

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        """Lend both outputs to a reading thread of this wheel's Rust.

        Connecting happens on that thread and not in `setup()`, so an endpoint
        that is slow or down cannot spend the helper's start-up budget.
        """
        module_host_table = ctx.open_module_host_table(
            output_ports=[ENCODED_VIDEO_OUTPUT_PORT, ENCODED_AUDIO_OUTPUT_PORT]
        )
        self._reading_thread = _native.WhepPlayerThread(
            module_host_table, self._url, self._bearer_token
        )

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_the_reading_thread()

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop_the_reading_thread()

    def _stop_the_reading_thread(self) -> None:
        if self._reading_thread is None:
            return
        # Bounded well inside the helper's five-second teardown budget: the
        # thread's own wait for media is bounded. A thread still inside a
        # connect against an unresponsive relay outlives this, and closes the
        # session itself when the connect returns — which is the most a stop
        # can honestly promise.
        if not self._reading_thread.stop_and_join(READER_THREAD_JOIN_TIMEOUT_SECONDS):
            log.warn(
                "WhepPlayer: the reading thread is still connecting; the session "
                "will close when that connect returns"
            )
        self._reading_thread = None
