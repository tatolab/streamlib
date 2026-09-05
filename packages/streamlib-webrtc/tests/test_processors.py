# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two processors as a graph sees them.

`rt.add` with no adapter and no engine change is the whole claim of the
extension model, so it is what these check — over a real `Runtime`, which needs
no device to build a graph.
"""

import os
from typing import Any

import pytest

import streamlib
from streamlib import (
    H264Decoder,
    H264Encoder,
    OpusDecoder,
    OpusEncoder,
    RuntimeContextFullAccess,
    log,
)
from streamlib._engine import ProcessorLinkDataAccess
from streamlib._processor_hosting import construct_processor_instance
from streamlib_webrtc import WhepPlayer, WhipPublisher
from streamlib_webrtc.processors import (
    FIRST_RECONNECT_DELAY_SECONDS,
    HELPER_LINK_PAYLOAD_CEILING_BYTES,
    LONGEST_RECONNECT_DELAY_SECONDS,
    LinkOutputWriteFailure,
    VideoOrAudio,
    _native,
    refuse_audio_rtp_cannot_carry,
    resolve_track_kind,
)


@pytest.fixture
def runtime():
    runtime = streamlib.Runtime()
    try:
        yield runtime
    finally:
        runtime.shutdown()


@pytest.mark.parametrize("processor_class", [WhipPublisher, WhepPlayer])
def test_an_installed_extensions_processor_is_added_like_any_other(
    runtime, processor_class
):
    added = runtime.add(processor_class, config={"url": "https://example.invalid/x"})

    assert added.display_name == processor_class.__name__


def test_the_publisher_wires_to_both_encoders_without_an_adapter(runtime):
    """One fan-in port takes both encoders, which is what makes a WHIP session
    with video and audio a matter of wiring rather than of config."""
    video_encoder = runtime.add(H264Encoder)
    audio_encoder = runtime.add(OpusEncoder)
    publisher = runtime.add(
        WhipPublisher, config={"url": "https://example.invalid/whip"}
    )

    runtime.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    runtime.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))


def test_the_player_wires_to_both_decoders_without_an_adapter(runtime):
    player = runtime.add(WhepPlayer, config={"url": "https://example.invalid/whep"})
    video_decoder = runtime.add(H264Decoder)
    audio_decoder = runtime.add(OpusDecoder)

    runtime.connect(
        player.output("encoded_video"), video_decoder.input("encoded_video")
    )
    runtime.connect(
        player.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )


def test_a_publish_and_play_round_trip_composes_as_published(runtime):
    """The shape the live proof drives: encode, publish, play back, decode."""
    encoder = runtime.add(H264Encoder)
    publisher = runtime.add(
        WhipPublisher, config={"url": "https://example.invalid/whip"}
    )
    player = runtime.add(WhepPlayer, config={"url": "https://example.invalid/whep"})
    decoder = runtime.add(H264Decoder)

    runtime.connect(encoder.output("encoded_video"), publisher.input("tracks"))
    runtime.connect(player.output("encoded_video"), decoder.input("encoded_video"))


def test_each_codec_names_the_track_it_belongs_on():
    assert resolve_track_kind("h264", "encoder/encoded_video", {}) == "video"
    assert resolve_track_kind("opus", "encoder/encoded_audio", {}) == "audio"


@pytest.mark.parametrize("codec", ["h265", "av1", "", None, 5])
def test_a_codec_this_session_does_not_carry_is_refused_naming_it(codec):
    """H.265 included: the offer registers H.264 and Opus, so a stream this
    session never negotiated is a refusal and not a silent wrong track."""
    with pytest.raises(ValueError, match="does not carry"):
        resolve_track_kind(codec, "encoder/encoded_video", {})


def test_two_links_publishing_one_medium_are_refused_naming_both():
    already: "dict[str, VideoOrAudio]" = {"first/encoded_video": "video"}

    with pytest.raises(ValueError, match="second/encoded_video.*first/encoded_video"):
        resolve_track_kind("h264", "second/encoded_video", already)


def test_a_link_that_changes_medium_is_refused_naming_it():
    already: "dict[str, VideoOrAudio]" = {"encoder/tracks": "video"}

    with pytest.raises(ValueError, match="does not change medium"):
        resolve_track_kind("opus", "encoder/tracks", already)


def test_a_link_publishing_the_medium_it_already_claimed_is_unchanged():
    already: "dict[str, VideoOrAudio]" = {"encoder/encoded_video": "video"}

    assert resolve_track_kind("h264", "encoder/encoded_video", already) == "video"


def test_an_oversized_bag_is_reported_once_rather_than_silently_dropped(
    monkeypatch,
):
    """The engine drops a bag past the link ceiling at `debug` rather than
    raising, which downstream looks like a stream that simply stopped. The
    player is the only place that can say what actually happened — and a
    per-frame condition reported per frame is noise, so it says it once."""
    reported: "list[str]" = []
    monkeypatch.setattr(log, "error", reported.append)
    player = WhepPlayer(url="https://example.invalid/whep")
    over_the_ceiling = b"\x00" * (HELPER_LINK_PAYLOAD_CEILING_BYTES + 1)

    player._report_a_bag_the_link_will_drop("encoded_video", over_the_ceiling)
    player._report_a_bag_the_link_will_drop("encoded_video", over_the_ceiling)

    assert len(reported) == 1
    assert "encoded_video" in reported[0]
    assert str(len(over_the_ceiling)) in reported[0]


def test_a_bag_inside_the_ceiling_is_not_reported(monkeypatch):
    reported: "list[str]" = []
    monkeypatch.setattr(log, "error", reported.append)

    WhepPlayer(url="https://example.invalid/whep")._report_a_bag_the_link_will_drop(
        "encoded_video", b"\x00" * 4096
    )

    assert reported == []


@pytest.mark.parametrize("channels", [3, 6, 8])
def test_multichannel_opus_is_refused_because_rtp_cannot_carry_it(channels):
    """`OpusEncoder` legally produces mapping-family-1 multistream packets for
    3-8 channels, and RFC 7587 defines an RTP payload format for mono and
    stereo only. Forwarded anyway, the far end decodes garbage and nothing
    upstream says why."""
    with pytest.raises(ValueError, match="mono and stereo only"):
        refuse_audio_rtp_cannot_carry(channels, "encoder/encoded_audio")


@pytest.mark.parametrize("channels", [1, 2])
def test_the_channel_counts_rtp_carries_are_published(channels):
    refuse_audio_rtp_cannot_carry(channels, "encoder/encoded_audio")


class _PublisherUnderTest:
    """A `WhipPublisher` whose `setup()` can be driven without a graph.

    A helper-process context opened directly on wired links is the same seam
    `setup()` sees in a real child, and it needs no runtime and no device.
    """

    @staticmethod
    def set_up_with(
        request: pytest.FixtureRequest,
        inbound_links: int,
        config: "dict[str, Any]",
    ) -> None:
        unique = f"whipsetup{os.getpid()}_{request.node.name}"
        link_data_access = ProcessorLinkDataAccess()
        for index in range(inbound_links):
            link_data_access.wire_input_link(
                "tracks", f"{unique}/encoder{index}", f"{unique}_dest/notify",
                "read_next_in_order", 8, 2, 4, f"L-{unique}-{index}",
            )  # fmt: skip
        context = RuntimeContextFullAccess.open_for_helper_process(
            {}, link_data_access, "runtime-under-test", "processor-under-test"
        )
        # Constructed the way the helper constructs it: config is the class's
        # own keyword arguments, not something read off the context.
        construct_processor_instance(WhipPublisher, config, link_data_access).setup(
            context
        )


AN_ENDPOINT = {"url": "https://example.invalid/whip"}


@pytest.mark.parametrize("inbound_links", [1, 2])
def test_a_publisher_sets_up_against_the_links_a_session_can_carry(
    request, inbound_links
):
    _PublisherUnderTest.set_up_with(request, inbound_links, AN_ENDPOINT)


def test_a_publisher_with_nothing_wired_refuses_rather_than_publishing_silence(
    request,
):
    """A graph that forgot the connect would otherwise open a session and send
    nothing, which looks from the relay's side like a working publisher."""
    with pytest.raises(ValueError, match="nothing is connected"):
        _PublisherUnderTest.set_up_with(request, 0, AN_ENDPOINT)


