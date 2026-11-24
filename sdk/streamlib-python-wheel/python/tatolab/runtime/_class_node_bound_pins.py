# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The pins a helper binds on a class node under bare `@node`, and how a value of each pin type is read off a port and written on one.

Each pin resolves its port and how it reads or writes once, when it is bound. A Texture,
Audio, Tensor or encoded pin reads and writes through its port's family, as `ctx.inputs` and
`ctx.outputs` do; a Trigger's fire reads as `True` and is written as the empty record every
Trigger message is. A Boolean, Integer or Float travels as a control message, a Color, Vector,
Rectangle or Transform as one on each channel of its parts, a String, Menu, Asset, List or bundle as the
record `{"value": <stored JSON form>}`, and a named type as the bytes its class packs, each read
and written as a function node run as a standalone node reads and writes it. A bundle holding a
Texture travels as one Bundle frame, its Texture by the pool slot it lives in.
"""

from __future__ import annotations

import dataclasses
import json
import struct
from typing import Any, Callable, Optional

from tatolab.stream import Array, Channels, Image, Input, Output, Packets, Records
from tatolab.stream._class_node_declaration import ClassNodeMember
from tatolab.stream._node_context_protocols import (
    LinkInputDataReader,
    LinkOutputDataWriter,
    NodeLinkDataAccess,
)

from tatolab.runtime._bundle_holding_a_texture import (
    holds_a_texture,
    the_bundle_a_frame_read_builds,
    the_small_members_and_textures_of,
)
from tatolab.runtime._engine import (
    read_the_bundle_frame_at_an_input_port,
    read_the_control_value_at_an_input_port,
    read_the_named_value_at_an_input_port,
    read_the_parts_at_an_input_port,
    read_the_stored_value_at_an_input_port,
    write_the_bundle_frame_on_an_output_port,
    write_the_named_value_on_an_output_port,
    write_the_stored_value_on_an_output_port,
)
from tatolab.runtime._function_node_carried_values import the_class_a_named_pin_type_names
from tatolab.runtime._function_node_stored_values import (
    STORED_VALUE_RECORD_KEY,
    STORED_VALUE_TYPES,
    the_stored_value_of,
    the_value_a_body_takes,
)

__all__ = [
    "BoundInputPin",
    "BoundOutputPin",
    "ValueReadAtAnInputPort",
    "bind_the_pins_of_a_class_node",
    "the_reader_of_a_value_at_an_input_port",
]

# The family each pin type that travels as a stream reads and writes through.
_FAMILY_OF_EACH_STREAM_PIN_TYPE: "dict[str, Any]" = {
    "texture": Image,
    "audio": Channels,
    "tensor": Array,
    "encoded_video": Packets,
    "encoded_audio": Packets,
    "trigger": Records,
}

# The pin types whose value travels as one number on a control message.
_NUMBER_PIN_TYPES = ("boolean", "integer", "float")

# The pin types whose value travels as one number on each channel of a control message.
_PART_PIN_TYPES = ("color", "vector", "rectangle", "transform")

# A control message on a wire: one f64 sample on each channel, at no regular rate.
_CONTROL_VALUE_SAMPLE_RATE = 0

type ValueReadAtAnInputPort = Callable[[], Any]
"""Reads the newest value its port holds, in its Python form, or `None` when none arrived."""


def bind_the_pins_of_a_class_node(
    members: "tuple[ClassNodeMember, ...]",
    *,
    link_data_access: NodeLinkDataAccess,
    inputs: LinkInputDataReader,
    outputs: LinkOutputDataWriter,
    node_id: str,
) -> "dict[str, BoundInputPin | BoundOutputPin]":
    """The pin object of each `InputPin` and `OutputPin` of `members`, by its port's name."""
    pins: "dict[str, BoundInputPin | BoundOutputPin]" = {}
    member_by_port = {member.port: member for member in members}
    for member in members:
        if member.form == "input_pin":
            pins[member.port] = BoundInputPin(
                member, link_data_access=link_data_access, inputs=inputs, node_id=node_id
            )
        elif member.form == "output_pin":
            pins[member.port] = BoundOutputPin(
                member,
                link_data_access=link_data_access,
                outputs=outputs,
                node_id=node_id,
                pin_types=_the_pin_types_of(member, member_by_port),
            )
    return pins


