# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`read(port, into=T)` — the opt-in strictness dial, over a live link.

A message's payload is bytes, and nothing between two nodes ever compares a
type: the payload is delivered and the consumer decides how strictly to read
it. A plain read hands back the bytes; `into=T` hands them to `T.from_bytes`.
These tests drive that over real wired iceoryx2 ports rather than a stand-in —
the payload has to survive the wire before `into=` has anything to cast.

Both ends live on this thread because iceoryx2's ports are `!Send`, and the
destination is wired first because a send with no subscriber attached is
dropped.
"""

from __future__ import annotations

import dataclasses
import json
import os
from collections.abc import Buffer, Iterator, Mapping
from dataclasses import dataclass
from typing import Any, Self, TypedDict, assert_type

import pydantic
import pytest

from tatolab.runtime import _engine
from tatolab.stream import (
    NodeLinkDataAccess,
    PortPayloadBorrow,
    gpu_limited_access_of_the_typed_read_in_progress,
)

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)

OUTPUT_PORT = "detections_to_downstream"
INPUT_PORT = "detections_from_upstream"


def json_payload(record: "Mapping[str, Any]") -> bytes:
    """`record` as the JSON bytes the targets below read."""
    return json.dumps(record).encode()


class JsonRecordCast:
    """A target built from a JSON object's keys, the cast a test author writes."""

    @classmethod
    def from_bytes(cls, payload: Buffer) -> Self:
        return cls(**json.loads(bytes(payload)))


class DetectionTypedDict(TypedDict):
    """A TypedDict, which has no `from_bytes`."""

    label: str
    score: float


@dataclass
class DetectionDataclass(JsonRecordCast):
    """The constructing target: construction is the validation."""

    label: str
    score: float

    def to_bytes(self) -> bytes:
        return json_payload(dataclasses.asdict(self))


class DetectionModel(JsonRecordCast, pydantic.BaseModel):
    """The validating target: pydantic checks the field types a dataclass
    would take on trust."""

    label: str
    score: float


class PayloadBytesTheCastWasHanded:
    """A target with its own `from_bytes`, keeping exactly what it was handed
    and what the read offered it while it ran."""

    def __init__(self, payload: PortPayloadBorrow, offered: object) -> None:
        self.payload = payload
        self.offered = offered

    @classmethod
    def from_bytes(cls, payload: PortPayloadBorrow) -> Self:
        return cls(payload, gpu_limited_access_of_the_typed_read_in_progress())


@dataclass
class DetectionDataclassWithoutACast:
    """A dataclass with no `from_bytes`."""

    label: str
    score: float


def payload_bytes_and_stamp_of(
    read: "tuple[PortPayloadBorrow, int] | tuple[None, None]",
) -> "tuple[bytes | None, int | None]":
    payload, timestamp_ns = read
    return (None if payload is None else bytes(payload), timestamp_ns)


class WiredLinkUnderTest:
    """One live link, from the writing end to the reading end."""

    def __init__(
        self, source: NodeLinkDataAccess, destination: NodeLinkDataAccess
    ) -> None:
        self.source = source
        self.destination = destination

    def deliver(self, payload: bytes) -> None:
        self.source.write_to_output_port(OUTPUT_PORT, payload)


@pytest.fixture
def wired_link(request: pytest.FixtureRequest) -> Iterator[WiredLinkUnderTest]:
    """A source and a destination joined by one link.

    Service names carry the test's own name because every test in this
    process shares one iceoryx2 domain — a fixed name would let one test's
    channel meet the next one's.
    """
    unique = f"pinto{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/detections"
    notify_service_name = f"{unique}_dest/notify"
    link_id = f"L-{unique}"

    destination = _engine.open_node_link_data_access_for_helper_process()
    destination.wire_input_link(
        INPUT_PORT,
        channel_service_name,
        channel_service_name,
        notify_service_name,
        "read_next_in_order",
        8,
        8,
        2,
        1,
        link_id,
    )
    source = _engine.open_node_link_data_access_for_helper_process()
    source.wire_output_link(
        OUTPUT_PORT,
        channel_service_name,
        notify_service_name,
        1024,
        1 << 20,
        8,
        2,
        1,
        link_id,
    )
    yield WiredLinkUnderTest(source, destination)


def test_a_read_without_into_yields_the_payload_bytes_unchanged(
    wired_link: WiredLinkUnderTest,
):
    """No target named, so nothing is cast: the read hands back a borrow of
    the bytes the writer wrote — whatever they encode, the runtime never looks inside."""
    for payload in (json_payload({"label": "cat", "score": 0.9}), b"\xc1 not json"):
        wired_link.deliver(payload)

        read = wired_link.destination.read_from_input_port(INPUT_PORT)

        assert read is not None
        assert bytes(read) == payload
        assert type(read).__name__ == "PortPayloadBorrow", "a read borrows rather than copying"


