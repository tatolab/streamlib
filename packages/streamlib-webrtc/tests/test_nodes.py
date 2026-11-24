# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


import dataclasses
import os
import time
from collections.abc import Buffer, Callable
from typing import Any, cast

import pytest
from helper_process_link_wiring import wire_one_link
from parent_stream_facts_stand_in import ParentHoldingStreamFacts
from played_media_stand_ins import (
    ANNEX_B_ACCESS_UNIT,
    MONO_OPUS_IDENTIFICATION_HEADER_HEX,
    OPUS_PACKET,
)
from tatolab.runtime import _engine
from tatolab.runtime._class_node_bound_pins import bind_the_pins_of_a_class_node
from tatolab.runtime._node_hosting import class_node_members_of
from tatolab.stream import (
    Message,
    Output,
    Packets,
    log,
)
from tatolab.stream._class_node_construction import construct_class_node
from tatolab.stream.stream_facts import PacketsStreamFacts
from tatolab.webrtc import WhepPlayer, WhipPublisher
from tatolab.webrtc.nodes import (
    ENCODED_AUDIO_OUTPUT_PORT,
    ENCODED_VIDEO_OUTPUT_PORT,
    READER_THREAD_JOIN_TIMEOUT_SECONDS,
    TRACKS_INPUT_PORT,
    PublishedTrack,
    VideoOrAudio,
    _native,
    published_track_from_upstream_stream_facts,
    refuse_audio_rtp_cannot_carry,
    resolve_track_kind,
)

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")


H264_VIDEO_STREAM_FACTS = PacketsStreamFacts(
    rfc6381_codec="avc1.42e01f", coded_width=320, coded_height=192
)
MONO_OPUS_STREAM_FACTS = PacketsStreamFacts(
    rfc6381_codec="opus",
    sample_rate=48_000,
    channel_count=1,
    codec_configuration_hex=MONO_OPUS_IDENTIFICATION_HEADER_HEX,
)


def opus_stream_facts_of(channels: int) -> PacketsStreamFacts:
    return dataclasses.replace(MONO_OPUS_STREAM_FACTS, channel_count=channels)


@pytest.mark.parametrize("codec", ["avc1.42e01f", "avc1.640028", "avc3.64001f"])
def test_h264_stream_facts_name_a_video_track(codec):
    assert published_track_from_upstream_stream_facts(
        PacketsStreamFacts(rfc6381_codec=codec), "encoder/encoded_video"
    ) == PublishedTrack(kind="video", channels=None)


def test_opus_stream_facts_name_an_audio_track_with_their_channel_count():
    assert published_track_from_upstream_stream_facts(
        opus_stream_facts_of(2), "encoder/encoded_audio"
    ) == PublishedTrack(kind="audio", channels=2)


@pytest.mark.parametrize(
    "rfc6381_codec",
    ["hvc1.1.6.L93.B0", "av01.0.08M.08", "mp4a.40.2", None],
    ids=["h265", "av1", "aac", "no_codec"],
)
def test_stream_facts_naming_media_this_session_does_not_carry_are_refused_naming_them(
    rfc6381_codec,
):
    """H.265 included: the offer registers H.264 and Opus, so a stream this
    session never negotiated is a refusal and not a silent wrong track."""
    with pytest.raises(ValueError, match="encoder/output.*does not carry"):
        published_track_from_upstream_stream_facts(
            PacketsStreamFacts(rfc6381_codec=rfc6381_codec), "encoder/output"
        )


def test_a_link_whose_upstream_port_states_no_facts_is_refused_naming_it():
    with pytest.raises(ValueError, match="`numpy/frames` carries no Packets stream facts"):
        published_track_from_upstream_stream_facts(None, "numpy/frames")


def test_two_links_publishing_one_medium_are_refused_naming_both():
    already: "dict[str, VideoOrAudio]" = {"first/encoded_video": "video"}

    with pytest.raises(ValueError, match="second/encoded_video.*first/encoded_video"):
        resolve_track_kind("video", "second/encoded_video", already)