def the_reader_of_a_value_at_an_input_port(
    pin_type: "dict[str, Any]",
    *,
    link_data_access: NodeLinkDataAccess,
    inputs: LinkInputDataReader,
    node_id: str,
    input_port: str,
    bundle_class: Optional[type] = None,
) -> Optional[ValueReadAtAnInputPort]:
    """What reads the newest value of `pin_type` `input_port` holds, in its Python form, resolved once.

    A bundle, or each bundle element of a List, is built as `bundle_class`, the dataclass it is
    written as; a bundle holding a Texture is read off its Bundle frame, each Texture the
    `ImageFrame` it is. A Texture or a Tensor read as a value is its newest frame. `None` for a pin type whose value
    no wire carries, a node reference or a choice of pin type, and for Audio, encoded media and
    a Trigger, which travel only as a stream.
    """
    what_it_is = f"`{node_id}`'s input `{input_port}`"
    pin_type_name = pin_type["type"]
    if pin_type_name in ("texture", "tensor"):
        read_through = _named(Input(_FAMILY_OF_EACH_STREAM_PIN_TYPE[pin_type_name]), input_port)

        def read_the_newest_frame() -> Any:
            return inputs.read(read_through)

        return read_the_newest_frame
    if pin_type_name in _NUMBER_PIN_TYPES:

        def read_a_number() -> Any:
            number = read_the_control_value_at_an_input_port(
                link_data_access=link_data_access, node_id=node_id, input_port=input_port
            )
            return None if number is None else _the_number_as_its_pin_type(pin_type_name, number)

        return read_a_number
    if pin_type_name in _PART_PIN_TYPES:
        value_class = _value_class_of_parts(pin_type)

        def read_a_value_of_parts() -> Any:
            parts = read_the_parts_at_an_input_port(
                link_data_access=link_data_access,
                node_id=node_id,
                input_port=input_port,
                value_type=value_class.value_type,
            )
            return None if parts is None else value_class.from_parts(parts)

        return read_a_value_of_parts
    if holds_a_texture(pin_type):
        pin_type_as_json = json.dumps(pin_type)
        assert bundle_class is not None, "a bundle pin is written as a dataclass"
        built_as = bundle_class

        def read_a_bundle_frame() -> Any:
            members_read = read_the_bundle_frame_at_an_input_port(
                inputs=inputs, node_id=node_id, input_port=input_port, pin_type=pin_type_as_json
            )
            if members_read is None:
                return None
            return the_bundle_a_frame_read_builds(pin_type, members_read, what_it_is, built_as)

        return read_a_bundle_frame
    if pin_type_name in STORED_VALUE_TYPES:
        pin_type_as_json = json.dumps(pin_type)

        def read_a_stored_value() -> Any:
            record = read_the_stored_value_at_an_input_port(
                link_data_access=link_data_access,
                node_id=node_id,
                input_port=input_port,
                pin_type=pin_type_as_json,
            )
            if record is None:
                return None
            return the_value_a_body_takes(
                pin_type, record[STORED_VALUE_RECORD_KEY], what_it_is, bundle_class
            )

        return read_a_stored_value
    if pin_type_name == "named":
        named_class: Any = the_class_a_named_pin_type_names(pin_type["named_pin_type"], what_it_is)

        def read_a_named_value() -> Any:
            payload = read_the_named_value_at_an_input_port(
                link_data_access=link_data_access, node_id=node_id, input_port=input_port
            )
            return None if payload is None else named_class.from_bytes(payload)

        return read_a_named_value
    return None


