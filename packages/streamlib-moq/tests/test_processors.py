# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The two processors as a graph sees them.

`rt.add` with no adapter and no engine change is the whole claim of the
extension model, so it is what these check — over a real `Runtime`, which needs
no device to build a graph. Nothing here reaches a relay.
"""

from dataclasses import dataclass
from typing import Any
from unittest import mock

import pytest

import streamlib
from streamlib import log
from streamlib import (
    H264Decoder,
    H264Encoder,
    OpusDecoder,
    OpusEncoder,
    RuntimeContextLimitedAccess,
    decode_msgpack_bytes_to_python_object,
    encode_bag_to_msgpack_bytes,
    input,
    output,
    processor,
)
from streamlib_moq import MoqBroadcastPublisher, MoqBroadcastSubscriber, _native
from streamlib_moq import processors as processors_module
from streamlib_moq.processors import (
    BAGS_BETWEEN_PROGRESS_REPORTS,
    CONTAINER_FORMATS,
    DATA_BAGS_OUTPUT_PORT,
    DATA_OBJECT_BAG_KEY,
    DATA_OBJECT_SEQUENCE_INDEX_KEY,
    DATA_OBJECT_TIMESTAMP_NS_KEY,
    DATA_TRACK_KIND,
    HELPER_LINK_FRAME_HEADER_BYTES,
    HELPER_LINK_PAYLOAD_CEILING_BYTES,
    READER_THREAD_JOIN_TIMEOUT_SECONDS,
    SUBSCRIBER_POLL_TIMEOUT_MS,
    WIDEST_THE_ENGINE_RE_ENCODES_A_WIRE_VALUE,
    DataTrackSequenceGapCount,
    data_object_envelope_of,
    data_track_object_bytes,
    describe_the_delivery_deadline,
    describe_what_the_delivery_deadline_shed,
    framed_bag_byte_count,
    the_bitstream_alone_puts_the_bag_near_the_link_ceiling,
    the_envelope_alone_could_put_the_bag_past_the_link_ceiling,
    track_kind_of_bag,
    track_medium_of_codec,
)

A_RELAY = "https://relay.invalid/a-token"
A_BROADCAST = "streamlib/a-broadcast"

PUBLISHER_CONFIG = {"relay_url": A_RELAY}
A_VIDEO_BAG = {
    "codec": "h264",
    "bitstream": b"\x00\x00\x00\x01\x65",
    "is_sync_point": True,
    "group_index": 0,
    "sequence_index": 0,
    "width": 320,
    "height": 180,
}
A_DATA_BAG = {"frame": 3, "note": "hi", "blob": b"\x00\x01", "nested": {"a": [1, 2.5, None]}}


@processor(
    execution="continuous",
    interval_ms=100,
    description="Writes one telemetry bag per tick, as a graph's own data source",
)
class TelemetryProbe:
    @output()
    def telemetry(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.outputs.write("telemetry", {"frame": 1, "note": "hi", "blob": b"\x00"})


SUBSCRIBER_CONFIG = {
    "relay_url": A_RELAY,
    "broadcast": A_BROADCAST,
    "video_track": "1.m4s",
    "audio_track": "2.m4s",
}
A_DATA_TRACK_SUBSCRIBER_CONFIG = {
    "relay_url": A_RELAY,
    "broadcast": A_BROADCAST,
    "container_format": "streamlib_bag",
    "data_track": "telemetry",
}
A_DATA_ENVELOPE = {"sequence_index": 7, "timestamp_ns": 5_000_000_000, "bag": A_DATA_BAG}


@processor(description="Reads the data bags a subscriber writes, as a graph's own sink")
class TelemetryReader:
    @input(delivery_profile="ordered")
    def data_bags(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.inputs.read("data_bags")


@pytest.fixture
def runtime():
    runtime = streamlib.Runtime()
    try:
        yield runtime
    finally:
        runtime.shutdown()


@pytest.mark.parametrize(
    ("processor_class", "config"),
    [(MoqBroadcastPublisher, PUBLISHER_CONFIG), (MoqBroadcastSubscriber, SUBSCRIBER_CONFIG)],
)
def test_an_installed_extensions_processor_is_added_like_any_other(
    runtime, processor_class, config
):
    added = runtime.add(processor_class, config=config)

    assert added.display_name == processor_class.__name__


def test_the_publisher_wires_to_both_encoders_without_an_adapter(runtime):
    """One fan-in port takes both encoders, which is what makes a broadcast
    with video and audio a matter of wiring rather than of config."""
    video_encoder = runtime.add(H264Encoder)
    audio_encoder = runtime.add(OpusEncoder)
    publisher = runtime.add(MoqBroadcastPublisher, config=PUBLISHER_CONFIG)

    runtime.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    runtime.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))


def test_a_data_producing_processor_wires_into_the_publisher_beside_both_encoders(runtime):
    """A data track is a matter of wiring: any processor's output into the
    same fan-in port the encoders feed, with the tracks named in that order."""
    video_encoder = runtime.add(H264Encoder)
    audio_encoder = runtime.add(OpusEncoder)
    probe = runtime.add(TelemetryProbe)
    publisher = runtime.add(
        MoqBroadcastPublisher,
        config={
            **PUBLISHER_CONFIG,
            "container_format": "streamlib_bag",
            "track_names": ["video", "audio", "telemetry"],
        },
    )

    runtime.connect(video_encoder.output("encoded_video"), publisher.input("tracks"))
    runtime.connect(audio_encoder.output("encoded_audio"), publisher.input("tracks"))
    runtime.connect(probe.output("telemetry"), publisher.input("tracks"))


def test_the_subscriber_wires_to_both_decoders_without_an_adapter(runtime):
    subscriber = runtime.add(MoqBroadcastSubscriber, config=SUBSCRIBER_CONFIG)
    video_decoder = runtime.add(H264Decoder)
    audio_decoder = runtime.add(OpusDecoder)

    runtime.connect(
        subscriber.output("encoded_video"), video_decoder.input("encoded_video")
    )
    runtime.connect(
        subscriber.output("encoded_audio"), audio_decoder.input("encoded_audio")
    )


@pytest.mark.parametrize("container_format", CONTAINER_FORMATS)
def test_both_container_formats_are_addable(runtime, container_format):
    added = runtime.add(
        MoqBroadcastPublisher,
        config={**PUBLISHER_CONFIG, "container_format": container_format},
    )

    assert added.display_name == "MoqBroadcastPublisher"


def test_a_container_format_this_wheel_does_not_write_is_refused_by_name():
    with pytest.raises(ValueError, match="container_format"):
        MoqBroadcastPublisher(relay_url=A_RELAY, container_format="mpegts")  # type: ignore[arg-type]


def test_a_publisher_without_a_relay_is_refused_by_name():
    with pytest.raises(ValueError, match="relay_url"):
        MoqBroadcastPublisher(relay_url="")


def test_the_relay_refusal_says_where_a_draft_16_token_goes():
    """Draft-16 provisions relays per account and carries the token in the URL
    path, so a bare host is not a usable endpoint and the message has to say
    so — there is nowhere else to learn it."""
    with pytest.raises(ValueError, match="token"):
        MoqBroadcastSubscriber(relay_url="", broadcast=A_BROADCAST, video_track="1.m4s")


def test_a_subscriber_naming_no_track_at_all_is_refused_by_name():
    """Three static output ports and no track named for any would subscribe to
    nothing and produce nothing, which reads from outside as a hang."""
    with pytest.raises(ValueError, match="video_track.*audio_track.*data_track"):
        MoqBroadcastSubscriber(relay_url=A_RELAY, broadcast=A_BROADCAST)


def test_a_subscriber_may_name_one_track_and_leave_the_other_ports_silent():
    MoqBroadcastSubscriber(
        relay_url=A_RELAY, broadcast=A_BROADCAST, video_track="1.m4s"
    )


def test_a_subscriber_naming_only_a_data_track_is_added_like_any_other(runtime):
    added = runtime.add(MoqBroadcastSubscriber, config=A_DATA_TRACK_SUBSCRIBER_CONFIG)

    assert added.display_name == "MoqBroadcastSubscriber"


def test_the_subscribers_data_bags_port_wires_to_a_processor_that_reads_it(runtime):
    """A data track is a matter of wiring on this side too: a static port a
    downstream names at wiring time, beside the two decoders' ports."""
    subscriber = runtime.add(MoqBroadcastSubscriber, config=A_DATA_TRACK_SUBSCRIBER_CONFIG)
    reader = runtime.add(TelemetryReader)

    runtime.connect(subscriber.output("data_bags"), reader.input("data_bags"))


