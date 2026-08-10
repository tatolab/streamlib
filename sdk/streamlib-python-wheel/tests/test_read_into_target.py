# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`read(port, into=T)` — the opt-in strictness dial, over a live link.

The engine has no type layer, so nothing between two processors ever compares
a type: a bag is delivered and the consumer decides how strictly to read it.
That decision is what these tests drive, and they drive it over real wired
iceoryx2 ports rather than a stand-in — the bag has to survive the wire before
`into=` has anything to cast.

Both ends live on this thread because iceoryx2's ports are `!Send`, and the
destination is wired first because a send with no subscriber attached is
dropped.
"""

import os
from collections.abc import Iterator
from dataclasses import dataclass
from typing import Any, TypedDict

import pydantic
import pytest
from typing_extensions import TypedDict as TypedDictFromTypingExtensions
from typing_extensions import assert_type

from streamlib import ProcessorLinkDataAccess

OUTPUT_PORT = "detections_to_downstream"
INPUT_PORT = "detections_from_upstream"


class DetectionTypedDict(TypedDict):
    """The free-cast target: a TypedDict is a dict at runtime."""

    label: str
    score: float


class DetectionTypedDictFromTypingExtensions(TypedDictFromTypingExtensions):
    """The spelling `typing.is_typeddict` does not recognize."""

    label: str
    score: float


@dataclass
class DetectionDataclass:
    """The constructing target: construction is the validation."""

    label: str
    score: float


class DetectionModel(pydantic.BaseModel):
    """The validating target: pydantic checks the field types a dataclass
    would take on trust."""

    label: str
    score: float


class WiredLinkUnderTest:
    """One live link, from the writing end to the reading end."""

    def __init__(
        self, source: ProcessorLinkDataAccess, destination: ProcessorLinkDataAccess
    ) -> None:
        self.source = source
        self.destination = destination

    def deliver(self, bag: dict[str, Any]) -> None:
        self.source.write_to_output_port(OUTPUT_PORT, bag)


@pytest.fixture
def wired_link(request: pytest.FixtureRequest) -> Iterator[WiredLinkUnderTest]:
    """A source and a destination joined by one link.

    Service names carry the pid and the test's own name because iceoryx2
    service state is machine-global and outlives a crashed process — a fixed
    name would let one bad run poison every later one.
    """
    unique = f"pinto{os.getpid()}_{request.node.name}"
    channel_service_name = f"{unique}/detections"
    notify_service_name = f"{unique}_dest/notify"
    link_id = f"L-{unique}"

    destination = ProcessorLinkDataAccess()
    destination.wire_input_link(
        INPUT_PORT,
        channel_service_name,
        notify_service_name,
        "read_next_in_order",
        8,
        2,
        1,
        True,
        link_id,
    )
    source = ProcessorLinkDataAccess()
    source.wire_output_link(
        OUTPUT_PORT,
        channel_service_name,
        notify_service_name,
        1024,
        1 << 20,
        8,
        2,
        1,
        True,
        link_id,
    )
    yield WiredLinkUnderTest(source, destination)


def test_a_read_without_into_yields_the_bag_as_a_mapping(
    wired_link: WiredLinkUnderTest,
):
    """The unchanged default: no target named, so nothing is cast and the bag
    arrives as the mapping it is."""
    wired_link.deliver({"label": "cat", "score": 0.9})

    bag = wired_link.destination.read_from_input_port(INPUT_PORT)

    assert bag == {"label": "cat", "score": 0.9}
    assert type(bag) is dict


def test_an_empty_mailbox_reads_as_none_with_a_target_named(
    wired_link: WiredLinkUnderTest,
):
    """`into=` is a dial on how a bag is read, not a promise that one arrived —
    an empty mailbox still reads as `None` rather than constructing an empty
    target."""
    assert wired_link.destination.read_from_input_port(INPUT_PORT, into=DetectionModel) is None


def test_a_typed_dict_target_casts_for_free(wired_link: WiredLinkUnderTest):
    """The bag comes back as the dict it already is — nothing constructed."""
    wired_link.deliver({"label": "cat", "score": 0.9})

    detection = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=DetectionTypedDict
    )

    assert detection == {"label": "cat", "score": 0.9}
    assert type(detection) is dict


def test_a_typed_dict_target_validates_nothing(wired_link: WiredLinkUnderTest):
    """What "for free" costs: a bag missing a declared key and carrying one
    nobody declared reads without complaint. An author who wants the check
    names a dataclass or a model instead."""
    wired_link.deliver({"label": "cat", "unexpected": True})

    detection = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=DetectionTypedDict
    )

    assert detection == {"label": "cat", "unexpected": True}


def test_a_typing_extensions_typed_dict_is_an_accepted_target(
    wired_link: WiredLinkUnderTest,
):
    """A package supporting several interpreter versions spells its TypedDicts
    this way, and the read has to accept one rather than raise.

    It cannot show whether the bag was copied on the way through — both
    spellings return an equal dict when constructed, so only the Rust unit test
    can see the difference. What this pins is that the target is accepted at
    all.
    """
    wired_link.deliver({"label": "cat", "score": 0.9})

    detection = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=DetectionTypedDictFromTypingExtensions
    )

    assert detection == {"label": "cat", "score": 0.9}


def test_a_dataclass_target_is_constructed_from_the_bag(
    wired_link: WiredLinkUnderTest,
):
    wired_link.deliver({"label": "cat", "score": 0.9})

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
    wired_link.deliver({"label": "cat", "confidence": 0.9})

    with pytest.raises(TypeError, match="confidence"):
        wired_link.destination.read_from_input_port(INPUT_PORT, into=DetectionDataclass)


def test_a_pydantic_model_target_is_constructed_from_the_bag(
    wired_link: WiredLinkUnderTest,
):
    wired_link.deliver({"label": "cat", "score": 0.9})

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
    wired_link.deliver({"label": "cat", "score": "very"})

    with pytest.raises(pydantic.ValidationError, match="score"):
        wired_link.destination.read_from_input_port(INPUT_PORT, into=DetectionModel)


def test_the_named_target_is_what_a_type_checker_sees(wired_link: WiredLinkUnderTest):
    """Half of what `into=` buys is static: the annotation on a port method is
    read by humans and type checkers only, so a read that names a target has
    to come back as that target rather than `Any`.

    `assert_type` is checked by pyright and is a no-op at runtime — the reason
    this asserts nothing else.
    """
    detection = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=DetectionDataclass
    )
    assert_type(detection, DetectionDataclass | None)

    entries = wired_link.destination.read_from_input_port(
        INPUT_PORT, into=DetectionTypedDict
    )
    assert_type(entries, DetectionTypedDict | None)
