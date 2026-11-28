# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`InputPin[T]`, `OutputPin[T]` and `PinGroup[S]`: how a class node under bare `@node` declares a stream, a trigger, an output and a slot that repeats.

A class node declares each of them as an annotation with no value, `video: InputPin[Texture]`,
and each value input as an annotated field with a default, `radius: float = 4.0`. The form
states the flow: an `InputPin` streams, or fires for a Trigger, and a field is a value. Each is
read by bare `@node` at decoration and is never constructed: on a running node the attribute
holds the object its helper bound, which reads or writes through the pin's port, and in a test
whatever stand-in the test hands `construct_class_node`. A `PinGroup` holds the slots its
node was built with.
"""

from __future__ import annotations

from collections.abc import Buffer, Iterator
from typing import TYPE_CHECKING, Generic, Literal, NoReturn, TypeVar, overload

from ._pin_types import Audio, EncodedAudio, EncodedVideo, Tensor, Texture, Trigger

if TYPE_CHECKING:
    from ._message import Message
    from ._node_context_protocols import PortPayloadBorrow, PortPayloadLoan
    from .image_frame import TextureToWrite
    from .stream_facts import PortStreamFacts

__all__ = ["InputPin", "OutputPin", "PinGroup"]

PinTypeOfThePin = TypeVar("PinTypeOfThePin")
PinTypeTheInputReads = TypeVar("PinTypeTheInputReads", covariant=True)
SlotOfTheGroup = TypeVar("SlotOfTheGroup")


class _ClassNodePin:
    """A pin a class node declares in an annotation; never constructed."""

    __slots__ = ()

    def __init__(self) -> NoReturn:
        raise TypeError(
            f"`{type(self).__name__}[...]` declares a class node's pin in an annotation with no "
            f"value, `video: {type(self).__name__}[Texture]`, and is never constructed."
        )


class InputPin(_ClassNodePin, Generic[PinTypeTheInputReads]):
    """A class node's Stream or Trigger input: `video: InputPin[Texture]`.

    How it reads its messages is `StreamDelivery` inside it,
    `InputPin[Annotated[Audio, StreamDelivery("ordered")]]`; a pin that states none reads by
    its type's default.

    `read()` gives the next value waiting, or `None`: a `Texture`, a block of `Audio`, a
    `Tensor`, a unit of `EncodedVideo` or `EncodedAudio`, a Trigger's fire as `True`, and a
    value or a named type in its Python form. Each read type names its pin type as `pin_type`,
    so a read of a pin of several types narrows by it.
    """

    __slots__ = ()

    @overload
    def read(self: InputPin[Texture]) -> Texture | None: ...
    @overload
    def read(self: InputPin[Audio]) -> Audio | None: ...
    @overload
    def read(self: InputPin[Tensor]) -> Tensor | None: ...
    @overload
    def read(self: InputPin[EncodedVideo]) -> EncodedVideo | None: ...
    @overload
    def read(self: InputPin[EncodedAudio]) -> EncodedAudio | None: ...
    @overload
    def read(self: InputPin[EncodedVideo | EncodedAudio]) -> EncodedVideo | EncodedAudio | None: ...
    @overload
    def read(self: InputPin[Trigger]) -> Literal[True] | None: ...
    @overload
    def read(
        self: InputPin[Texture | Audio | Tensor | EncodedVideo | EncodedAudio | None],
    ) -> Texture | Audio | Tensor | EncodedVideo | EncodedAudio | None: ...
    @overload
    def read(self) -> PinTypeTheInputReads | None: ...
    def read(self) -> object:
        """The next value waiting at this pin, or `None` when none is."""
        raise NotImplementedError

    @overload
    def read_message(self: InputPin[Texture]) -> Message[Texture] | None: ...
    @overload
    def read_message(self: InputPin[Audio]) -> Message[Audio] | None: ...
    @overload
    def read_message(self: InputPin[Tensor]) -> Message[Tensor] | None: ...
    @overload
    def read_message(self: InputPin[EncodedVideo]) -> Message[EncodedVideo] | None: ...
    @overload
    def read_message(self: InputPin[EncodedAudio]) -> Message[EncodedAudio] | None: ...
    @overload
    def read_message(
        self: InputPin[EncodedVideo | EncodedAudio],
    ) -> Message[EncodedVideo | EncodedAudio] | None: ...
    @overload
    def read_message(self: InputPin[Trigger]) -> Message[Literal[True]] | None: ...
    @overload
    def read_message(
        self: InputPin[Texture | Audio | Tensor | EncodedVideo | EncodedAudio | None],
    ) -> Message[Texture | Audio | Tensor | EncodedVideo | EncodedAudio] | None: ...
    @overload
    def read_message(self) -> Message[PinTypeTheInputReads] | None: ...
    def read_message(self) -> object:
        """The next message waiting at this pin with its stamp, group flag, losses and link, or `None`.

        Its payload is what `read()` gives. A value carried as its parts or as a number is read
        with `read()`, which gives its newest value.
        """
        raise NotImplementedError

    def read_message_as_sent(self) -> Message[PortPayloadBorrow] | None:
        """The next message waiting at this pin with its payload's bytes as they were sent,
        whichever of the pin's types it carries, or `None`: what a gateway hands on unread."""
        raise NotImplementedError

    def has_data(self) -> bool:
        """Whether a message is waiting at this pin."""
        raise NotImplementedError

    def inbound_link_names(self) -> list[str]:
        """Every link feeding this pin, in wiring order, readable from `setup()` on."""
        raise NotImplementedError

    def upstream_stream_facts(self, *, inbound_link: str | None = None) -> PortStreamFacts | None:
        """The stream facts the output feeding this pin carries now."""
        raise NotImplementedError

    def count_the_message_last_read_as_lost(self) -> None:
        """Count the message last read from this pin as lost on the link that delivered it."""
        raise NotImplementedError