def test_a_data_track_beside_both_media_tracks_is_accepted_under_streamlib_bag():
    MoqBroadcastSubscriber(
        relay_url=A_RELAY,
        broadcast=A_BROADCAST,
        container_format="streamlib_bag",
        video_track="video",
        audio_track="audio",
        data_track="telemetry",
    )


def test_a_data_track_under_cmaf_is_refused_by_name_at_construction():
    """CMAF has no packaging for a data track, and the default container is
    `cmaf` — so a subscriber that named one and nothing else must hear it
    here, not as a broadcast that never produces."""
    with pytest.raises(ValueError, match="data_track.*cmaf"):
        MoqBroadcastSubscriber(relay_url=A_RELAY, broadcast=A_BROADCAST, data_track="telemetry")


def test_the_documented_envelope_decodes_to_its_three_parts_with_the_bag_whole():
    envelope = data_object_envelope_of(encode_bag_to_msgpack_bytes(A_DATA_ENVELOPE))

    assert (envelope.sequence_index, envelope.timestamp_ns) == (7, 5_000_000_000)
    assert envelope.bag == A_DATA_BAG
    assert type(envelope.bag["blob"]) is bytes


@pytest.mark.parametrize(
    ("payload", "named"),
    [
        (encode_bag_to_msgpack_bytes({"timestamp_ns": 1, "bag": {}}), "`sequence_index`"),
        (encode_bag_to_msgpack_bytes({"sequence_index": 1, "bag": {}}), "`timestamp_ns`"),
        (encode_bag_to_msgpack_bytes({"sequence_index": 1, "timestamp_ns": 1}), "`bag`"),
        (
            encode_bag_to_msgpack_bytes({"sequence_index": 1, "timestamp_ns": 1, "bag": [1]}),
            "`bag` is a list",
        ),
        (
            encode_bag_to_msgpack_bytes({"sequence_index": True, "timestamp_ns": 1, "bag": {}}),
            "`sequence_index` is True",
        ),
        (
            encode_bag_to_msgpack_bytes({"sequence_index": 1, "timestamp_ns": "now", "bag": {}}),
            "`timestamp_ns` is 'now'",
        ),
        # A msgpack array of three, and a map cut off after its header.
        (b"\x93\x01\x02\x03", "is a list, not the map"),
        (b"\x81", "not msgpack"),
        # Keys the engine's decoder accepts and its encoder refuses, spelled
        # by hand: an int key, one inside a list of maps, and a non-UTF-8 key
        # the decoder hands back as bytes.
        (
            b"\x83\xaesequence_index\x01\xactimestamp_ns\x02\xa3bag\x81\x01\xa1x",
            r"not a str at `bag`: 1 \(int\)",
        ),
        (
            b"\x83\xaesequence_index\x01\xactimestamp_ns\x02\xa3bag"
            b"\x81\xa4rows\x91\x81\x01\xa1x",
            r"not a str at `bag\.rows\[0\]`: 1 \(int\)",
        ),
        (
            b"\x83\xaesequence_index\x01\xactimestamp_ns\x02\xa3bag\x81\xa1\xff\xa1x",
            r"not a str at `bag`: b'\\xff' \(bytes\)",
        ),
    ],
)
def test_an_object_that_is_not_the_envelope_is_refused_naming_what_is_wrong(payload, named):
    """The publisher writes the envelope and the subscriber reads it, and the
    two meet only on the wire — so a refusal has to say which key drifted, or
    an operator cannot tell which side did."""
    with pytest.raises(ValueError, match=named):
        data_object_envelope_of(payload)