def test_a_link_that_changes_medium_is_refused_naming_it():
    already: "dict[str, VideoOrAudio]" = {"encoder/tracks": "video"}

    with pytest.raises(ValueError, match="does not change medium"):
        resolve_track_kind("audio", "encoder/tracks", already)


def test_a_link_publishing_the_medium_it_already_claimed_is_unchanged():
    already: "dict[str, VideoOrAudio]" = {"encoder/encoded_video": "video"}

    assert resolve_track_kind("video", "encoder/encoded_video", already) == "video"


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


class PublisherWiredToEncoders:
    """A set-up `WhipPublisher` whose `tracks` input one link per encoder port feeds.

    A helper-process context opened directly on wired links is the same seam
    `setup()` and `process()` see in a real child, and it needs no runtime and
    no device.
    """

    def __init__(
        self,
        request: pytest.FixtureRequest,
        encoder_output_ports: "list[str]",
        config: "dict[str, Any]",
    ) -> None:
        unique = f"whipwired{os.getpid()}_{request.node.name}"
        self.parent = ParentHoldingStreamFacts()
        destination_notify_service_name = f"{unique}_dest/notify"
        tracks_input = _engine.open_node_link_data_access_for_helper_process()
        self.encoder_outputs = _engine.open_node_link_data_access_for_helper_process()
        self.inbound_link_by_encoder_output_port: "dict[str, str]" = {}
        for encoder_output_port in encoder_output_ports:
            channel_service_name = f"{unique}/{encoder_output_port}"
            wire_one_link(
                self.encoder_outputs,
                encoder_output_port,
                tracks_input,
                TRACKS_INPUT_PORT,
                channel_service_name,
                destination_notify_service_name,
                notify_max_notifiers=len(encoder_output_ports),
            )
            self.inbound_link_by_encoder_output_port[encoder_output_port] = (
                channel_service_name
            )
        self.encoder_context = _engine.open_runtime_context_full_access_for_helper_process(
            {}, self.encoder_outputs, "runtime-under-test", "encoders-under-test"
        )
        self.context = _engine.open_runtime_context_full_access_for_helper_process(
            {},
            tracks_input,
            "runtime-under-test",
            "node-under-test",
            self.parent.escalate_request_to_parent,
        )
        # Built the way the helper builds it: each value becomes the field of its
        # name, and `tracks` reads through the port the links feed.
        members = class_node_members_of(WhipPublisher)
        assert members is not None
        pins = bind_the_pins_of_a_class_node(
            members,
            link_data_access=tracks_input,
            inputs=self.context.inputs,
            outputs=self.context.outputs,
            node_id="node-under-test",
        )
        self.publisher = construct_class_node(WhipPublisher, field_values=config, pins=pins)
        self.publisher.setup(self.context)

    def state_stream_facts(
        self, encoder_output_port: str, stream_facts: PacketsStreamFacts
    ) -> None:
        self.parent.stream_facts_by_inbound_link[
            self.inbound_link_by_encoder_output_port[encoder_output_port]
        ] = stream_facts.to_wire()

    def publish(
        self,
        encoder_output_port: str,
        payload: bytes,
        timestamp_ns: int = 0,
        starts_group: bool = True,
    ) -> None:
        written_through = Output(Packets)
        written_through.__set_name__(type(self), encoder_output_port)
        self.encoder_context.outputs.write(written_through, payload, timestamp_ns, starts_group)
        self.publisher.process(
            _engine.limited_access_view_of_runtime_context_for_helper_process(
                self.context
            )
        )


def numbered_encoder_output_ports(count: int) -> "list[str]":
    return [f"encoder{index}_output" for index in range(count)]


AN_ENDPOINT = {"url": "https://example.invalid/whip"}


@pytest.mark.parametrize("inbound_links", [1, 2])
def test_a_publisher_sets_up_against_the_links_a_session_can_carry(
    request, inbound_links
):
    PublisherWiredToEncoders(request, numbered_encoder_output_ports(inbound_links), AN_ENDPOINT)


def test_a_publisher_with_nothing_wired_refuses_rather_than_publishing_silence(
    request,
):
    """A graph that forgot the connect would otherwise open a session and send
    nothing, which looks from the relay's side like a working publisher."""
    with pytest.raises(ValueError, match="nothing is connected"):
        PublisherWiredToEncoders(request, numbered_encoder_output_ports(0), AN_ENDPOINT)