class BoundInputPin:
    """An `InputPin` of a running class node: reads the messages its port receives."""

    __slots__ = ("_inputs", "_port", "_node_id", "_read_a_value", "_read_through", "_reads_a_fire")

    def __init__(
        self,
        member: ClassNodeMember,
        *,
        link_data_access: NodeLinkDataAccess,
        inputs: LinkInputDataReader,
        node_id: str,
    ) -> None:
        self._inputs = inputs
        self._port = member.port
        self._node_id = node_id
        pin_types = _the_pin_types_of(member)
        families = [_FAMILY_OF_EACH_STREAM_PIN_TYPE.get(pin_type["type"]) for pin_type in pin_types]
        self._reads_a_fire = [pin_type["type"] for pin_type in pin_types] == ["trigger"]
        self._read_a_value: Optional[ValueReadAtAnInputPort] = None
        if len(pin_types) == 1 and families[0] is None:
            self._read_a_value = the_reader_of_a_value_at_an_input_port(
                pin_types[0],
                link_data_access=link_data_access,
                inputs=inputs,
                node_id=node_id,
                input_port=member.port,
                bundle_class=member.bundle_class,
            )
            if self._read_a_value is None:
                raise TypeError(
                    f"`{node_id}`'s pin `{member.port}` is a {pin_types[0]['type']}, which no "
                    f"wire carries"
                )
        stream_families = list(dict.fromkeys(family for family in families if family is not None)) or [Records]
        self._read_through: Any = _named(
            Input(stream_families[0], also_takes=tuple(stream_families[1:])), member.port
        )

    def read(self) -> Any:
        """The next value waiting at this pin, or `None` when none is."""
        if self._read_a_value is not None:
            return self._read_a_value()
        payload = self._inputs.read(self._read_through)
        if payload is None:
            return None
        return True if self._reads_a_fire else payload

    def read_message(self) -> Any:
        """The next message waiting at this pin with its stamp, group flag, losses and link, or `None`."""
        if self._read_a_value is not None:
            raise TypeError(
                f"`{self._node_id}`'s pin `{self._port}` carries a value, which arrives as its "
                f"newest. To fix: read it with `read()`"
            )
        message = self._inputs.read_message(self._read_through)
        if message is None or not self._reads_a_fire:
            return message
        return dataclasses.replace(message, payload=True)

    def read_message_as_sent(self) -> Any:
        """The next message waiting at this pin with its payload's bytes as they were sent, or `None`."""
        if self._read_a_value is not None:
            raise TypeError(
                f"`{self._node_id}`'s pin `{self._port}` carries a value, which arrives as its "
                f"newest. To fix: read it with `read()`"
            )
        return self._inputs.read_message(self._port)

    def has_data(self) -> bool:
        """Whether a message is waiting at this pin."""
        return self._inputs.has_data(self._port)

    def inbound_link_names(self) -> "list[str]":
        """Every link feeding this pin, in wiring order."""
        return self._inputs.inbound_link_names(self._port)

    def upstream_stream_facts(self, *, inbound_link: Optional[str] = None) -> Any:
        """The stream facts the output feeding this pin carries now."""
        return self._inputs.upstream_stream_facts(self._port, inbound_link=inbound_link)

    def count_the_message_last_read_as_lost(self) -> None:
        """Count the message last read from this pin as lost on the link that delivered it."""
        self._inputs.count_the_message_last_read_as_lost(self._port)

    def __repr__(self) -> str:
        return f"InputPin({self._port!r})"