def test_read_with_timestamp_yields_the_payload_bytes_and_the_writers_stamp(
    wired_link: WiredLinkUnderTest,
):
    wired_link.source.write_to_output_port(OUTPUT_PORT, b"stamped", 42)

    assert payload_bytes_and_stamp_of(
        wired_link.destination.read_from_input_port_with_timestamp(INPUT_PORT)
    ) == (b"stamped", 42)
    assert wired_link.destination.read_from_input_port_with_timestamp(INPUT_PORT) == (
        None,
        None,
    )


def test_an_empty_mailbox_reads_as_none_with_a_target_named(
    wired_link: WiredLinkUnderTest,
):
    """`into=` is a dial on how a payload is read, not a promise that one
    arrived — an empty mailbox still reads as `None` rather than calling
    `from_bytes` on nothing."""
    assert wired_link.destination.read_from_input_port(INPUT_PORT, into=DetectionModel) is None


def test_the_target_is_handed_the_payload_bytes_exactly(
    wired_link: WiredLinkUnderTest,
):
    """`into=T` calls `T.from_bytes` with the bytes the writer wrote, and what
    it returns is the read. The context-free data plane offers no GPU
    capability while it runs."""
    wired_link.deliver(b"\x00\x01\x02 any bytes at all")

    read = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=PayloadBytesTheCastWasHanded
    )

    assert read is not None
    assert bytes(read.payload) == b"\x00\x01\x02 any bytes at all"
    assert read.offered is None


def test_a_dataclass_target_is_constructed_from_the_payload(
    wired_link: WiredLinkUnderTest,
):
    wired_link.deliver(DetectionDataclass(label="cat", score=0.9).to_bytes())

    detection = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=DetectionDataclass
    )

    assert detection == DetectionDataclass(label="cat", score=0.9)


def test_a_dataclass_target_raises_at_the_read_on_a_mismatch(
    wired_link: WiredLinkUnderTest,
):
    """The whole point of the dial: the producer published something else and
    nothing in the engine noticed, so the consumer's own read is where that
    surfaces."""
    wired_link.deliver(json_payload({"label": "cat", "confidence": 0.9}))

    with pytest.raises(TypeError, match="confidence"):
        wired_link.destination.read_from_input_port(INPUT_PORT, into=DetectionDataclass)


def test_a_pydantic_model_target_is_constructed_from_the_payload(
    wired_link: WiredLinkUnderTest,
):
    wired_link.deliver(json_payload({"label": "cat", "score": 0.9}))

    detection = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=DetectionModel
    )

    assert detection == DetectionModel(label="cat", score=0.9)


def test_a_pydantic_model_target_raises_at_the_read_on_a_bad_field(
    wired_link: WiredLinkUnderTest,
):
    """A model checks the field types a dataclass takes on trust, and its own
    `ValidationError` is what reaches the author — naming the field and what
    it got, which no wrapper of ours could."""
    wired_link.deliver(json_payload({"label": "cat", "score": "very"}))

    with pytest.raises(pydantic.ValidationError, match="score"):
        wired_link.destination.read_from_input_port(INPUT_PORT, into=DetectionModel)


@pytest.mark.parametrize(
    "target_without_from_bytes",
    [DetectionDataclassWithoutACast, DetectionTypedDict, "DetectionDataclass"],
)
def test_a_target_without_from_bytes_is_refused_by_port_name(
    wired_link: WiredLinkUnderTest, target_without_from_bytes: object
):
    """A class with no `from_bytes`, a bare TypedDict and a string cannot
    build anything from bytes; the refusal names the port and says what to
    name instead."""
    wired_link.deliver(json_payload({"label": "cat", "score": 0.9}))

    with pytest.raises(TypeError, match=f'"{INPUT_PORT}"') as refusal:
        wired_link.destination.read_from_input_port(
            INPUT_PORT,
            into=target_without_from_bytes,  # type: ignore[call-overload]
        )
    assert "from_bytes" in str(refusal.value)


# =============================================================================
# The write takes a buffer and nothing else
# =============================================================================


@pytest.mark.parametrize(
    ("payload_that_is_not_a_buffer", "type_name"),
    [
        ({"label": "cat"}, "dict"),
        ("cat", "str"),
    ],
)
def test_a_write_refuses_a_payload_that_is_not_a_buffer_naming_the_buffer(
    wired_link: WiredLinkUnderTest, payload_that_is_not_a_buffer: object, type_name: str
):
    """No encoding happens behind `write`, so a record handed to it is an
    author's mistake to name at the call — never one to pack for them."""
    with pytest.raises(TypeError, match=f"as a contiguous buffer.*; got a {type_name}"):
        wired_link.source.write_to_output_port(
            OUTPUT_PORT,
            payload_that_is_not_a_buffer,  # type: ignore[arg-type]
        )

    assert wired_link.destination.read_from_input_port(INPUT_PORT) is None