def test_more_links_than_a_session_can_carry_are_refused_naming_the_count(request):
    with pytest.raises(ValueError, match="3 links feed"):
        _PublisherUnderTest.set_up_with(request, 3, AN_ENDPOINT)


@pytest.mark.parametrize("config", [{"url": ""}, {"url": 7}])
def test_a_publisher_whose_endpoint_is_not_an_address_is_refused_by_name(
    request, config
):
    with pytest.raises(ValueError, match="`url` is required"):
        _PublisherUnderTest.set_up_with(request, 1, config)


def test_a_publisher_added_with_no_endpoint_at_all_is_refused_by_the_engine(request):
    """`url` has no default, so the engine's own construction refusal names it
    before any of this wheel's validation runs."""
    with pytest.raises(TypeError, match="missing 1 required positional argument"):
        _PublisherUnderTest.set_up_with(request, 1, {})


@pytest.mark.parametrize("processor_class", [WhipPublisher, WhepPlayer])
def test_config_reaches_a_processor_as_its_own_constructor_keywords(processor_class):
    """The shape `rt.add(cls, config={...})` actually delivers.

    `rt.add` records the config and the helper constructs the class from it, so
    a class whose settings are not constructor parameters passes every
    graph-building test and then fails in the child on the first run. This is
    the engine's own mapping, called directly.
    """
    constructed = construct_processor_instance(
        processor_class,
        {"url": "https://example.invalid/x", "bearer_token": "a-token"},
        None,
    )

    assert isinstance(constructed, processor_class)