class BoundOutputPin:
    """An `OutputPin` of a running class node: writes the messages its port sends."""

    __slots__ = ("_outputs", "_port", "_node_id", "_write_a_value", "_write_through", "_writes")

    def __init__(
        self,
        member: ClassNodeMember,
        *,
        link_data_access: NodeLinkDataAccess,
        outputs: LinkOutputDataWriter,
        node_id: str,
        pin_types: "list[dict[str, Any]]",
    ) -> None:
        self._outputs = outputs
        self._port = member.port
        self._node_id = node_id
        declared_pin_type: Any = member.pin_declaration["pin_type"]
        if declared_pin_type["kind"] == "set_by_input":
            setting = declared_pin_type["input_name"]

            def refuse_a_write(value: Any, timestamp_ns: Optional[int]) -> None:
                del value, timestamp_ns
                raise TypeError(
                    f"`{node_id}`'s output `{member.port}` is of the type its setting `{setting}` "
                    f"chooses, which a Python body does not write yet; the node's native half "
                    f"writes it through its module host"
                )

            self._write_a_value = refuse_a_write
            self._write_through = None
            self._writes = "set_by_input"
            return
        if len(pin_types) != 1:
            raise TypeError(
                f"`{node_id}`'s output `{member.port}` declares no one type, and an output writes "
                f"exactly one"
            )
        pin_type = pin_types[0]
        if pin_type["type"] == "connection":

            def refuse_a_write_on_a_connection(value: Any, timestamp_ns: Optional[int]) -> None:
                del value, timestamp_ns
                raise TypeError(
                    f"`{node_id}`'s output `{member.port}` gives its node out as a connection, so "
                    f"nothing is written on it: each wire from it brings the node"
                )

            self._write_a_value = refuse_a_write_on_a_connection
            self._write_through = None
            self._writes = "connection"
            return
        family = _FAMILY_OF_EACH_STREAM_PIN_TYPE.get(pin_type["type"])
        self._write_a_value: Optional[Callable[[Any, Optional[int]], None]] = None
        if family is None:
            self._write_a_value = _the_writer_of_a_value_on_an_output_port(
                pin_type,
                link_data_access=link_data_access,
                outputs=outputs,
                node_id=node_id,
                output_port=member.port,
            )
        codecs = pin_type.get("codecs") or []
        codec = codecs[0] if family is Packets and len(codecs) == 1 else None
        self._write_through: Any = _named(Output(family or Records, codec=codec), member.port)
        self._writes = pin_type["type"]

    def write(
        self,
        value: Any,
        timestamp_ns: Optional[int] = None,
        *,
        start_sample: Optional[int] = None,
        sample_rate: Optional[int] = None,
        starts_group: bool = True,
    ) -> None:
        """Send `value` on this pin, stamped `timestamp_ns` or stream time now."""
        if self._write_a_value is not None:
            self._write_a_value(value, timestamp_ns)
        elif self._writes == "trigger":
            if value is not True:
                raise TypeError(
                    f"`{self._node_id}`'s output `{self._port}` is a Trigger, which fires with "
                    f"`write(True)`, and was given {value!r}"
                )
            self._outputs.write(self._write_through, {}, timestamp_ns)
        elif self._writes in ("encoded_video", "encoded_audio"):
            self._outputs.write(self._write_through, value, timestamp_ns, starts_group)
        elif self._writes == "audio" and sample_rate is not None:
            if start_sample is None:
                raise TypeError(
                    f"`{self._node_id}`'s output `{self._port}` writes Audio from a `start_sample`"
                )
            self._outputs.write_channels_block(
                self._port,
                value,
                start_sample=start_sample,
                sample_rate=sample_rate,
                timestamp_ns=timestamp_ns,
            )
        elif self._writes == "audio":
            self._outputs.write(self._write_through, value, timestamp_ns, start_sample=start_sample)
        else:
            self._outputs.write(self._write_through, value, timestamp_ns)

    def loan(self, payload_byte_len: int, timestamp_ns: Optional[int] = None, starts_group: bool = True) -> Any:
        """Loan `payload_byte_len` bytes on this pin to fill in place."""
        return self._outputs.loan(self._port, payload_byte_len, timestamp_ns, starts_group)

    def set_stream_facts(self, stream_facts: Any) -> None:
        """Replace the stream facts this pin carries."""
        self._outputs.set_stream_facts(self._port, stream_facts)

    def __repr__(self) -> str:
        return f"OutputPin({self._port!r})"