def test_the_gap_count_counts_jumps_and_the_objects_they_skipped():
    count = DataTrackSequenceGapCount()
    for sequence_index in (5, 6, 7, 10, 11, 20):
        count.account(sequence_index)

    assert (count.gaps, count.objects_missed) == (2, 10)


def test_a_first_object_is_not_a_gap_however_far_into_the_stream_the_subscriber_joined():
    count = DataTrackSequenceGapCount()
    count.account(500)
    count.account(501)

    assert (count.gaps, count.objects_missed) == (0, 0)


def test_a_publisher_that_restarted_its_count_is_not_read_as_a_loss():
    count = DataTrackSequenceGapCount()
    for sequence_index in (8, 9, 0, 1):
        count.account(sequence_index)

    assert (count.gaps, count.objects_missed) == (0, 0)


def test_a_reconnected_session_starts_its_count_afresh_and_keeps_what_was_lost():
    count = DataTrackSequenceGapCount()
    count.account(0)
    count.account(3)
    count.forget_the_closed_broadcasts_count()
    count.account(100)

    assert (count.gaps, count.objects_missed) == (1, 2)


def _a_data_object(payload: bytes) -> _native.ReceivedDataObject:
    return _native.ReceivedDataObject("telemetry", payload)


@dataclass(frozen=True)
class _AnAccessUnit:
    """What the native layer hands the media path, stated by a test."""

    codec: str = "h264"
    bitstream: bytes = A_VIDEO_BAG["bitstream"]
    is_sync_point: bool = True
    group_index: int = 0
    sequence_index: int = 0
    width: int = 320
    height: int = 180
    color: "dict[str, str] | None" = None
    timestamp_ns: int = 1


class _OutputsRecordingWrites:
    def __init__(self) -> None:
        self.writes: "list[tuple[str, dict[str, Any], int | None]]" = []

    def write(self, port: str, bag: "dict[str, Any]", timestamp_ns: "int | None" = None) -> None:
        self.writes.append((port, bag, timestamp_ns))


def _a_data_track_subscriber() -> MoqBroadcastSubscriber:
    return MoqBroadcastSubscriber(
        relay_url=A_RELAY,
        broadcast=A_BROADCAST,
        container_format="streamlib_bag",
        data_track="telemetry",
    )


def _an_envelope_of(sequence_index: int) -> bytes:
    return encode_bag_to_msgpack_bytes(
        {"sequence_index": sequence_index, "timestamp_ns": sequence_index, "bag": {"n": sequence_index}}
    )


def test_gaps_are_said_on_the_data_ports_progress_line_at_the_cadence():
    subscriber = _a_data_track_subscriber()
    outputs = _OutputsRecordingWrites()
    said: "list[str]" = []
    # Two objects, then a jump over three, then the rest of one cadence.
    sequence_indices = [0, 1, *range(5, 5 + BAGS_BETWEEN_PROGRESS_REPORTS - 2)]

    with mock.patch.object(log, "info", said.append):
        for sequence_index in sequence_indices:
            subscriber._write_a_data_object(
                _a_data_object(_an_envelope_of(sequence_index)),
                outputs,  # type: ignore[arg-type]
            )

    assert len(outputs.writes) == BAGS_BETWEEN_PROGRESS_REPORTS
    cadence_lines = [
        line for line in said if f"bags_written={BAGS_BETWEEN_PROGRESS_REPORTS}" in line
    ]
    assert len(cadence_lines) == 1, said
    assert "sequence_gaps=1 objects_missed=3" in cadence_lines[0], cadence_lines[0]
    assert "objects_refused=0 on `telemetry`" in cadence_lines[0], cadence_lines[0]


