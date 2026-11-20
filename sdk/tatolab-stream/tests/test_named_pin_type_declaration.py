# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@named_pin_type` declares a module's own pin type, named by the class's import path, and a bare `@node` pin typed by it carries it as bytes, linked only."""

from __future__ import annotations

import struct
from collections.abc import Buffer
from dataclasses import dataclass
from typing import Annotated, Any, Self

import pytest

from tatolab.stream import (
    NamedPinTypeValue,
    NodeDeclarationRefusal,
    PinMeaning,
    named_pin_type,
    node,
)
from tatolab.stream._function_node_declaration import NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP
from tatolab.stream._named_pin_type_declaration import NamedPinTypeDeclared, named_pin_type_declared_by
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)

FACE_BOX = f"{__name__}:FaceBox"


@named_pin_type(description="A face found in a picture: its box and how sure the finder is.")
@dataclass(frozen=True)
class FaceBox:
    x: float
    y: float
    score: float

    @classmethod
    def from_bytes(cls, payload: Buffer) -> Self:
        return cls(*struct.unpack("<3d", bytes(payload)))

    def to_bytes(self) -> bytes:
        return struct.pack("<3d", self.x, self.y, self.score)


class Detector:
    @named_pin_type(description="What a detector saw, as one byte.")
    class Sighting:
        def __init__(self, seen: int) -> None:
            self.seen = seen

        @classmethod
        def from_bytes(cls, payload: Buffer) -> Self:
            return cls(bytes(payload)[0])

        def to_bytes(self) -> bytes:
            return bytes([self.seen])


@node
def Find(threshold: float = 0.5) -> FaceBox:
    return FaceBox(0.25, 0.75, threshold)


@node
def Frame(face: FaceBox, spare: FaceBox | None = None) -> Annotated[FaceBox, PinMeaning("face")]:
    return face


def Defaulted(face: FaceBox = FaceBox(0.0, 0.0, 1.0)) -> float:
    return face.score


def input_port(declared: Any, name: str) -> dict[str, Any]:
    return next(port for port in getattr(declared, NODE_DECLARATION_INPUT_PORTS_STAMP) if port["name"] == name)


def test_the_type_is_named_by_the_classs_import_path_with_its_description() -> None:
    assert named_pin_type_declared_by(FaceBox) == NamedPinTypeDeclared(
        import_path=FACE_BOX,
        description="A face found in a picture: its box and how sure the finder is.",
    )
    assert named_pin_type_declared_by(Detector.Sighting) == NamedPinTypeDeclared(
        import_path=f"{__name__}:Detector.Sighting", description="What a detector saw, as one byte."
    )
    assert isinstance(FaceBox(0.0, 0.0, 1.0), NamedPinTypeValue)


def test_a_subclass_and_anything_undecorated_declare_no_type() -> None:
    class Wider(FaceBox):
        pass

    assert named_pin_type_declared_by(Wider) is None
    assert named_pin_type_declared_by(float) is None
    assert named_pin_type_declared_by(FaceBox(0.0, 0.0, 1.0)) is None


def test_a_class_that_does_not_pack_its_values_is_refused_with_the_fix() -> None:
    with pytest.raises(TypeError, match=r"no `from_bytes` classmethod.*@classmethod def from_bytes"):

        @named_pin_type(description="Packs only.")
        class PacksOnly:
            def to_bytes(self) -> bytes:
                return b""

    with pytest.raises(TypeError, match=r"no `from_bytes` classmethod"):

        @named_pin_type(description="Reads on an instance.")
        class ReadsOnAnInstance:
            def from_bytes(self, payload: Buffer) -> Self:
                return self

            def to_bytes(self) -> bytes:
                return b""

    with pytest.raises(TypeError, match=r"no `to_bytes` method.*def to_bytes\(self\) -> bytes"):

        @named_pin_type(description="Reads only.")
        class ReadsOnly:
            @classmethod
            def from_bytes(cls, payload: Buffer) -> Self:
                return cls()


def test_a_class_defined_in_a_function_is_refused_as_no_path_finds_it() -> None:
    with pytest.raises(TypeError, match=r"defined inside a function.*top level of its module"):

        @named_pin_type(description="A local type.")
        class Local:
            @classmethod
            def from_bytes(cls, payload: Buffer) -> Self:
                return cls()

            def to_bytes(self) -> bytes:
                return b""


@pytest.mark.parametrize("description", ["", "   ", None])
def test_a_type_with_no_description_is_refused(description: Any) -> None:
    with pytest.raises(TypeError, match=r"what a value of the type is"):
        named_pin_type(description=description)


def test_the_decorator_goes_on_a_class() -> None:
    with pytest.raises(TypeError, match=r"goes on a class"):
        named_pin_type(description="A function.")(lambda: None)  # pyright: ignore[reportArgumentType]


def test_a_named_type_pin_declares_its_type_reads_only_its_link_and_travels_as_named() -> None:
    face = input_port(Frame, "face")
    assert face["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "named", "named_pin_type": FACE_BOX}},
        "flow": "stream",
    }
    assert "pin_default" not in face
    assert face["family"] == {"family": "records"}
    spare = input_port(Frame, "spare")
    assert spare["pin_declaration"]["may_be_unset"] is True
    assert spare["pin_default"] is None

    (result,) = getattr(Frame, NODE_DECLARATION_OUTPUT_PORTS_STAMP)
    assert result["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "named", "named_pin_type": FACE_BOX}},
        "flow": "stream",
        "annotations": {"meaning": "face"},
    }
    signature = getattr(Frame, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)
    assert [(entry["value_type"], entry["pin_type"], entry["optional"]) for entry in signature["inputs"]] == [
        ("named", {"type": "named", "named_pin_type": FACE_BOX}, False),
        ("named", {"type": "named", "named_pin_type": FACE_BOX}, True),
    ]
    assert signature["outputs"][0]["value_type"] == "named"
    assert getattr(Find, NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP)["outputs"][0]["pin_type"] == {
        "type": "named",
        "named_pin_type": FACE_BOX,
    }


def test_a_named_type_input_with_a_default_is_refused_as_it_holds_no_value() -> None:
    with pytest.raises(NodeDeclarationRefusal, match=r"holds no value.*drop the default") as refused:
        node(Defaulted)

    assert refused.value.finding_code == "wire_only_input_with_a_default"
    assert refused.value.pin == "face"