def _the_writer_of_a_value_on_an_output_port(
    pin_type: "dict[str, Any]",
    *,
    link_data_access: NodeLinkDataAccess,
    outputs: LinkOutputDataWriter,
    node_id: str,
    output_port: str,
) -> "Callable[[Any, Optional[int]], None]":
    what_it_is = f"`{node_id}`'s output `{output_port}`"
    pin_type_name = pin_type["type"]
    if pin_type_name in _NUMBER_PIN_TYPES or pin_type_name in _PART_PIN_TYPES:
        value_class = _value_class_of_parts(pin_type) if pin_type_name in _PART_PIN_TYPES else None
        control_values_written = [0]

        def write_a_control_value(value: Any, timestamp_ns: Optional[int]) -> None:
            if value_class is not None:
                if type(value) is not value_class:
                    raise TypeError(
                        f"{what_it_is} is a `{value_class.__name__}`, and was given {type(value).__name__}"
                    )
                parts: "tuple[float, ...]" = value.parts
            else:
                parts = (float(the_value_a_body_takes(pin_type, value, what_it_is)),)
            outputs.write_channels_block(
                output_port,
                memoryview(struct.pack(f"={len(parts)}d", *parts)).cast("d", (len(parts), 1)),
                start_sample=control_values_written[0],
                sample_rate=_CONTROL_VALUE_SAMPLE_RATE,
                timestamp_ns=timestamp_ns,
            )
            control_values_written[0] += 1

        return write_a_control_value
    if holds_a_texture(pin_type):
        pin_type_as_json = json.dumps(pin_type)

        def write_a_bundle_frame(value: Any, timestamp_ns: Optional[int]) -> None:
            small_members, textures = the_small_members_and_textures_of(pin_type, value, what_it_is)
            write_the_bundle_frame_on_an_output_port(
                outputs=outputs,
                output_port=output_port,
                pin_type=pin_type_as_json,
                small_members=json.dumps(small_members),
                textures=textures,
                timestamp_ns=timestamp_ns,
            )

        return write_a_bundle_frame
    if pin_type_name in STORED_VALUE_TYPES:
        pin_type_as_json = json.dumps(pin_type)

        def write_a_stored_value(value: Any, timestamp_ns: Optional[int]) -> None:
            write_the_stored_value_on_an_output_port(
                link_data_access=link_data_access,
                output_port=output_port,
                pin_type=pin_type_as_json,
                record={STORED_VALUE_RECORD_KEY: the_stored_value_of(pin_type, value, what_it_is)},
            )

        return write_a_stored_value
    if pin_type_name == "named":
        named_class: Any = the_class_a_named_pin_type_names(pin_type["named_pin_type"], what_it_is)

        def write_a_named_value(value: Any, timestamp_ns: Optional[int]) -> None:
            if not isinstance(value, named_class):
                raise TypeError(
                    f"{what_it_is} is a `{named_class.__name__}`, and was given {type(value).__name__}"
                )
            write_the_named_value_on_an_output_port(
                link_data_access=link_data_access, output_port=output_port, payload=bytes(value.to_bytes())
            )

        return write_a_named_value
    raise TypeError(f"{what_it_is} is a {pin_type_name}, which no value is written as")


def _the_pin_types_of(
    member: ClassNodeMember, member_by_port: "Optional[dict[str, ClassNodeMember]]" = None
) -> "list[dict[str, Any]]":
    """The pin types `member` declares: its one type, each of several, the types of the input an
    output typed by the class's type parameter follows, or none for a pin typed another way."""
    declared_pin_type: Any = member.pin_declaration["pin_type"]
    if declared_pin_type["kind"] == "is":
        return [declared_pin_type["pin_type"]]
    if declared_pin_type["kind"] == "one_of":
        return list(declared_pin_type["pin_types"])
    if declared_pin_type["kind"] == "same_as_input" and member_by_port is not None:
        followed = member_by_port.get(declared_pin_type["input_port_name"])
        if followed is not None:
            return _the_pin_types_of(followed)
    return []


def _the_number_as_its_pin_type(pin_type_name: str, number: float) -> Any:
    if pin_type_name == "boolean":
        return number != 0.0
    if pin_type_name == "integer":
        return int(round(number))
    return number


def _value_class_of_parts(pin_type: "dict[str, Any]") -> Any:
    from tatolab.stream._pin_types import PIN_VALUE_CLASSES

    return next(value_class for value_class in PIN_VALUE_CLASSES if value_class.pin_type == pin_type)


def _named(port_attribute: Any, port: str) -> Any:
    """`port_attribute`, an `Input` or `Output` of the family a pin reads or writes through, named `port`."""
    port_attribute.__set_name__(None, port)
    return port_attribute