def test_refused_objects_are_said_once_in_full_and_then_counted_at_the_cadence():
    """A far end writing the wrong shape writes it on every object, and a line
    per object would bury the log of a stream that never recovers."""
    subscriber = _a_data_track_subscriber()
    said: "list[str]" = []

    with mock.patch.object(log, "warn", said.append):
        for _ in range(BAGS_BETWEEN_PROGRESS_REPORTS):
            subscriber._write_a_data_object(
                _a_data_object(b"\x93\x01\x02\x03"),
                _OutputsRecordingWrites(),  # type: ignore[arg-type]
            )

    assert len(said) == 2, said
    assert "is a list, not the map" in said[0] and "`telemetry`" in said[0], said[0]
    assert f"objects_refused={BAGS_BETWEEN_PROGRESS_REPORTS}" in said[1], said[1]


def test_stop_says_what_the_data_track_wrote_and_lost_even_when_the_cadence_never_fired():
    subscriber = _a_data_track_subscriber()
    for sequence_index in (0, 4):
        subscriber._write_a_data_object(
            _a_data_object(_an_envelope_of(sequence_index)),
            _OutputsRecordingWrites(),  # type: ignore[arg-type]
        )
    said: "list[str]" = []

    with mock.patch.object(log, "info", said.append):
        subscriber.stop(None)  # type: ignore[arg-type]

    assert any(
        "stop" in line and "bags_written=2" in line and "sequence_gaps=1 objects_missed=3" in line
        for line in said
    ), said


def test_a_subscriber_naming_no_data_track_says_nothing_about_one_at_stop():
    subscriber = MoqBroadcastSubscriber(relay_url=A_RELAY, broadcast=A_BROADCAST, video_track="1.m4s")
    said: "list[str]" = []

    with mock.patch.object(log, "info", said.append):
        subscriber.stop(None)  # type: ignore[arg-type]

    assert said == []


def test_the_oversize_guard_measures_a_data_bag_by_its_envelope_not_by_a_bitstream_it_lacks():
    """The media precheck reads `bitstream` and counts a bag without one as
    near-zero, so a data bag routed through it would never be measured and an
    oversize one would be dropped by the link unsaid. The ceiling is patched
    above the media precheck's margin so that precheck stays silent here and
    only the envelope's own length can make the guard look."""
    subscriber = _a_data_track_subscriber()
    bag = {"blob": b"\x00" * 100_000}
    envelope = encode_bag_to_msgpack_bytes({"sequence_index": 0, "timestamp_ns": 0, "bag": bag})
    a_ceiling_the_media_precheck_never_reaches = 100_000
    said: "list[str]" = []

    with (
        mock.patch.object(
            processors_module,
            "HELPER_LINK_PAYLOAD_CEILING_BYTES",
            a_ceiling_the_media_precheck_never_reaches,
        ),
        mock.patch.object(log, "error", said.append),
    ):
        assert not the_bitstream_alone_puts_the_bag_near_the_link_ceiling(bag)
        subscriber._write_a_data_object(
            _a_data_object(envelope),
            _OutputsRecordingWrites(),  # type: ignore[arg-type]
        )

    assert said and "data_bags" in said[0] and "framed" in said[0], said


def test_a_data_bag_far_under_the_ceiling_is_never_encoded_a_second_time():
    subscriber = _a_data_track_subscriber()
    envelope = encode_bag_to_msgpack_bytes(A_DATA_ENVELOPE)

    with mock.patch.object(
        processors_module, "encode_bag_to_msgpack_bytes"
    ) as the_exact_measure:
        subscriber._write_a_data_object(
            _a_data_object(envelope),
            _OutputsRecordingWrites(),  # type: ignore[arg-type]
        )

    the_exact_measure.assert_not_called()


_THE_WIDEST_ENVELOPE_WHOSE_BAG_SURELY_FITS = (
    HELPER_LINK_PAYLOAD_CEILING_BYTES - HELPER_LINK_FRAME_HEADER_BYTES
) // WIDEST_THE_ENGINE_RE_ENCODES_A_WIRE_VALUE


@pytest.mark.parametrize(
    ("envelope_byte_count", "reaches_the_ceiling"),
    [
        (_THE_WIDEST_ENVELOPE_WHOSE_BAG_SURELY_FITS, False),
        (_THE_WIDEST_ENVELOPE_WHOSE_BAG_SURELY_FITS + 1, True),
    ],
)
def test_the_envelope_bound_reaches_the_ceiling_exactly_where_the_widest_re_encode_would(
    envelope_byte_count, reaches_the_ceiling
):
    assert (
        the_envelope_alone_could_put_the_bag_past_the_link_ceiling(envelope_byte_count)
        is reaches_the_ceiling
    )


def _bytes_a_wire_value_re_encodes_to(wire: bytes) -> int:
    value = decode_msgpack_bytes_to_python_object(wire)
    beside_a_nil = len(encode_bag_to_msgpack_bytes({"v": None}))
    return len(encode_bag_to_msgpack_bytes({"v": value})) - beside_a_nil + 1