def test_more_links_than_a_session_can_carry_are_refused_naming_the_count(request):
    with pytest.raises(ValueError, match="3 links feed"):
        PublisherWiredToEncoders(request, numbered_encoder_output_ports(3), AN_ENDPOINT)


@pytest.mark.parametrize("config", [{"url": ""}, {"url": 7}])
def test_a_publisher_whose_endpoint_is_not_an_address_is_refused_by_name(
    request, config
):
    with pytest.raises(ValueError, match="`url` is not set"):
        PublisherWiredToEncoders(request, numbered_encoder_output_ports(1), config)


def test_a_publisher_added_with_no_values_takes_its_defaults_and_names_the_unset_url(request):
    with pytest.raises(ValueError, match="`url` is not set"):
        PublisherWiredToEncoders(request, numbered_encoder_output_ports(1), {})


def a_node_built_with(node_class: type, field_values: "dict[str, Any]") -> Any:
    """`node_class` built as its helper builds it, each pin bound to a stand-in."""
    members = class_node_members_of(node_class)
    assert members is not None
    return construct_class_node(
        node_class,
        field_values=field_values,
        pins={member.port: object() for member in members if member.form in ("input_pin", "output_pin")},
    )


@pytest.mark.parametrize("node_class", [WhipPublisher, WhepPlayer])
def test_each_setting_reaches_a_node_as_its_field(node_class):
    """The shape `stream_builder.add(cls, input_values={...})` delivers: each value is the field
    of its name before `__init__()` reads it."""
    built = a_node_built_with(
        node_class, {"url": "https://example.invalid/x", "bearer_token": "a-token"}
    )

    assert (built.url, built.bearer_token) == ("https://example.invalid/x", "a-token")


@pytest.mark.parametrize("node_class", [WhipPublisher, WhepPlayer])
def test_the_bearer_token_is_optional(node_class):
    assert a_node_built_with(node_class, {"url": "https://example.invalid/x"}).bearer_token is None


@pytest.mark.parametrize("node_class", [WhipPublisher, WhepPlayer])
def test_a_node_added_without_an_endpoint_is_refused_at_construction(node_class):
    with pytest.raises(ValueError, match="`url` is not set"):
        a_node_built_with(node_class, {})


@pytest.mark.parametrize("node_class", [WhipPublisher, WhepPlayer])
def test_each_setting_is_an_input_that_takes_a_wire(node_class):
    """Every setting has a socket: none is declared only as a setting."""
    from tatolab.stream._node_declaration import NODE_DECLARATION_INPUT_PORTS_STAMP

    modes = {
        port["name"]: port["input_mode"]
        for port in getattr(node_class, NODE_DECLARATION_INPUT_PORTS_STAMP)
        if port["name"] in ("url", "bearer_token")
    }
    assert modes == {"url": {"mode": "wire_or_value"}, "bearer_token": {"mode": "wire_or_value"}}


def test_the_publishers_tracks_take_h264_video_and_opus_audio_on_one_pin():
    from tatolab.stream._node_declaration import NODE_DECLARATION_INPUT_PORTS_STAMP

    (tracks,) = [
        port
        for port in getattr(WhipPublisher, NODE_DECLARATION_INPUT_PORTS_STAMP)
        if port["name"] == TRACKS_INPUT_PORT
    ]
    assert tracks["pin_declaration"] == {
        "pin_type": {
            "kind": "one_of",
            "pin_types": [
                {"type": "encoded_video", "codecs": ["h264"]},
                {"type": "encoded_audio", "codecs": ["opus"]},
            ],
        },
        "flow": "stream",
    }
    assert tracks["delivery_profile"] == "ordered"


