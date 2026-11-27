# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The two nodes this wheel supplies.

Both sit on the encoded side of the codec blocks and touch no raw frame, no
surface and no GPU: `WhipPublisher` consumes the Annex-B access units and Opus
packets `H264Encoder` and `OpusEncoder` publish, and `WhepPlayer` writes the
ones `H264Decoder` and `OpusDecoder` consume, each bare and stamped on its
message. The codec, the coded extent and the channel count ride the ports'
stream facts. Each is one function, running in its own helper process, and its
`url` and `bearer_token` are live: a new value opens a session of it beside the
one running, and the old one closes once the new one has opened. The publisher
calls this wheel's own Rust from its per-frame function; the player lends its two
output ports to a thread of this wheel's Rust through the module host table,
which writes every frame with no Python attached. The engine is never on the
data path.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping
from typing import Annotated, Literal, NamedTuple

from tatolab.stream import (
    Description,
    EncodedAudio,
    EncodedCodecs,
    EncodedVideo,
    Input,
    InputPin,
    Message,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    StreamDelivery,
    computed,
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


class EndpointUrlNotSet(ValueError):
    """`url` is empty, and a session is opened at the endpoint it names."""


def _required_url(url: str, node_name: str) -> str:
    if not url:
        raise EndpointUrlNotSet(
            f"{node_name}: `url` is not set; it takes the endpoint's address"
        )
    return url


def _optional_bearer_token(bearer_token: "str | None") -> "str | None":
    return bearer_token or None


PublishedTracks = InputPin[
    Annotated[
        Annotated[EncodedVideo, EncodedCodecs("h264")] | Annotated[EncodedAudio, EncodedCodecs("opus")],
        StreamDelivery("ordered"),
        Description("H.264 access units or Opus packets; each inbound link becomes one track"),
    ]
]
"""The `tracks` pin of a node running a `WhipSessionPublishing`: H.264 video or Opus audio, one
track per inbound link, read in order."""


class WhipSessionPublishing:
    """One WHIP session, publishing what the `tracks` pin of the node running it reads.

    `WhipPublisher` builds one at the endpoint its `url` names each time that changes, and a
    node naming its endpoint another way builds one of its own.
    """

    def __init__(self, url: str, bearer_token: "str | None") -> None:
        self._url = url
        self._bearer_token = bearer_token
        self._session: "_native.WhipSession | None" = None
        self._inbound_links: "list[str]" = []
        self._track_by_inbound_link: "dict[str, PublishedTrack]" = {}
        self._inbound_links_awaiting_a_group_start: "set[str]" = set()
        self._media_frames_published = 0
        self._media_frames_discarded_awaiting_a_group_start = 0

    def set_up(self, tracks: PublishedTracks, *, replacing_a_connected_session: bool = False) -> None:
        """Make the session against the links feeding `tracks`, which are wired before the node
        is set up, so the track count is known before a message has arrived and before anything
        is offered. Replacing a connected session, it connects here when it can, so that a
        refusal leaves the connected one publishing."""
        self._inbound_links = tracks.inbound_link_names()
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
        if replacing_a_connected_session:
            self._connect_now_if_every_track_is_known(tracks)

    def _connect_now_if_every_track_is_known(self, tracks: PublishedTracks) -> None:
        """Connect while being set up when every link's upstream already states its stream
        facts, as it does while a session is publishing: a refusal then raises
        `WhipSessionNotOpened` here, and the session publishing keeps publishing. Each link still
        enters the new session at its next group start."""
        if any(
            tracks.upstream_stream_facts(inbound_link=inbound_link) is None
            for inbound_link in self._inbound_links
        ):
            return
        published = [self._track_of(tracks, inbound_link) for inbound_link in self._inbound_links]
        for inbound_link, track in zip(self._inbound_links, published):
            if track.kind == "audio":
                refuse_audio_rtp_cannot_carry(track.channels, inbound_link)
        audio = next((track for track in published if track.kind == "audio"), None)
        self._connect(audio or published[0], when="as the session is set up")

    def publish_the_next_message(self, tracks: PublishedTracks) -> None:
        """Send the next message `tracks` holds, if any, on its link's track."""
        message = tracks.read_message()
        if message is None:
            return

        track = self._track_of(tracks, message.inbound_link)
        if track.kind == "audio":
            # Before the connect, not after: refusing a stream once a session
            # is already open leaves exactly the phantom publisher — live at
            # the relay, permanently silent — that refusing exists to prevent.
            refuse_audio_rtp_cannot_carry(track.channels, message.inbound_link)
        if not self._enters_or_continues_a_group(message):
            return
        self._connect(track, when="on the first media frame")

        session = self._connected_session()
        try:
            if track.kind == "video":
                session.write_video_access_unit(message.payload.payload, message.timestamp_ns)
            else:
                session.write_audio_opus_packet(message.payload.payload)
        except ValueError as refused_unit:
            raise ValueError(
                f"WhipPublisher: a message on `{message.inbound_link}` is "
                f"refused: {refused_unit}"
            ) from refused_unit
        self._report_progress()

    @property
    def is_connected(self) -> bool:
        return self._session is not None and self._session.is_connected

    def close(self) -> None:
        """Close the session, and say how much it published."""
        if self._session is not None:
            self._session.close()
            self._session = None
        log.info(
            f"WhipPublisher: session closed, "
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

    def _track_of(self, tracks: PublishedTracks, inbound_link: str) -> PublishedTrack:
        """The track `inbound_link` publishes as, read off its upstream stream
        facts on its first message and fixed for the session."""
        track = self._track_by_inbound_link.get(inbound_link)
        if track is not None:
            return track
        track = published_track_from_upstream_stream_facts(
            tracks.upstream_stream_facts(inbound_link=inbound_link),
            inbound_link,
        )
        resolve_track_kind(
            track.kind,
            inbound_link,
            {link: known.kind for link, known in self._track_by_inbound_link.items()},
        )
        self._track_by_inbound_link[inbound_link] = track
        return track

    def _enters_or_continues_a_group(self, message: "Message[EncodedVideo | EncodedAudio]") -> bool:
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

    def _connect(self, track: PublishedTrack, *, when: str) -> None:
        """Open the session, unless it is open, offering the media the wiring settled.

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
            f"WhipPublisher: opening a session {when} "
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


@node.meta(
    description=(
        "Publishes encoded video and audio to a WHIP endpoint, "
        "one RTP track per inbound link"
    ),
)
@node(paced_by_real_time=True)
def WhipPublisher(
    *,
    url: Input[
        Annotated[
            str,
            Description(
                "The WHIP endpoint to publish to; the node fails, naming `url`, until it is set."
            ),
        ]
    ] = Input(""),
    bearer_token: Input[
        Annotated[
            str | None,
            Description("Sent as `Authorization: Bearer` when the endpoint wants one."),
        ]
    ] = Input(None),
    tracks: PublishedTracks,
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Media frames in, one WHIP session out.

    One fan-in input, and each inbound link is one track whose medium the
    upstream port's stream facts name.

    Each link enters at a message that starts a group — a keyframe, or any
    Opus packet — and after a read reports a loss it discards up to the next
    one, so the far end is never sent a picture whose references it lacks.

    `url` and `bearer_token` are live. The first session opens on the first
    media frame rather than as the node is set up, because a relay round trip
    there spends the helper's start-up budget and a relay outage there takes
    the whole graph down with it. A new value opens its session off the frame
    thread while the old one keeps publishing; once it has opened the old one
    closes and each link enters the new one at its next group start, so video
    pauses until the next keyframe. A session the endpoint refuses raises
    `WhipSessionNotOpened` on the node, and the old one keeps publishing.

    One known offset: video reaches the far end presenting roughly one frame
    interval later than audio. The RTP payloader applies a sample's duration to
    the frame *after* it, so a publisher that cannot see the next frame without
    delaying this one numbers video one frame behind real time; audio takes its
    duration from each packet's own sample count and is exact. At 30 fps that
    is about 33 ms of video lag, inside the ITU-R BT.1359 comfort bound but not
    zero.
    """

    last_built: "WhipSessionPublishing | None" = None

    def publishing_at_the_endpoint() -> WhipSessionPublishing:
        nonlocal last_built
        publishing = WhipSessionPublishing(
            _required_url(url.get(), "WhipPublisher"), _optional_bearer_token(bearer_token.get())
        )
        try:
            # The first session connects on its first media frame, and so does one replacing a
            # session that never connected: only a connected session has frames to keep
            # publishing while its replacement is refused.
            publishing.set_up(
                tracks,
                replacing_a_connected_session=last_built is not None and last_built.is_connected,
            )
        except BaseException:
            publishing.close()
            raise
        last_built = publishing
        return publishing

    publishing = computed(publishing_at_the_endpoint)

    def publish_the_next_message(ctx: RuntimeContextLimitedAccess) -> None:
        del ctx
        publishing.get().publish_the_next_message(tracks)

    return publish_the_next_message


@node.meta(description="Plays encoded video and audio back from a WHEP endpoint")
@node(execution="manual")
def WhepPlayer(
    *,
    url: Input[
        Annotated[
            str,
            Description(
                "The WHEP endpoint to play from; the node fails, naming `url`, until it is set."
            ),
        ]
    ] = Input(""),
    bearer_token: Input[
        Annotated[
            str | None,
            Description("Sent as `Authorization: Bearer` when the endpoint wants one."),
        ]
    ] = Input(None),
    encoded_video: OutputPin[
        Annotated[
            EncodedVideo,
            EncodedCodecs("h264"),
            Description("H.264 access units, Annex-B, one per message"),
        ]
    ],
    encoded_audio: OutputPin[
        Annotated[EncodedAudio, EncodedCodecs("opus"), Description("Opus packets, one per message")]
    ],
    ctx: RuntimeContextFullAccess,
) -> None:
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

    `url` and `bearer_token` are live. A new value dials its endpoint off the frame thread
    while the old one keeps playing, and the reading thread moves to it once it has connected;
    an endpoint that refuses raises `WhepSessionNotOpened` on the node, and the old one keeps
    playing. The first endpoint is the exception: a WHEP endpoint refuses until its publisher
    starts, so the thread redials it with backoff instead, and a player brought up before its
    publisher plays once the publisher does.
    """
    del encoded_video, encoded_audio
    # Both outputs go to a reading thread of this wheel's Rust, which writes every frame with no
    # Python attached.
    reading_thread = _native.WhepPlayerThread(
        ctx.open_module_host_table(
            output_ports=[ENCODED_VIDEO_OUTPUT_PORT, ENCODED_AUDIO_OUTPUT_PORT]
        )
    )
    computed(
        lambda: reading_thread.open_session(
            _required_url(url.get(), "WhepPlayer"), _optional_bearer_token(bearer_token.get())
        )
    )