@pytest.mark.parametrize(
    ("wire", "what"),
    [
        (b"\xca\x00\x00\x00\x00", "an f32, which re-encodes as an f64"),
        (b"\xd4\x01\x00", "a fixext1, which re-encodes as the ext it came from"),
        (b"\xc7\x01\x05\x00", "an ext8, which re-encodes as a shorter fixext"),
        (b"\xcd\x00\x07", "a uint16 spelled long, which re-encodes as a fixint"),
    ],
)
def test_the_widening_the_bound_allows_for_covers_every_wire_form_the_codec_widens(wire, what):
    """The bound multiplies the envelope by this factor, so a wire form the
    codec re-encodes wider than that would slip an oversize bag past it."""
    assert (
        _bytes_a_wire_value_re_encodes_to(wire)
        <= len(wire) * WIDEST_THE_ENGINE_RE_ENCODES_A_WIRE_VALUE
    ), what


def test_the_widening_is_real_so_the_bound_is_not_a_needless_margin():
    an_f32 = b"\xca\x00\x00\x00\x00"

    assert _bytes_a_wire_value_re_encodes_to(an_f32) > len(an_f32)


class _SessionHandingOverObjectsThenStopping:
    """`next_media` as the drain loop calls it: each object in turn, then a
    `None` that also asks the subscriber to stop."""

    def __init__(self, subscriber: MoqBroadcastSubscriber, objects: "list[Any]") -> None:
        self._subscriber = subscriber
        self._objects = list(objects)

    def next_media(self, timeout_ms: int) -> Any:
        assert timeout_ms == SUBSCRIBER_POLL_TIMEOUT_MS
        if self._objects:
            return self._objects.pop(0)
        self._subscriber._stop.set()
        return None


def test_the_drain_loop_routes_a_received_data_object_to_the_data_path():
    """The one line that decides whether a data object ever reaches the data
    path dispatches on the native type, so it is driven with the real one."""
    subscriber = _a_data_track_subscriber()
    outputs = _OutputsRecordingWrites()
    session = _SessionHandingOverObjectsThenStopping(
        subscriber, [_a_data_object(encode_bag_to_msgpack_bytes(A_DATA_ENVELOPE))]
    )

    subscriber._drain_until_stopped(session, outputs)  # type: ignore[arg-type]

    assert outputs.writes == [(DATA_BAGS_OUTPUT_PORT, A_DATA_BAG, 5_000_000_000)]


@pytest.mark.parametrize(
    ("codec", "medium"),
    [("h264", "video"), ("h265", "video"), ("opus", "audio")],
)
def test_a_bags_codec_settles_which_medium_its_link_carries(codec, medium):
    assert track_medium_of_codec(codec, "encoder/out") == medium


def test_a_codec_this_broadcast_does_not_carry_is_refused_naming_the_link():
    """A graph that wired something other than an encoder into the publisher is
    a wiring mistake, and this message is the only place it is catchable."""
    with pytest.raises(ValueError, match="encoder/out"):
        track_medium_of_codec("vp9", "encoder/out")


def test_a_missing_codec_is_refused_rather_than_guessed_at():
    with pytest.raises(ValueError, match="codec"):
        track_medium_of_codec(None, "encoder/out")


#: A helper's teardown reply and its exit are each bounded at five seconds by
#: the engine, and `stop()` runs inside the first of those.
HELPER_TEARDOWN_BUDGET_SECONDS = 5.0


def test_a_stop_completes_inside_the_helpers_teardown_budget():
    """The two waits a `stop()` can sit through are the reading thread's own
    poll and the join that follows it; together they must leave the budget
    room, or the helper is killed rather than stopped."""
    longest_stop_seconds = (
        SUBSCRIBER_POLL_TIMEOUT_MS / 1000 + READER_THREAD_JOIN_TIMEOUT_SECONDS
    )

    assert longest_stop_seconds < HELPER_TEARDOWN_BUDGET_SECONDS


def test_a_bag_past_the_link_ceiling_is_reported_once_and_not_every_frame():
    """The engine drops an oversized bag at `debug` rather than raising, so a
    subscriber that said nothing would look like a stream that just stopped —
    but saying it per frame would bury the log of a stream that never
    recovers."""
    subscriber = MoqBroadcastSubscriber(
        relay_url=A_RELAY, broadcast=A_BROADCAST, video_track="1.m4s"
    )
    bag = {**A_VIDEO_BAG, "bitstream": b"x" * (HELPER_LINK_PAYLOAD_CEILING_BYTES + 1)}
    said: "list[str]" = []
    with mock.patch.object(log, "error", said.append):
        for _ in range(3):
            subscriber._report_a_bag_the_link_will_drop(
                "encoded_video", bag, the_bitstream_alone_puts_the_bag_near_the_link_ceiling(bag)
            )

    assert len(said) == 1
    assert "Reported once" in said[0]


def test_a_publisher_carrying_a_delivery_deadline_is_added_like_any_other(runtime):
    added = runtime.add(
        MoqBroadcastPublisher,
        config={**PUBLISHER_CONFIG, "delivery_deadline_ms": 250},
    )

    assert added.display_name == "MoqBroadcastPublisher"


@pytest.mark.parametrize("not_a_deadline", ["250", 2.5, True, -1])
def test_a_delivery_deadline_that_is_not_a_count_of_milliseconds_is_refused_by_name(
    not_a_deadline,
):
    """`bool` is an `int` in Python, so `True` would otherwise read as a
    one-millisecond deadline that sheds every frame but the sync points."""
    with pytest.raises(ValueError, match="delivery_deadline_ms"):
        MoqBroadcastPublisher(relay_url=A_RELAY, delivery_deadline_ms=not_a_deadline)