class RecordingWhipSession:
    """A `_native.WhipSession` stand-in keeping what the publisher offered and sent."""

    constructed: "list[RecordingWhipSession]" = []

    def __init__(self, endpoint_url: str, bearer_token: "str | None" = None) -> None:
        del endpoint_url, bearer_token
        self.is_connected = False
        self.offered: "tuple[bool, bool, int | None] | None" = None
        self.video_access_units: "list[tuple[bytes, int]]" = []
        self.audio_opus_packets: "list[bytes]" = []
        RecordingWhipSession.constructed.append(self)

    def connect(
        self, *, video: bool, audio: bool, audio_channels: "int | None" = None
    ) -> None:
        self.offered = (video, audio, audio_channels)
        self.is_connected = True

    def write_video_access_unit(self, annex_b_access_unit: Buffer, timestamp_ns: int) -> None:
        self.video_access_units.append((bytes(annex_b_access_unit), timestamp_ns))

    def write_audio_opus_packet(self, opus_packet: Buffer) -> None:
        self.audio_opus_packets.append(bytes(opus_packet))

    def close(self) -> None:
        self.is_connected = False


def route_every_log_level_to(
    monkeypatch: pytest.MonkeyPatch, log_record_sink: "Callable[[str], None]"
) -> None:
    """`log` reaches a sink only in the interpreter the runtime starts for a node."""
    for log_level in ("debug", "info", "warn", "error"):
        monkeypatch.setattr(log, log_level, log_record_sink)


def discard_log_record(message: str) -> None:
    del message


@pytest.fixture
def recording_whip_session(monkeypatch) -> "type[RecordingWhipSession]":
    RecordingWhipSession.constructed = []
    monkeypatch.setattr(_native, "WhipSession", RecordingWhipSession)
    route_every_log_level_to(monkeypatch, discard_log_record)
    return RecordingWhipSession


def test_an_access_unit_reaches_the_session_whole_with_its_envelope_stamp(
    request, recording_whip_session
):
    """The envelope's stamp — the one a local reader is promised — places it
    on the RTP clock."""
    wired = PublisherWiredToEncoders(request, ["encoded_video"], AN_ENDPOINT)
    wired.state_stream_facts("encoded_video", H264_VIDEO_STREAM_FACTS)

    wired.publish("encoded_video", ANNEX_B_ACCESS_UNIT, timestamp_ns=1_234)

    (session,) = recording_whip_session.constructed
    assert session.offered == (True, False, None)
    assert session.video_access_units == [(ANNEX_B_ACCESS_UNIT, 1_234)]


def test_an_opus_packet_offers_the_channel_count_its_stream_facts_name(
    request, recording_whip_session
):
    wired = PublisherWiredToEncoders(request, ["encoded_audio"], AN_ENDPOINT)
    wired.state_stream_facts("encoded_audio", opus_stream_facts_of(2))

    wired.publish("encoded_audio", OPUS_PACKET)

    (session,) = recording_whip_session.constructed
    assert session.offered == (False, True, 2)
    assert session.audio_opus_packets == [OPUS_PACKET]


def test_two_links_are_one_video_and_one_audio_track_by_their_stream_facts(
    request, recording_whip_session
):
    wired = PublisherWiredToEncoders(
        request, ["encoded_video", "encoded_audio"], AN_ENDPOINT
    )
    wired.state_stream_facts("encoded_video", H264_VIDEO_STREAM_FACTS)
    wired.state_stream_facts("encoded_audio", MONO_OPUS_STREAM_FACTS)

    wired.publish("encoded_audio", OPUS_PACKET)
    wired.publish("encoded_video", ANNEX_B_ACCESS_UNIT, timestamp_ns=5)

    (session,) = recording_whip_session.constructed
    assert session.offered == (True, True, 1)
    assert session.audio_opus_packets == [OPUS_PACKET]
    assert session.video_access_units == [(ANNEX_B_ACCESS_UNIT, 5)]


def test_a_link_whose_upstream_states_no_stream_facts_is_refused_before_any_session_opens(
    request, recording_whip_session
):
    """Any port can be wired into `tracks`, and the refusal is the only place
    that wiring mistake is caught."""
    wired = PublisherWiredToEncoders(request, ["frames"], AN_ENDPOINT)

    with pytest.raises(ValueError, match="carries no Packets stream facts") as refused:
        wired.publish("frames", ANNEX_B_ACCESS_UNIT)

    inbound_link = wired.inbound_link_by_encoder_output_port["frames"]
    assert f"`{inbound_link}`" in str(refused.value)
    assert recording_whip_session.constructed[0].offered is None