class OutputPin(_ClassNodePin, Generic[PinTypeOfThePin]):
    """A class node's output: `result: OutputPin[Texture]`.

    `write(value)` sends one message: a `Texture` or a `TextureToWrite`,
    Audio as a `[channel][sample]` array from `start_sample`, at `sample_rate` when the pin's
    type does not fix one, a Tensor as a mapping of name to array, encoded media as the bytes of
    one unit, a Trigger's fire as `True`, and a value or a named type in its Python form.
    """

    __slots__ = ()

    @overload
    def write(
        self: OutputPin[Texture],
        value: Texture | TextureToWrite,
        timestamp_ns: int | None = None,
    ) -> None: ...
    @overload
    def write(
        self: OutputPin[Audio],
        value: Buffer,
        timestamp_ns: int | None = None,
        *,
        start_sample: int,
        sample_rate: int | None = None,
    ) -> None: ...
    @overload
    def write(
        self: OutputPin[EncodedVideo] | OutputPin[EncodedAudio],
        value: Buffer,
        timestamp_ns: int | None = None,
        *,
        starts_group: bool = True,
    ) -> None: ...
    @overload
    def write(self: OutputPin[Trigger], value: Literal[True], timestamp_ns: int | None = None) -> None: ...
    @overload
    def write(self, value: PinTypeOfThePin, timestamp_ns: int | None = None) -> None: ...
    def write(
        self,
        value: object,
        timestamp_ns: int | None = None,
        *,
        start_sample: int | None = None,
        sample_rate: int | None = None,
        starts_group: bool = True,
    ) -> None:
        """Send `value` on this pin, stamped `timestamp_ns` or stream time now."""
        raise NotImplementedError

    def loan(
        self, payload_byte_len: int, timestamp_ns: int | None = None, starts_group: bool = True
    ) -> PortPayloadLoan:
        """Loan `payload_byte_len` bytes on this pin to fill in place."""
        raise NotImplementedError

    def set_stream_facts(self, stream_facts: PortStreamFacts) -> None:
        """Replace the stream facts this pin carries."""
        raise NotImplementedError


class PinGroup(Generic[SlotOfTheGroup]):
    """A node's slot of pins and values that repeats: `layers: PinGroup[Layer]`.

    The slot is a frozen dataclass whose fields are pins, `picture: InputPin[Texture]`, and
    values with their defaults, `opacity: float = 1.0`, read as a class node's own pins and
    fields are. Each slot's ports are named `<group>.<index>.<field>`, from 0. A group whose
    slot holds an input that takes a wire has one empty slot after the last wired one; a group
    whose slot holds only outputs or settings has as many slots as the node's Integer input
    named after the group says.

    A node written as one function takes the group as a parameter, and `get()` reads its slots
    in a build or in the per-frame function: each slot the dataclass, its pins bound and its
    values as set. A slot wired or unwired while the node plays, or a value set on one, gives
    `get()` the new slots, and builds again each computed value that read them.

    On a running class node the attribute holds the slots the node was built with, in order.
    Nothing rebuilds a class node when the count changes, so `len(self.layers)` read in
    `__init__()` or `setup()` holds for its life.
    """

    __slots__ = ()

    def __init__(self) -> NoReturn:
        raise TypeError(
            "`PinGroup[...]` declares a node's pin group in an annotation with no value, "
            "`layers: PinGroup[Layer]`, and is never constructed."
        )

    def __len__(self) -> int:
        """How many slots the node was built with."""
        raise NotImplementedError

    def __iter__(self) -> Iterator[SlotOfTheGroup]:
        """Each slot, in order."""
        raise NotImplementedError

    def __getitem__(self, index: int) -> SlotOfTheGroup:
        """The slot at `index`, from 0."""
        raise NotImplementedError

    def get(self) -> "tuple[SlotOfTheGroup, ...]":
        """Each slot of a node function's group, in order, read in a build or in the per-frame
        function of the running node."""
        raise NotImplementedError