@pytest.mark.parametrize("processor_class", [WhipPublisher, WhepPlayer])
def test_the_bearer_token_is_optional(processor_class):
    assert isinstance(
        construct_processor_instance(
            processor_class, {"url": "https://example.invalid/x"}, None
        ),
        processor_class,
    )


@pytest.mark.parametrize("processor_class", [WhipPublisher, WhepPlayer])
def test_a_processor_added_without_an_endpoint_is_refused_at_construction(
    processor_class,
):
    with pytest.raises((TypeError, ValueError)):
        construct_processor_instance(processor_class, {}, None)


def test_a_multichannel_bag_is_refused_before_any_session_is_opened(request):
    """The ordering, not just the refusal.

    Refusing after the connect would leave a live session at the relay that
    then publishes nothing — the phantom publisher the refusal exists to
    prevent. The endpoint here is unresolvable, so a connect attempted first
    would raise about signalling instead of about channels.
    """
    unique = f"whipmulti{os.getpid()}_{request.node.name}"
    channel = f"{unique}/encoder"
    notify = f"{unique}_dest/notify"

    destination = ProcessorLinkDataAccess()
    destination.wire_input_link(
        "tracks", channel, notify, "read_next_in_order", 8, 2, 1, f"L-{unique}",
    )  # fmt: skip
    source = ProcessorLinkDataAccess()
    source.wire_output_link(
        "encoded_audio", channel, notify, 1024, 1 << 20, 8, 2, 1, f"L-{unique}",
    )  # fmt: skip

    context = RuntimeContextFullAccess.open_for_helper_process(
        {}, destination, "runtime-under-test", "processor-under-test"
    )
    publisher = construct_processor_instance(
        WhipPublisher, {"url": "https://unresolvable.invalid/whip"}, destination
    )
    publisher.setup(context)
    source.write_to_output_port(
        "encoded_audio",
        {
            "codec": "opus",
            "bitstream": b"\x78\x01\x02",
            "is_sync_point": True,
            "group_index": 0,
            "sequence_index": 0,
            "sample_rate": 48_000,
            "channels": 6,
            "sample_count": 960,
            "pre_skip": 0,
        },
    )

    with pytest.raises(ValueError, match="mono and stereo only"):
        publisher.process(context.limited_access_view_for_helper_process())


class RecordingWhepSession:
    """A `_native.WhepSession` stand-in whose connect outcome is scripted.

    The reconnect loop is the whole reason `WhepPlayer` survives an endpoint
    that is not yet live, and nothing about it needs a network, a device or the
    compiled module: the player resolves `_native.WhepSession` at call time, so
    a fake substituted on the module is what each attempt constructs.
    """

    #: Every session this class has minted, so a test can assert one per attempt.
    constructed: "list[RecordingWhepSession]" = []
    #: What each successive `connect()` does, popped in order: `None` succeeds,
    #: an `Exception` is raised.
    connect_outcomes: "list[Exception | None]" = []

    def __init__(self, url: str, bearer_token: "str | None" = None) -> None:
        del url, bearer_token
        self.connect_calls = 0
        self.closed = False
        RecordingWhepSession.constructed.append(self)

    def connect(self) -> None:
        self.connect_calls += 1
        outcome = RecordingWhepSession.connect_outcomes.pop(0)
        if isinstance(outcome, Exception):
            raise outcome

    def next_media(self, timeout_ms: int) -> None:
        del timeout_ms
        return None

    def close(self) -> None:
        self.closed = True


@pytest.fixture
def scripted_whep_session(monkeypatch):
    """Point `WhepPlayer` at the fake, and reset its class-level script."""
    RecordingWhepSession.constructed = []
    RecordingWhepSession.connect_outcomes = []
    monkeypatch.setattr(_native, "WhepSession", RecordingWhepSession)
    return RecordingWhepSession