@pytest.mark.parametrize(
    "payload_buffer",
    [bytearray(b"cat"), memoryview(b"xcat")[1:]],
    ids=["bytearray", "memoryview"],
)
def test_a_write_copies_any_contiguous_buffer(
    wired_link: WiredLinkUnderTest, payload_buffer: "bytearray | memoryview"
):
    wired_link.source.write_to_output_port(OUTPUT_PORT, payload_buffer)

    read = wired_link.destination.read_from_input_port(INPUT_PORT)
    assert read is not None
    assert bytes(read) == b"cat"


def test_a_write_that_names_starts_group_still_delivers_the_payload(
    wired_link: WiredLinkUnderTest,
):
    """The group flag rides the frame header, beside the stamp; the payload a
    reader gets is the same bytes either way. A Rust reader observing the flag
    itself is the wheel crate's own unit test."""
    wired_link.source.write_to_output_port(OUTPUT_PORT, b"delta", 7, False)
    wired_link.source.write_to_output_port(
        OUTPUT_PORT, b"key", timestamp_ns=8, starts_group=True
    )

    assert payload_bytes_and_stamp_of(
        wired_link.destination.read_from_input_port_with_timestamp(INPUT_PORT)
    ) == (b"delta", 7)
    assert payload_bytes_and_stamp_of(
        wired_link.destination.read_from_input_port_with_timestamp(INPUT_PORT)
    ) == (b"key", 8)


# =============================================================================
# Cast, not contract
# =============================================================================


class UnrelatedAudioChunk(TypedDict):
    """The type the producer publishes — nothing like what the consumer reads
    into, and nothing anywhere gets to notice."""

    samples: list[int]
    sample_rate_hz: int


def test_a_producer_publishing_one_type_reaches_a_consumer_reading_another(
    wired_link: WiredLinkUnderTest,
):
    """The engine has no type layer, so a producer publishing type X into a
    consumer that reads type Y is not an error anywhere in the engine: the
    payload is delivered, and the disagreement is the consumer's to discover."""
    audio_chunk: UnrelatedAudioChunk = {"samples": [1, 2, 3], "sample_rate_hz": 48_000}
    wired_link.deliver(json_payload(audio_chunk))

    read = wired_link.destination.read_from_input_port(INPUT_PORT)
    assert read is not None
    assert bytes(read) == json_payload(audio_chunk)


def test_the_mismatch_surfaces_only_at_a_read_that_asked_for_strictness(
    wired_link: WiredLinkUnderTest,
):
    """The same delivered payload two ways: named a target, the read raises;
    named nothing, the read hands back the bytes. Delivery is identical either
    way — the only difference is what the consumer asked for."""
    audio_chunk = json_payload({"samples": [1, 2, 3], "sample_rate_hz": 48_000})

    wired_link.deliver(audio_chunk)
    with pytest.raises(TypeError):
        wired_link.destination.read_from_input_port(INPUT_PORT, into=DetectionDataclass)

    wired_link.deliver(audio_chunk)
    read = wired_link.destination.read_from_input_port(INPUT_PORT)
    assert read is not None
    assert bytes(read) == audio_chunk


def test_a_read_that_raises_consumes_its_message(wired_link: WiredLinkUnderTest):
    """Where the message goes when the cast fails: the read pops it before it
    casts, so a raise is not a peek an author can retry — the message is
    spent. The alternative (leaving it queued) would silently re-raise on
    every later read of that port."""
    wired_link.deliver(json_payload({"samples": [1, 2, 3], "sample_rate_hz": 48_000}))

    with pytest.raises(TypeError):
        wired_link.destination.read_from_input_port(INPUT_PORT, into=DetectionDataclass)

    assert wired_link.destination.read_from_input_port(INPUT_PORT) is None


def test_the_link_keeps_delivering_after_a_read_raises(wired_link: WiredLinkUnderTest):
    """A failed strict read is a consumer-side exception, not a link fault:
    both ends stay healthy and the very next message crosses normally."""
    wired_link.deliver(json_payload({"samples": [1, 2, 3], "sample_rate_hz": 48_000}))
    with pytest.raises(TypeError):
        wired_link.destination.read_from_input_port(INPUT_PORT, into=DetectionDataclass)

    for expected_score in (0.1, 0.2, 0.3):
        wired_link.deliver(DetectionDataclass(label="cat", score=expected_score).to_bytes())
        detection = wired_link.destination.read_from_input_port(
            INPUT_PORT, into=DetectionDataclass
        )
        assert detection == DetectionDataclass(label="cat", score=expected_score)


def test_the_named_target_is_what_a_type_checker_sees(wired_link: WiredLinkUnderTest):
    """Half of what `into=` buys is static: a read that names a target comes
    back as that target rather than `Any`, and a plain read as a borrow.

    `assert_type` is checked by pyright and is a no-op at runtime — the reason
    this asserts nothing else.
    """
    payload = wired_link.destination.read_from_input_port(INPUT_PORT)
    assert_type(payload, PortPayloadBorrow | None)

    detection = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=DetectionDataclass
    )
    assert_type(detection, DetectionDataclass | None)