def test_multichannel_stream_facts_are_refused_before_any_session_is_opened(request):
    """The ordering, not just the refusal.

    Refusing after the connect would leave a live session at the relay that
    then publishes nothing — the phantom publisher the refusal exists to
    prevent. The endpoint here is unresolvable, so a connect attempted first
    would raise about signalling instead of about channels.
    """
    wired = PublisherWiredToEncoders(
        request, ["encoded_audio"], {"url": "https://unresolvable.invalid/whip"}
    )
    wired.state_stream_facts("encoded_audio", opus_stream_facts_of(6))

    with pytest.raises(ValueError, match="mono and stereo only"):
        wired.publish("encoded_audio", OPUS_PACKET)


def test_a_video_link_is_entered_at_its_first_group_start(
    request, recording_whip_session
):
    """A delta frame before the first keyframe references pictures the far end
    never had."""
    wired = PublisherWiredToEncoders(request, ["encoded_video"], AN_ENDPOINT)
    wired.state_stream_facts("encoded_video", H264_VIDEO_STREAM_FACTS)

    wired.publish("encoded_video", b"\x01delta", timestamp_ns=1, starts_group=False)
    wired.publish("encoded_video", b"\x02keyframe", timestamp_ns=2, starts_group=True)
    wired.publish("encoded_video", b"\x03delta", timestamp_ns=3, starts_group=False)

    (session,) = recording_whip_session.constructed
    assert session.video_access_units == [(b"\x02keyframe", 2), (b"\x03delta", 3)]


@dataclasses.dataclass(frozen=True)
class PacketsUnitHeld:
    """A read Packets unit, holding its bytes."""

    payload: bytes


class TracksPinReplayingMessages:
    """A publisher's `tracks` pin, handing back scripted messages and one set of stream facts."""

    def __init__(
        self,
        inbound_links: "list[str]",
        upstream_stream_facts: PacketsStreamFacts,
        messages: "list[Message[PacketsUnitHeld]]",
    ) -> None:
        self.inbound_links = inbound_links
        self.upstream_stream_facts_handed_back = upstream_stream_facts
        self.messages_still_to_hand_back = list(messages)

    def inbound_link_names(self) -> "list[str]":
        return self.inbound_links

    def read_message(self) -> "Message[PacketsUnitHeld] | None":
        if self.messages_still_to_hand_back:
            return self.messages_still_to_hand_back.pop(0)
        return None

    def upstream_stream_facts(self, *, inbound_link: "str | None" = None) -> PacketsStreamFacts:
        del inbound_link
        return self.upstream_stream_facts_handed_back


def video_message(
    payload: bytes, *, starts_group: bool, lost_since_last_read: int = 0
) -> "Message[PacketsUnitHeld]":
    return Message(
        payload=PacketsUnitHeld(payload),
        timestamp_ns=0,
        starts_group=starts_group,
        lost_since_last_read=lost_since_last_read,
        inbound_link="encoder/encoded_video",
    )


def test_a_reported_loss_discards_to_the_next_group_start(recording_whip_session):
    """After a loss the next delta frame references a picture the far end never
    received; the group start reporting a loss is itself sent."""
    tracks = TracksPinReplayingMessages(
        ["encoder/encoded_video"],
        H264_VIDEO_STREAM_FACTS,
        [
            video_message(b"keyframe", starts_group=True),
            video_message(b"delta after a loss", starts_group=False, lost_since_last_read=2),
            video_message(b"delta in the broken group", starts_group=False),
            video_message(b"next keyframe", starts_group=True),
            video_message(b"keyframe reporting a loss", starts_group=True, lost_since_last_read=1),
            video_message(b"delta after it", starts_group=False),
        ],
    )
    context = cast(Any, None)
    publisher = construct_class_node(
        WhipPublisher, field_values=AN_ENDPOINT, pins={TRACKS_INPUT_PORT: tracks}
    )
    publisher.setup(context)

    for _ in range(6):
        publisher.process(context)

    (session,) = recording_whip_session.constructed
    assert [access_unit for access_unit, _ in session.video_access_units] == [
        b"keyframe",
        b"next keyframe",
        b"keyframe reporting a loss",
        b"delta after it",
    ]