def _player_for_a_drain_that_returns_immediately() -> WhepPlayer:
    player = WhepPlayer(url="https://example.invalid/whep")
    # The drain would otherwise spin on `next_media` returning None; stopping
    # the player makes each attempt reach the backoff at once.
    return player


def test_a_refused_connect_is_retried_rather_than_ending_the_stream(
    scripted_whep_session, monkeypatch
):
    """The 409 a WHEP endpoint answers before its ingest is live is not the end.

    This is the failure the live proof found: the player gave up permanently on
    the first refusal, stayed alive, and produced nothing for the rest of the
    run with nothing downstream able to tell that from an endpoint carrying no
    media.
    """
    scripted_whep_session.connect_outcomes = [
        RuntimeError("409 Conflict: Live broadcast not started yet"),
        RuntimeError("409 Conflict: Live broadcast not started yet"),
        None,
    ]
    player = _player_for_a_drain_that_returns_immediately()

    waited: "list[float]" = []

    def stop_after_the_third_attempt(delay_seconds: float) -> None:
        waited.append(delay_seconds)

    monkeypatch.setattr(player._stop, "wait", stop_after_the_third_attempt)
    # The third connect succeeds, so the drain runs; end it there.
    monkeypatch.setattr(player, "_drain_until_stopped", lambda session, outputs: None)
    monkeypatch.setattr(
        player._stop, "is_set", lambda: len(scripted_whep_session.constructed) >= 3
    )

    player._play_until_stopped(outputs=None)

    assert len(scripted_whep_session.constructed) == 3, (
        "each attempt constructs its own session — a closed peer connection "
        "cannot be dialled again"
    )
    assert all(session.closed for session in scripted_whep_session.constructed)
    assert waited == [
        FIRST_RECONNECT_DELAY_SECONDS,
        FIRST_RECONNECT_DELAY_SECONDS * 2,
    ], "the backoff doubles between attempts"


def test_the_backoff_stops_doubling_at_its_ceiling(scripted_whep_session, monkeypatch):
    """An endpoint that is down for hours must not back off for hours."""
    refusals = 12
    scripted_whep_session.connect_outcomes = [
        RuntimeError("still down") for _ in range(refusals)
    ]
    player = _player_for_a_drain_that_returns_immediately()

    waited: "list[float]" = []
    monkeypatch.setattr(player._stop, "wait", waited.append)
    monkeypatch.setattr(
        player._stop, "is_set", lambda: len(waited) >= refusals - 1
    )

    player._play_until_stopped(outputs=None)

    ceiling = LONGEST_RECONNECT_DELAY_SECONDS
    assert max(waited) == ceiling
    assert waited[-1] == ceiling


def test_a_stop_during_the_backoff_ends_the_thread_without_reconnecting(
    scripted_whep_session, monkeypatch
):
    """`stop()` must return well inside the helper's five-second budget."""
    scripted_whep_session.connect_outcomes = [RuntimeError("down")]
    player = _player_for_a_drain_that_returns_immediately()

    def stop_arrives_mid_backoff(delay_seconds: float) -> None:
        del delay_seconds
        player._stop.set()

    monkeypatch.setattr(player._stop, "wait", stop_arrives_mid_backoff)

    player._play_until_stopped(outputs=None)

    assert len(scripted_whep_session.constructed) == 1, (
        "a stop arriving during the backoff opens no further session"
    )


def test_a_refused_bag_names_its_port_and_stops_rather_than_reconnecting(
    scripted_whep_session, monkeypatch
):
    """A wire-contract violation is this player's fault, not the endpoint's.

    Retrying it would spend an endpoint's session forever on a bag that will be
    refused every time, under a message naming neither the port nor the cause.
    """
    scripted_whep_session.connect_outcomes = [None]
    player = _player_for_a_drain_that_returns_immediately()

    def refuse_the_write(session, outputs):
        del session, outputs
        raise LinkOutputWriteFailure("encoded_video")

    monkeypatch.setattr(player, "_drain_until_stopped", refuse_the_write)

    errors: "list[str]" = []
    monkeypatch.setattr(log, "error", errors.append)
    monkeypatch.setattr(
        player._stop, "wait", lambda _: pytest.fail("a refused bag must not be retried")
    )

    player._play_until_stopped(outputs=None)

    assert len(scripted_whep_session.constructed) == 1
    assert scripted_whep_session.constructed[0].closed
    assert any("encoded_video" in message for message in errors), (
        "the port that refused the bag has to reach the log"
    )