def test_a_run_that_shed_nothing_says_so_rather_than_saying_nothing():
    """A silently shed frame is the failure mode this wheel is careful about,
    so the absence of drops is reported as loudly as their presence."""
    assert (
        describe_what_the_delivery_deadline_shed([])
        == "the delivery deadline shed nothing"
    )


def test_the_shed_report_names_each_link_with_its_objects_and_its_bytes():
    said = describe_what_the_delivery_deadline_shed(
        [("camera", 12, 48213), ("second_camera", 1, 900)]
    )

    assert said == (
        "the delivery deadline shed camera=12 objects/48213 bytes, "
        "second_camera=1 objects/900 bytes"
    )


@pytest.mark.parametrize(
    ("delivery_deadline_ms", "said"),
    [
        (None, "no delivery deadline is configured"),
        (0, "the delivery deadline is 0 ms"),
        (250, "the delivery deadline is 250 ms"),
    ],
)
def test_the_deadline_a_publisher_runs_under_is_said_where_it_is_configured(
    delivery_deadline_ms, said
):
    """A run's log has to state which arm it is, or a measured before/after
    cannot be told apart after the fact."""
    assert describe_the_delivery_deadline(delivery_deadline_ms) == said


class _SessionThatAnswers:
    """A publishing session whose every publish call gives one fixed answer."""

    def __init__(self, reaches_the_transport: bool) -> None:
        self._answer = reaches_the_transport
        self.calls = 0

    def publish_video_access_unit(self, *args, **kwargs) -> bool:
        del args, kwargs
        self.calls += 1
        return self._answer

    def objects_the_delivery_deadline_shed(self) -> "list[tuple[str, int, int]]":
        return [] if self._answer else [("camera", self.calls, 900 * self.calls)]

    def close(self) -> "str | None":
        return None


class _InputsReadingOneVideoBag:
    def read_from_inbound_link_with_timestamp(self, port: str):
        assert port == "tracks"
        return (dict(A_VIDEO_BAG), "camera", 5_000_000_000)


class _ContextReadingOneVideoBag:
    inputs = _InputsReadingOneVideoBag()


class _InputsReadingBagsInTurn:
    """Each read hands over the next `(bag, inbound_link, timestamp_ns)`."""

    def __init__(self, reads: "list[tuple[dict, str, int]]") -> None:
        self._reads = iter(reads)

    def read_from_inbound_link_with_timestamp(self, port: str):
        assert port == "tracks"
        return next(self._reads, None)


class _ContextReadingBagsInTurn:
    def __init__(self, reads: "list[tuple[dict, str, int]]") -> None:
        self.inputs = _InputsReadingBagsInTurn(reads)


class _SessionRecordingWhatWasPublished:
    """A publishing session that keeps every data object it was handed."""

    def __init__(self) -> None:
        self.data_objects: "list[tuple[str, bytes]]" = []
        self.media_calls = 0

    def publish_data_object(self, inbound_link_name: str, object_bytes: bytes) -> None:
        self.data_objects.append((inbound_link_name, object_bytes))

    def publish_video_access_unit(self, *args, **kwargs) -> bool:
        del args, kwargs
        self.media_calls += 1
        return True

    def publish_audio_packet(self, *args, **kwargs) -> bool:
        del args, kwargs
        self.media_calls += 1
        return True

    def objects_the_delivery_deadline_shed(self) -> "list[tuple[str, int, int]]":
        return []

    def close(self) -> "str | None":
        return None


def _a_streamlib_bag_publisher_over(
    session: _SessionRecordingWhatWasPublished,
) -> MoqBroadcastPublisher:
    publisher = MoqBroadcastPublisher(relay_url=A_RELAY, container_format="streamlib_bag")
    publisher._session = session  # type: ignore[assignment]
    return publisher


class _InputsWiredTo:
    def __init__(self, inbound_links: "list[str]") -> None:
        self._inbound_links = inbound_links

    def inbound_link_names(self, port: str) -> "list[str]":
        assert port == "tracks"
        return list(self._inbound_links)


class _SetupContextWiredTo:
    runtime_id = "a-runtime"

    def __init__(self, inbound_links: "list[str]") -> None:
        self.inputs = _InputsWiredTo(inbound_links)


def _drive_bags_through(
    reaches_the_transport: bool, bag_count: int = 1
) -> "tuple[list[str], int]":
    publisher = MoqBroadcastPublisher(relay_url=A_RELAY)
    session = _SessionThatAnswers(reaches_the_transport)
    publisher._session = session  # type: ignore[assignment]
    said: "list[str]" = []
    with mock.patch.object(log, "info", said.append):
        for _ in range(bag_count):
            publisher.process(_ContextReadingOneVideoBag())  # type: ignore[arg-type]
        publisher.teardown(None)  # type: ignore[arg-type]
    return said, session.calls


def _drive_one_bag_through(reaches_the_transport: bool) -> "tuple[list[str], int]":
    return _drive_bags_through(reaches_the_transport)


