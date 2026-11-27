# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


import dataclasses
import os
import time
from collections.abc import Buffer, Callable, Iterator
from typing import Any

import pytest
from helper_process_link_wiring import wire_one_link
from parent_stream_facts_stand_in import ParentHoldingStreamFacts
from played_media_stand_ins import (
    ANNEX_B_ACCESS_UNIT,
    MONO_OPUS_IDENTIFICATION_HEADER_HEX,
    OPUS_PACKET,
)
from tatolab.runtime._node_the_wheel_hosts import link_input_data_reader_of, link_output_data_writer_of
from tatolab.runtime import _engine
from tatolab.runtime._class_node_bound_pins import bind_the_pins_of_a_class_node
from tatolab.runtime._node_function_running import NodeFunctionRunning
from tatolab.stream import (
    Message,
    log,
)
from tatolab.stream._node_declaration import NODE_DECLARATION_INPUT_PORTS_STAMP
from tatolab.stream.stream_facts import PacketsStreamFacts
from tatolab.webrtc import WhepPlayer, WhipPublisher, WhipSessionPublishing
from tatolab.webrtc.nodes import (
    ENCODED_AUDIO_OUTPUT_PORT,
    ENCODED_VIDEO_OUTPUT_PORT,
    TRACKS_INPUT_PORT,
    EndpointUrlNotSet,
    PublishedTrack,
    VideoOrAudio,
    _native,
    _required_url,
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


def frame_once_every_computed_value_is_built(
    running: NodeFunctionRunning, frame_ctx: Any, timeout_s: float = 5.0
) -> None:
    """Run one frame of `running`, waiting for the builds its body declared, which run off the
    frame thread, to give their first values."""
    deadline = time.monotonic() + timeout_s
    while not running._running.run_frame(lambda: running._per_frame(frame_ctx)):
        assert time.monotonic() < deadline, "the node's computed values were never built"
        time.sleep(0.005)


def wait_until(condition: "Callable[[], bool]", what: str, timeout_s: float = 5.0) -> None:
    deadline = time.monotonic() + timeout_s
    while not condition():
        assert time.monotonic() < deadline, f"never: {what}"
        time.sleep(0.005)


class PublisherWiredToEncoders:
    """A `WhipPublisher` whose `tracks` input one link per encoder port feeds, run as its helper
    runs it.

    A helper-process context opened directly on wired links is the same seam a node's setup and
    frames see in a real child, and it needs no runtime and no device.
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
            self.encoder_outputs, "runtime-under-test", "encoders-under-test"
        )
        self.context = _engine.open_runtime_context_full_access_for_helper_process(
            tracks_input,
            "runtime-under-test",
            "node-under-test",
            self.parent.escalate_request_to_parent,
        )
        # Bound the way the helper binds them: `tracks` reads through the port the links feed.
        pins = bind_the_pins_of_a_class_node(
            WhipPublisher.members,
            link_data_access=tracks_input,
            inputs=link_input_data_reader_of(self.context),
            outputs=link_output_data_writer_of(self.context),
            node_id="node-under-test",
        )
        self.tracks = pins[TRACKS_INPUT_PORT]
        self.publisher = NodeFunctionRunning(
            WhipPublisher, values_to_build_with=config, pins=pins
        )
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
        link_output_data_writer_of(self.encoder_context).write(
            encoder_output_port, payload, timestamp_ns, starts_group, kind="packets_unit"
        )
        frame_once_every_computed_value_is_built(
            self.publisher,
            _engine.limited_access_view_of_runtime_context_for_helper_process(self.context),
        )

    def stop(self) -> None:
        self.publisher.stop(self.context)


@pytest.fixture
def publishers_to_stop(monkeypatch) -> "Iterator[list[PublisherWiredToEncoders]]":
    route_every_log_level_to(monkeypatch, discard_log_record)
    publishers: "list[PublisherWiredToEncoders]" = []
    yield publishers
    for publisher in publishers:
        publisher.stop()


def a_publisher_wired_to(
    request: pytest.FixtureRequest,
    publishers_to_stop: "list[PublisherWiredToEncoders]",
    encoder_output_ports: "list[str]",
    config: "dict[str, Any]",
) -> PublisherWiredToEncoders:
    wired = PublisherWiredToEncoders(request, encoder_output_ports, config)
    publishers_to_stop.append(wired)
    return wired


def numbered_encoder_output_ports(count: int) -> "list[str]":
    return [f"encoder{index}_output" for index in range(count)]


AN_ENDPOINT = {"url": "https://example.invalid/whip"}


@pytest.mark.parametrize("inbound_links", [1, 2])
def test_a_publisher_sets_up_against_the_links_a_session_can_carry(
    request, publishers_to_stop, inbound_links
):
    wired = a_publisher_wired_to(
        request, publishers_to_stop, numbered_encoder_output_ports(inbound_links), AN_ENDPOINT
    )
    WhipSessionPublishing(AN_ENDPOINT["url"], None).set_up(wired.tracks)


def test_a_publisher_with_nothing_wired_refuses_rather_than_publishing_silence(
    request, publishers_to_stop
):
    """A graph that forgot the connect would otherwise open a session and send
    nothing, which looks from the relay's side like a working publisher."""
    wired = a_publisher_wired_to(
        request, publishers_to_stop, numbered_encoder_output_ports(0), AN_ENDPOINT
    )
    with pytest.raises(ValueError, match="nothing is connected"):
        WhipSessionPublishing(AN_ENDPOINT["url"], None).set_up(wired.tracks)


def test_more_links_than_a_session_can_carry_are_refused_naming_the_count(
    request, publishers_to_stop
):
    wired = a_publisher_wired_to(
        request, publishers_to_stop, numbered_encoder_output_ports(3), AN_ENDPOINT
    )
    with pytest.raises(ValueError, match="3 links feed"):
        WhipSessionPublishing(AN_ENDPOINT["url"], None).set_up(wired.tracks)


@pytest.mark.parametrize("node_function", [WhipPublisher, WhepPlayer])
def test_an_unset_endpoint_is_refused_by_its_own_kind(node_function):
    with pytest.raises(EndpointUrlNotSet, match="`url` is not set"):
        _required_url("", node_function.__name__)


def the_value_inputs_of(node_function: Any) -> "dict[str, dict[str, Any]]":
    return {
        port["name"]: port
        for port in getattr(node_function, NODE_DECLARATION_INPUT_PORTS_STAMP)
        if port["name"] in ("url", "bearer_token")
    }


@pytest.mark.parametrize("node_function", [WhipPublisher, WhepPlayer])
def test_the_endpoint_and_its_token_take_a_wire_or_a_value_set_live(node_function):
    """Mental-revert: declare either one a setting read at setup again, so a change rebuilds the
    node, and its port says so."""
    value_inputs = the_value_inputs_of(node_function)
    assert {name: port["input_mode"] for name, port in value_inputs.items()} == {
        "url": {"mode": "wire_or_value"},
        "bearer_token": {"mode": "wire_or_value"},
    }
    assert {name: port["pin_default"] for name, port in value_inputs.items()} == {
        "url": "",
        "bearer_token": None,
    }


def test_the_publishers_tracks_take_h264_video_and_opus_audio_on_one_pin():
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


#: An endpoint `RecordingWhipSession` refuses to connect to, as a relay refuses a session.
A_REFUSING_ENDPOINT = "https://refusing.example.invalid/whip"


class RecordingWhipSession:
    """A `_native.WhipSession` stand-in keeping what the publisher offered and sent."""

    constructed: "list[RecordingWhipSession]" = []

    def __init__(self, endpoint_url: str, bearer_token: "str | None" = None) -> None:
        del bearer_token
        self.endpoint_url = endpoint_url
        self.is_connected = False
        self.closed = False
        self.offered: "tuple[bool, bool, int | None] | None" = None
        self.video_access_units: "list[tuple[bytes, int]]" = []
        self.audio_opus_packets: "list[bytes]" = []
        RecordingWhipSession.constructed.append(self)

    def connect(
        self, *, video: bool, audio: bool, audio_channels: "int | None" = None
    ) -> None:
        self.offered = (video, audio, audio_channels)
        if self.endpoint_url == A_REFUSING_ENDPOINT:
            raise _native.WhipSessionNotOpened("WHIP signalling failed: 403 Forbidden")
        self.is_connected = True

    def write_video_access_unit(self, annex_b_access_unit: Buffer, timestamp_ns: int) -> None:
        self.video_access_units.append((bytes(annex_b_access_unit), timestamp_ns))

    def write_audio_opus_packet(self, opus_packet: Buffer) -> None:
        self.audio_opus_packets.append(bytes(opus_packet))

    def close(self) -> None:
        self.is_connected = False
        self.closed = True


def route_every_log_level_to(
    monkeypatch: pytest.MonkeyPatch, log_record_sink: "Callable[..., None]"
) -> None:
    """`log` reaches a sink only in the interpreter the runtime starts for a node."""
    for log_level in ("debug", "info", "warn", "error"):
        monkeypatch.setattr(log, log_level, log_record_sink)


def discard_log_record(message: str, **attributes: Any) -> None:
    del message, attributes


@pytest.fixture
def recording_whip_session(monkeypatch) -> "type[RecordingWhipSession]":
    RecordingWhipSession.constructed = []
    monkeypatch.setattr(_native, "WhipSession", RecordingWhipSession)
    route_every_log_level_to(monkeypatch, discard_log_record)
    return RecordingWhipSession


def test_an_access_unit_reaches_the_session_whole_with_its_envelope_stamp(
    request, publishers_to_stop, recording_whip_session
):
    """The envelope's stamp — the one a local reader is promised — places it
    on the RTP clock."""
    wired = a_publisher_wired_to(request, publishers_to_stop, ["encoded_video"], AN_ENDPOINT)
    wired.state_stream_facts("encoded_video", H264_VIDEO_STREAM_FACTS)

    wired.publish("encoded_video", ANNEX_B_ACCESS_UNIT, timestamp_ns=1_234)

    (session,) = recording_whip_session.constructed
    assert session.offered == (True, False, None)
    assert session.video_access_units == [(ANNEX_B_ACCESS_UNIT, 1_234)]


def test_an_opus_packet_offers_the_channel_count_its_stream_facts_name(
    request, publishers_to_stop, recording_whip_session
):
    wired = a_publisher_wired_to(request, publishers_to_stop, ["encoded_audio"], AN_ENDPOINT)
    wired.state_stream_facts("encoded_audio", opus_stream_facts_of(2))

    wired.publish("encoded_audio", OPUS_PACKET)

    (session,) = recording_whip_session.constructed
    assert session.offered == (False, True, 2)
    assert session.audio_opus_packets == [OPUS_PACKET]


def test_two_links_are_one_video_and_one_audio_track_by_their_stream_facts(
    request, publishers_to_stop, recording_whip_session
):
    wired = a_publisher_wired_to(
        request, publishers_to_stop, ["encoded_video", "encoded_audio"], AN_ENDPOINT
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
    request, publishers_to_stop, recording_whip_session
):
    """Any port can be wired into `tracks`, and the refusal is the only place
    that wiring mistake is caught."""
    wired = a_publisher_wired_to(request, publishers_to_stop, ["frames"], AN_ENDPOINT)

    with pytest.raises(ValueError, match="carries no Packets stream facts") as refused:
        wired.publish("frames", ANNEX_B_ACCESS_UNIT)

    inbound_link = wired.inbound_link_by_encoder_output_port["frames"]
    assert f"`{inbound_link}`" in str(refused.value)
    assert recording_whip_session.constructed[0].offered is None


def test_multichannel_stream_facts_are_refused_before_any_session_is_opened(
    request, publishers_to_stop
):
    """The ordering, not just the refusal.

    Refusing after the connect would leave a live session at the relay that
    then publishes nothing — the phantom publisher the refusal exists to
    prevent. The endpoint here is unresolvable, so a connect attempted first
    would raise about signalling instead of about channels.
    """
    wired = a_publisher_wired_to(
        request,
        publishers_to_stop,
        ["encoded_audio"],
        {"url": "https://unresolvable.invalid/whip"},
    )
    wired.state_stream_facts("encoded_audio", opus_stream_facts_of(6))

    with pytest.raises(ValueError, match="mono and stereo only"):
        wired.publish("encoded_audio", OPUS_PACKET)


def test_a_video_link_is_entered_at_its_first_group_start(
    request, publishers_to_stop, recording_whip_session
):
    """A delta frame before the first keyframe references pictures the far end
    never had."""
    wired = a_publisher_wired_to(request, publishers_to_stop, ["encoded_video"], AN_ENDPOINT)
    wired.state_stream_facts("encoded_video", H264_VIDEO_STREAM_FACTS)

    wired.publish("encoded_video", b"\x01delta", timestamp_ns=1, starts_group=False)
    wired.publish("encoded_video", b"\x02keyframe", timestamp_ns=2, starts_group=True)
    wired.publish("encoded_video", b"\x03delta", timestamp_ns=3, starts_group=False)

    (session,) = recording_whip_session.constructed
    assert session.video_access_units == [(b"\x02keyframe", 2), (b"\x03delta", 3)]


A_SECOND_ENDPOINT = "https://second.example.invalid/whip"


def test_a_new_endpoint_opens_beside_the_publishing_session_with_no_stop_and_takes_over(
    request, publishers_to_stop, recording_whip_session
):
    """Mental-revert: read `url` at setup again, and the new value reaches no session until the
    node is built again; open the new session at its first frame, and its refusal could not
    leave the old one publishing."""
    wired = a_publisher_wired_to(request, publishers_to_stop, ["encoded_video"], AN_ENDPOINT)
    wired.state_stream_facts("encoded_video", H264_VIDEO_STREAM_FACTS)
    wired.publish("encoded_video", b"\x01keyframe", timestamp_ns=1)
    (first,) = recording_whip_session.constructed

    wired.publisher.write_inputs({"url": A_SECOND_ENDPOINT})
    wait_until(
        lambda: len(recording_whip_session.constructed) == 2
        and recording_whip_session.constructed[1].is_connected,
        "the second session connected as it was built, off the frames",
    )
    (_, second) = recording_whip_session.constructed
    assert second.endpoint_url == A_SECOND_ENDPOINT
    assert second.offered == (True, False, None)

    wired.publish("encoded_video", b"\x02delta", timestamp_ns=2, starts_group=False)
    wired.publish("encoded_video", b"\x03keyframe", timestamp_ns=3)

    wait_until(lambda: first.closed, "the first session closed once frames moved to the second")
    assert first.video_access_units == [(b"\x01keyframe", 1)]
    assert second.video_access_units == [(b"\x03keyframe", 3)], "entered at its group start"


def test_a_new_endpoint_that_refuses_its_session_leaves_the_old_one_publishing(
    request, publishers_to_stop, recording_whip_session
):
    wired = a_publisher_wired_to(request, publishers_to_stop, ["encoded_video"], AN_ENDPOINT)
    wired.state_stream_facts("encoded_video", H264_VIDEO_STREAM_FACTS)
    wired.publish("encoded_video", b"\x01keyframe", timestamp_ns=1)
    (first,) = recording_whip_session.constructed

    wired.publisher.write_inputs({"url": A_REFUSING_ENDPOINT})
    wait_until(
        lambda: len(recording_whip_session.constructed) == 2
        and recording_whip_session.constructed[1].closed,
        "the refused session was closed by its build",
    )
    wired.publish("encoded_video", b"\x02delta", timestamp_ns=2, starts_group=False)

    assert first.is_connected
    assert first.video_access_units == [(b"\x01keyframe", 1), (b"\x02delta", 2)]


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
    tracks: Any = TracksPinReplayingMessages(
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
    publishing = WhipSessionPublishing(AN_ENDPOINT["url"], None)
    publishing.set_up(tracks)

    for _ in range(6):
        publishing.publish_the_next_message(tracks)

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
            decoder_inputs, "runtime-under-test", "decoders-under-test"
        )
        self.player_context = _engine.open_runtime_context_full_access_for_helper_process(
            player_outputs,
            "runtime-under-test",
            "player-under-test",
            self.parent.escalate_request_to_parent,
        )


@pytest.fixture
def player_wired_to_both_decoders(request, monkeypatch) -> PlayerWiredToBothDecoders:
    route_every_log_level_to(monkeypatch, discard_log_record)
    return PlayerWiredToBothDecoders(request)


#: How long letting go of the reading thread waits for it, as the wheel's Rust waits.
READING_THREAD_END_WAIT_SECONDS = 2.0

AN_UNANSWERING_WHEP_ENDPOINT = "https://example.invalid/whep"


def a_player(**input_values: object) -> NodeFunctionRunning:
    """A `WhepPlayer` as its helper runs it, each output bound to a stand-in: its thread writes
    the ports it is lent."""
    return NodeFunctionRunning(
        WhepPlayer,
        values_to_build_with=input_values,
        pins={ENCODED_VIDEO_OUTPUT_PORT: object(), ENCODED_AUDIO_OUTPUT_PORT: object()},
    )


def stopped_as_its_helper_stops_it(node: NodeFunctionRunning, context: object) -> None:
    """Note the parent's stop on the context, which the lent thread reads, then stop."""
    _engine.note_stop_request_from_parent_on_runtime_context(context, True)
    node.stop(context)


def test_a_set_up_player_lends_both_outputs_so_python_cannot_write_them(
    player_wired_to_both_decoders,
):
    """The reading thread writes both ports through the module host table with
    no Python attached, so a Python write to either is refused naming it."""
    player = a_player(url=AN_UNANSWERING_WHEP_ENDPOINT)
    context = player_wired_to_both_decoders.player_context
    player.setup(context)
    try:
        for output_port in (ENCODED_VIDEO_OUTPUT_PORT, ENCODED_AUDIO_OUTPUT_PORT):
            with pytest.raises(Exception, match=output_port):
                link_output_data_writer_of(context).write(output_port, b"from python")
    finally:
        stopped_as_its_helper_stops_it(player, context)


def test_a_player_whose_endpoint_never_answers_stops_inside_its_budget(
    player_wired_to_both_decoders,
):
    """The thread redials the first endpoint with backoff while it never answers, and a stop
    wakes it out of the backoff rather than waiting it out."""
    player = a_player(url=AN_UNANSWERING_WHEP_ENDPOINT)
    context = player_wired_to_both_decoders.player_context
    player.setup(context)
    time.sleep(0.2)
    stop_began = time.monotonic()
    stopped_as_its_helper_stops_it(player, context)
    player.teardown(context)
    assert time.monotonic() - stop_began < READING_THREAD_END_WAIT_SECONDS + 0.5


def test_a_player_never_set_up_stops_and_tears_down_cleanly(player_wired_to_both_decoders):
    player = a_player(url=AN_UNANSWERING_WHEP_ENDPOINT)
    context = player_wired_to_both_decoders.player_context
    player.stop(context)
    player.teardown(context)


def test_an_endpoint_refused_beside_an_open_one_raises_its_own_kind_and_the_first_plays_on(
    player_wired_to_both_decoders,
):
    """The first endpoint is redialled by the thread, since a WHEP endpoint refuses until its
    publisher starts; one opened beside it is the opening's to refuse."""
    context = player_wired_to_both_decoders.player_context
    reading_thread = _native.WhepPlayerThread(
        context.open_module_host_table(
            output_ports=[ENCODED_VIDEO_OUTPUT_PORT, ENCODED_AUDIO_OUTPUT_PORT]
        )
    )
    try:
        first = reading_thread.open_session(AN_UNANSWERING_WHEP_ENDPOINT)
        with pytest.raises(_native.WhepSessionNotOpened, match="the one playing keeps playing"):
            reading_thread.open_session("https://second.example.invalid/whep")
        first.close()
        first.close()
    finally:
        _engine.note_stop_request_from_parent_on_runtime_context(context, True)
        del reading_thread


def test_the_distribution_declares_both_nodes_for_the_node_catalog():
    from importlib.metadata import distribution

    declared = {
        entry_point.name: entry_point
        for entry_point in distribution("tatolab-webrtc").entry_points
        if entry_point.group == "tatolab.nodes"
    }

    assert set(declared) == {"WhipPublisher", "WhepPlayer"}
    for node_function in (WhipPublisher, WhepPlayer):
        entry_point = declared[node_function.__name__]
        assert entry_point.value == f"{node_function.__module__}:{node_function.__qualname__}"
        assert entry_point.load() is node_function