class PlayerWiredToBothDecoders:
    """A `WhepPlayer`'s two output ports, each wired to a decoder's input read here."""

    def __init__(self, request: pytest.FixtureRequest) -> None:
        unique = f"whepwired{os.getpid()}_{request.node.name}"
        self.parent = ParentHoldingStreamFacts()
        destination_notify_service_name = f"{unique}_dest/notify"
        decoder_inputs = _engine.open_node_link_data_access_for_helper_process()
        player_outputs = _engine.open_node_link_data_access_for_helper_process()
        output_ports = (ENCODED_VIDEO_OUTPUT_PORT, ENCODED_AUDIO_OUTPUT_PORT)
        player_outputs.declare_ports([], list(output_ports))
        for output_port in output_ports:
            wire_one_link(
                player_outputs,
                output_port,
                decoder_inputs,
                output_port,
                f"{unique}/{output_port}",
                destination_notify_service_name,
                notify_max_notifiers=len(output_ports),
            )
        self.decoders = _engine.open_runtime_context_full_access_for_helper_process(
            {}, decoder_inputs, "runtime-under-test", "decoders-under-test"
        )
        self.player_context = _engine.open_runtime_context_full_access_for_helper_process(
            {},
            player_outputs,
            "runtime-under-test",
            "player-under-test",
            self.parent.escalate_request_to_parent,
        )


@pytest.fixture
def player_wired_to_both_decoders(request, monkeypatch) -> PlayerWiredToBothDecoders:
    route_every_log_level_to(monkeypatch, discard_log_record)
    return PlayerWiredToBothDecoders(request)


def test_a_started_player_lends_both_outputs_so_python_cannot_write_them(
    player_wired_to_both_decoders,
):
    """The reading thread writes both ports through the module host table with
    no Python attached, so a Python write to either is refused naming it."""
    player = a_node_built_with(WhepPlayer, {"url": "https://example.invalid/whep"})
    context = player_wired_to_both_decoders.player_context
    player.start(context)
    try:
        for output_port in (ENCODED_VIDEO_OUTPUT_PORT, ENCODED_AUDIO_OUTPUT_PORT):
            with pytest.raises(Exception, match=output_port):
                context.outputs.write(output_port, b"from python")
    finally:
        player.stop(context)


def test_a_player_whose_endpoint_never_answers_stops_inside_its_join_budget(
    player_wired_to_both_decoders,
):
    """The thread reconnects with backoff against an endpoint that never
    answers, and a stop wakes it out of the backoff rather than waiting it out."""
    player = a_node_built_with(WhepPlayer, {"url": "https://example.invalid/whep"})
    context = player_wired_to_both_decoders.player_context
    player.start(context)
    time.sleep(0.2)
    stop_began = time.monotonic()
    player.stop(context)
    assert time.monotonic() - stop_began < READER_THREAD_JOIN_TIMEOUT_SECONDS + 0.5
    assert player._reading_thread is None


def test_a_player_never_started_stops_and_tears_down_cleanly(
    player_wired_to_both_decoders,
):
    player = a_node_built_with(WhepPlayer, {"url": "https://example.invalid/whep"})
    context = player_wired_to_both_decoders.player_context
    player.stop(context)
    player.teardown(context)


def test_the_distribution_declares_both_nodes_for_the_node_catalog():
    from importlib.metadata import distribution

    declared = {
        entry_point.name: entry_point
        for entry_point in distribution("tatolab-webrtc").entry_points
        if entry_point.group == "tatolab.nodes"
    }

    assert set(declared) == {"WhipPublisher", "WhepPlayer"}
    for node_class in (WhipPublisher, WhepPlayer):
        entry_point = declared[node_class.__name__]
        assert entry_point.value == f"{node_class.__module__}:{node_class.__qualname__}"
        assert entry_point.load() is node_class