def test_a_bag_the_deadline_shed_is_neither_counted_as_published_nor_announced_as_the_first():
    """The drop must be counted and said — and the numbers beside it must not
    lie: a shed bag reached nothing, so it is not a publish."""
    said, calls = _drive_one_bag_through(reaches_the_transport=False)

    assert calls == 1
    assert not any("first bag accepted" in line for line in said)
    assert any(
        "bags_published=0" in line and "shed camera=1 objects/900 bytes" in line
        for line in said
    ), said


def test_a_bag_that_reached_the_transport_is_counted_and_announced_as_the_first():
    said, calls = _drive_one_bag_through(reaches_the_transport=True)

    assert calls == 1
    assert any("first bag accepted by the broadcast" in line for line in said)
    assert any(
        "bags_published=1" in line and "shed nothing" in line for line in said
    ), said


def test_a_run_shedding_every_bag_still_reports_at_the_progress_cadence():
    """The cadence counts bags handed over, not bags written: a publisher
    shedding everything after its sync points must not fall silent until
    teardown."""
    from streamlib_moq.processors import BAGS_BETWEEN_PROGRESS_REPORTS

    said, _ = _drive_bags_through(
        reaches_the_transport=False, bag_count=BAGS_BETWEEN_PROGRESS_REPORTS
    )

    progress_lines = [line for line in said if "bags_published=0" in line]
    assert len(progress_lines) == 2, said  # one at the cadence, one at teardown
    assert f"shed camera={BAGS_BETWEEN_PROGRESS_REPORTS} objects" in progress_lines[0]


def test_a_bag_without_a_bitstream_key_is_a_data_track():
    assert track_kind_of_bag(A_DATA_BAG, "probe/telemetry") == DATA_TRACK_KIND


@pytest.mark.parametrize(
    ("codec", "kind"), [("h264", "video"), ("h265", "video"), ("opus", "audio")]
)
def test_a_bag_with_a_bitstream_key_is_media_by_its_codec(codec, kind):
    assert track_kind_of_bag({"codec": codec, "bitstream": b"\x00"}, "encoder/out") == kind


def test_a_data_bag_that_spells_a_bitstream_key_is_refused_as_media_naming_the_key():
    """`bitstream` is the wire contract's defining key for encoded media, so a
    data bag that happens to use it is refused as media — and the message has
    to say which key to rename, or the author cannot know what went wrong."""
    with pytest.raises(ValueError, match="bitstream"):
        track_kind_of_bag({"bitstream": b"raw", "frame": 1}, "probe/telemetry")


def test_the_data_object_decodes_to_exactly_three_keys_with_the_bag_nested_whole():
    decoded = decode_msgpack_bytes_to_python_object(
        data_track_object_bytes(A_DATA_BAG, 7, 5_000_000_000)
    )

    assert set(decoded) == {
        DATA_OBJECT_SEQUENCE_INDEX_KEY,
        DATA_OBJECT_TIMESTAMP_NS_KEY,
        DATA_OBJECT_BAG_KEY,
    }
    assert decoded[DATA_OBJECT_SEQUENCE_INDEX_KEY] == 7
    assert decoded[DATA_OBJECT_TIMESTAMP_NS_KEY] == 5_000_000_000
    assert decoded[DATA_OBJECT_BAG_KEY] == A_DATA_BAG
    assert type(decoded[DATA_OBJECT_BAG_KEY]["blob"]) is bytes


def test_a_data_track_mints_its_sequence_index_per_link_and_stamps_the_bags_own_stamp():
    session = _SessionRecordingWhatWasPublished()
    publisher = _a_streamlib_bag_publisher_over(session)
    ctx = _ContextReadingBagsInTurn(
        [({"n": 1}, "probe/a", 10), ({"n": 2}, "probe/b", 20), ({"n": 3}, "probe/a", 30)]
    )

    for _ in range(3):
        publisher.process(ctx)  # type: ignore[arg-type]

    published = [
        (link, decode_msgpack_bytes_to_python_object(object_bytes))
        for link, object_bytes in session.data_objects
    ]
    assert [
        (
            link,
            envelope[DATA_OBJECT_SEQUENCE_INDEX_KEY],
            envelope[DATA_OBJECT_TIMESTAMP_NS_KEY],
            envelope[DATA_OBJECT_BAG_KEY],
        )
        for link, envelope in published
    ] == [
        ("probe/a", 0, 10, {"n": 1}),
        ("probe/b", 0, 20, {"n": 2}),
        ("probe/a", 1, 30, {"n": 3}),
    ]
    assert session.media_calls == 0


def test_a_media_bag_on_a_link_that_first_published_data_is_refused_by_name():
    publisher = _a_streamlib_bag_publisher_over(_SessionRecordingWhatWasPublished())
    ctx = _ContextReadingBagsInTurn([({"n": 1}, "probe/a", 10), (dict(A_VIDEO_BAG), "probe/a", 20)])
    publisher.process(ctx)  # type: ignore[arg-type]

    with pytest.raises(ValueError, match="probe/a"):
        publisher.process(ctx)  # type: ignore[arg-type]


def test_a_data_bag_on_a_link_that_first_published_media_is_refused_by_name():
    publisher = _a_streamlib_bag_publisher_over(_SessionRecordingWhatWasPublished())
    ctx = _ContextReadingBagsInTurn([(dict(A_VIDEO_BAG), "camera", 10), ({"n": 1}, "camera", 20)])
    publisher.process(ctx)  # type: ignore[arg-type]

    with pytest.raises(ValueError, match="camera"):
        publisher.process(ctx)  # type: ignore[arg-type]


def test_data_objects_are_reported_at_the_progress_cadence():
    publisher = _a_streamlib_bag_publisher_over(_SessionRecordingWhatWasPublished())
    ctx = _ContextReadingBagsInTurn(
        [({"n": n}, "probe/a", n) for n in range(BAGS_BETWEEN_PROGRESS_REPORTS)]
    )
    said: "list[str]" = []
    with mock.patch.object(log, "info", said.append):
        for _ in range(BAGS_BETWEEN_PROGRESS_REPORTS):
            publisher.process(ctx)  # type: ignore[arg-type]

    assert any(
        f"data_objects_published={BAGS_BETWEEN_PROGRESS_REPORTS}" in line for line in said
    ), said


@pytest.mark.parametrize("not_names", ["video", b"video", [1, 2], [None]])
def test_track_names_that_are_not_a_sequence_of_names_are_refused_by_name(not_names):
    """A bare string is a `Sequence[str]` to Python, so it is refused by name
    rather than read as one track per character."""
    with pytest.raises(ValueError, match="track_names"):
        MoqBroadcastPublisher(
            relay_url=A_RELAY, container_format="streamlib_bag", track_names=not_names
        )


def test_track_names_unequal_in_count_to_the_inbound_links_are_refused_by_name_at_setup():
    """Links are known at `setup()` and not before, so the count is checked
    there — by the wheel's Rust, which is the one place the names are
    declared."""
    publisher = MoqBroadcastPublisher(
        relay_url=A_RELAY, container_format="streamlib_bag", track_names=["video"]
    )

    with pytest.raises(ValueError, match="track_names"):
        publisher.setup(_SetupContextWiredTo(["encoder/video", "probe/telemetry"]))  # type: ignore[arg-type]


def test_track_names_under_cmaf_are_refused_by_name_at_setup():
    publisher = MoqBroadcastPublisher(relay_url=A_RELAY, track_names=["video"])

    with pytest.raises(ValueError, match="cmaf"):
        publisher.setup(_SetupContextWiredTo(["encoder/video"]))  # type: ignore[arg-type]


def test_track_names_matching_the_links_are_declared_and_said_at_setup():
    publisher = MoqBroadcastPublisher(
        relay_url=A_RELAY,
        container_format="streamlib_bag",
        track_names=["video", "telemetry"],
    )
    said: "list[str]" = []
    with mock.patch.object(log, "info", said.append):
        publisher.setup(_SetupContextWiredTo(["encoder/video", "probe/telemetry"]))  # type: ignore[arg-type]

    assert any("track_names=['video', 'telemetry']" in line for line in said), said


def test_the_oversize_guard_charges_the_framed_encoded_bag_not_the_bitstream_alone():
    """The engine charges the frame header plus the whole encoded bag against
    the ceiling, so a bitstream just under it is still dropped — and a guard
    reading `len(bitstream)` would have stayed silent about it."""
    bag = {**A_VIDEO_BAG, "bitstream": b"\x00" * 100}
    subscriber = MoqBroadcastSubscriber(
        relay_url=A_RELAY, broadcast=A_BROADCAST, video_track="video"
    )
    said: "list[str]" = []
    with (
        mock.patch.object(processors_module, "HELPER_LINK_PAYLOAD_CEILING_BYTES", 150),
        mock.patch.object(log, "error", said.append),
    ):
        subscriber._report_a_bag_the_link_will_drop(
            "encoded_video", bag, the_bitstream_alone_puts_the_bag_near_the_link_ceiling(bag)
        )

    assert len(bag["bitstream"]) <= 150 < framed_bag_byte_count(bag)
    assert said and "encoded_video" in said[0] and "framed" in said[0], said


def test_the_oversize_guard_measures_the_framed_size_only_near_the_ceiling():
    """The exact measure is an encode of the whole bag on the reader thread,
    so a bag whose bitstream leaves it far under the ceiling is never
    encoded twice — the engine's own write is the only encode it gets."""
    subscriber = MoqBroadcastSubscriber(
        relay_url=A_RELAY, broadcast=A_BROADCAST, video_track="video"
    )
    outputs = _OutputsRecordingWrites()
    with mock.patch.object(
        processors_module, "encode_bag_to_msgpack_bytes"
    ) as the_exact_measure:
        subscriber._write_a_media_bag(
            _AnAccessUnit(),  # type: ignore[arg-type]
            outputs,  # type: ignore[arg-type]
        )

    the_exact_measure.assert_not_called()
    assert [(port, stamp) for port, _, stamp in outputs.writes] == [("encoded_video", 1)]


def test_the_oversize_guard_stops_measuring_once_it_has_reported():
    subscriber = MoqBroadcastSubscriber(
        relay_url=A_RELAY, broadcast=A_BROADCAST, video_track="video"
    )
    subscriber._reported_an_oversized_bag = True
    with mock.patch.object(
        processors_module, "encode_bag_to_msgpack_bytes"
    ) as the_exact_measure:
        subscriber._report_a_bag_the_link_will_drop(
            "encoded_video",
            {**A_VIDEO_BAG, "bitstream": b"x" * (HELPER_LINK_PAYLOAD_CEILING_BYTES + 1)},
            True,
        )

    the_exact_measure.assert_not_called()
