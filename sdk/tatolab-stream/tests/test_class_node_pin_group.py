# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A class node declares a `PinGroup[Slot]`: one slot of pins and values the engine repeats, built as a tuple of slots, and each mistake refused by name."""

from __future__ import annotations

import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Annotated, Any

import pytest

from tatolab.stream import (
    Description,
    InputPin,
    NodeDeclarationRefusal,
    NumberRange,
    OutputPin,
    PinGroup,
    SettingOnly,
    Texture,
    Vector2,
    node,
)
from tatolab.stream._class_node_construction import (
    assign_live_field_values,
    build_pin_group_slot,
    class_node_pin_groups,
    construct_class_node,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_EXECUTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_PIN_GROUPS_STAMP,
)
from tatolab.stream._project_node_declaration_check_entry import (
    check_the_node_declarations_of_a_project,
)


@dataclass(frozen=True, slots=True)
class Layer:
    picture: InputPin[Texture]
    opacity: Annotated[float, NumberRange(minimum=0.0, maximum=1.0)] = 1.0
    position: Vector2 = Vector2(0.0, 0.0)


@node.meta(description="Lays each layer over the one below it.")
@node
class Stack:
    layers: Annotated[PinGroup[Layer], Description("The layers, the bottom one first.")]
    result: OutputPin[Texture]

    def __init__(self) -> None:
        self.kernel_source = _a_kernel_sampling(len(self.layers))

    def process(self) -> None:
        for layer in self.layers:
            layer.picture.read()


@dataclass(frozen=True)
class Delayed[Value: float]:
    value: InputPin[Value]
    delayed: OutputPin[Value]


@node
class Delay:
    lines: PinGroup[Delayed]

    def process(self) -> None: ...


@dataclass(frozen=True)
class Unpacked:
    element: OutputPin[float]


@node
class Unpack:
    pack: InputPin[list[float]]
    elements: PinGroup[Unpacked]

    def process(self) -> None: ...


@dataclass(frozen=True)
class Band:
    gain: Annotated[float, SettingOnly()] = 1.0


@dataclass(frozen=True)
class Passed[Frame: Texture]:
    picture: InputPin[Frame]
    passed: OutputPin[Frame]


@dataclass(frozen=True)
class Untyped:
    picture: InputPin  # pyright: ignore[reportMissingTypeArgument]


class NotAPinType:
    pass


@dataclass(frozen=True)
class Odd:
    thing: NotAPinType = NotAPinType()


@dataclass
class Loose:
    opacity: float = 1.0


@dataclass(frozen=True)
class Empty:
    pass


@dataclass(frozen=True)
class Nested:
    inner: PinGroup[Layer]


@dataclass(frozen=True)
class Derived:
    picture: InputPin[Texture]
    opacity: float = field(init=False, default=1.0)


@dataclass(frozen=True)
class Counted:
    picture: InputPin[Texture]
    weights: list[float] = field(default_factory=lambda: [float(next(_factory_calls))])


_factory_calls = iter(range(100))


def _a_kernel_sampling(layer_count: int) -> str:
    """Slang for `layer_count` layers, one texture binding each, as a compositor builds it at setup."""
    return "\n".join(f"Texture2D layer_{layer};" for layer in range(layer_count))


def _the_group_of(node_class: type) -> dict[str, Any]:
    (pin_group,) = getattr(node_class, NODE_DECLARATION_PIN_GROUPS_STAMP)
    return pin_group


def test_a_pin_group_declares_its_slots_pins_and_values_typed() -> None:
    group = _the_group_of(Stack)

    assert group["name"] == "layers"
    assert group["description"] == "The layers, the bottom one first."
    assert [port["name"] for port in group["inputs"]] == ["picture", "opacity", "position"]
    assert group["outputs"] == []
    picture, opacity, position = group["inputs"]
    assert picture["pin_declaration"] == {
        "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
        "flow": "stream",
    }
    assert opacity["pin_declaration"]["pin_type"] == {"kind": "is", "pin_type": {"type": "float"}}
    assert opacity["pin_declaration"]["annotations"] == {"range": {"minimum": 0.0, "maximum": 1.0}}
    assert opacity["pin_default"] == 1.0
    assert opacity["input_mode"] == {"mode": "wire_or_value"}
    assert position["pin_default"] == {"x": 0.0, "y": 0.0}


def test_a_node_whose_only_inputs_are_in_a_group_runs_reactively() -> None:
    assert getattr(Stack, NODE_DECLARATION_INPUT_PORTS_STAMP) == []
    assert getattr(Stack, NODE_DECLARATION_EXECUTION_STAMP)["mode"] == "reactive"


def test_a_slots_output_follows_the_input_of_its_own_slot() -> None:
    (value,) = _the_group_of(Delay)["inputs"]
    (delayed,) = _the_group_of(Delay)["outputs"]

    assert value["pin_declaration"]["pin_type"] == {"kind": "is", "pin_type": {"type": "float"}}
    assert delayed["pin_declaration"]["pin_type"] == {
        "kind": "same_as_input",
        "input_port_name": "value",
    }
    assert delayed["family"] == {"family": "channels", "family_follows_input_port": "value"}


def test_a_group_holding_an_input_follows_its_wires_and_one_holding_only_outputs_has_its_count_set() -> None:
    (layers,) = class_node_pin_groups(Stack)
    (elements,) = class_node_pin_groups(Unpack)

    assert layers.follows_its_wires
    assert not elements.follows_its_wires
    assert layers.slot_ports(2) == (
        [
            "layers.0.picture",
            "layers.0.opacity",
            "layers.0.position",
            "layers.1.picture",
            "layers.1.opacity",
            "layers.1.position",
        ],
        [],
    )
    assert elements.slot_ports(2) == ([], ["elements.0.element", "elements.1.element"])


def _a_class_node(**annotations: Any) -> type:
    """A class node of `annotations`, each given as the object it is, so a slot named in a test resolves."""

    def process(self: object) -> None: ...

    return node(type("Stacked", (), {"__annotations__": annotations, "process": process}))


def test_a_group_whose_slot_holds_only_settings_has_its_count_set() -> None:
    equalizer = _a_class_node(bands=PinGroup[Band], audio=InputPin[Texture])

    (bands,) = class_node_pin_groups(equalizer)
    assert not bands.follows_its_wires


def test_a_slot_member_ports_name_their_slot_and_an_output_follows_its_slots_input() -> None:
    (lanes,) = class_node_pin_groups(_a_class_node(lanes=PinGroup[Passed]))
    picture, passed = lanes.members_of_slot(3)

    assert (picture.port, passed.port) == ("lanes.3.picture", "lanes.3.passed")
    assert passed.pin_declaration["pin_type"] == {
        "kind": "same_as_input",
        "input_port_name": "lanes.3.picture",
    }
    (passed_in_the_declaration,) = lanes.declared["outputs"]
    assert passed_in_the_declaration["pin_declaration"]["pin_type"]["input_port_name"] == "picture"


def test_a_node_is_built_holding_its_slots_in_order_and_reads_their_count_in_init() -> None:
    (layers,) = class_node_pin_groups(Stack)
    slots = [
        build_pin_group_slot(
            Stack,
            layers,
            field_values={"opacity": 0.5} if slot == 1 else {},
            pins={"picture": f"picture of slot {slot}"},
        )
        for slot in range(3)
    ]

    stack = construct_class_node(
        Stack, field_values={}, pins={"result": "result"}, pin_groups={"layers": slots}
    )

    assert [layer.picture for layer in stack.layers] == [
        "picture of slot 0",
        "picture of slot 1",
        "picture of slot 2",
    ]
    assert [layer.opacity for layer in stack.layers] == [1.0, 0.5, 1.0]
    assert stack.layers[0].position == Vector2(0.0, 0.0)
    assert stack.kernel_source == "Texture2D layer_0;\nTexture2D layer_1;\nTexture2D layer_2;"


def test_a_value_set_on_a_slots_field_gives_its_group_that_slot_holding_it() -> None:
    (layers,) = class_node_pin_groups(Stack)
    slots = [
        build_pin_group_slot(Stack, layers, field_values={}, pins={"picture": slot})
        for slot in range(2)
    ]
    stack = construct_class_node(Stack, field_values={}, pins={"result": "r"}, pin_groups={"layers": slots})

    assign_live_field_values(stack, {"layers.1.opacity": 0.25})

    assert [layer.opacity for layer in stack.layers] == [1.0, 0.25]
    assert stack.layers[1].picture == 1
    with pytest.raises(ValueError, match="declares no field `layers.2.opacity`"):
        assign_live_field_values(stack, {"layers.2.opacity": 0.25})


def test_a_node_built_with_no_slots_for_a_group_is_refused() -> None:
    with pytest.raises(ValueError, match="no slots for its pin group `layers`"):
        construct_class_node(Stack, field_values={}, pins={"result": "r"})


def _refusal_of(declare: Any) -> NodeDeclarationRefusal:
    with pytest.raises(NodeDeclarationRefusal) as refused:
        declare()
    return refused.value


def test_a_slot_member_with_no_pin_type_is_refused_naming_the_group_and_the_member() -> None:
    refusal = _refusal_of(lambda: _a_class_node(layers=PinGroup[Untyped]))
    assert refusal.finding_code == "pin_has_no_type"
    assert (refusal.node, refusal.pin) == ("Stacked", "layers.picture")
    assert "`Stacked`'s pin group `layers`:" in str(refusal)


def test_a_slot_member_typed_as_no_pin_type_is_refused() -> None:
    refusal = _refusal_of(lambda: _a_class_node(layers=PinGroup[Odd]))
    assert refusal.finding_code == "attribute_not_a_pin"
    assert refusal.pin == "layers.thing"


@pytest.mark.parametrize(
    ("slot", "finding_code"),
    [
        (Layer, None),
        (dict, "pin_group_slot_not_a_frozen_dataclass"),
        (Loose, "pin_group_slot_not_a_frozen_dataclass"),
        (Passed[Texture], "pin_group_slot_not_a_frozen_dataclass"),
        (Empty, "pin_group_slot_holds_no_pin"),
    ],
)
def test_a_slot_that_is_no_frozen_dataclass_of_pins_is_refused(slot: Any, finding_code: Any) -> None:
    def declare() -> type:
        return _a_class_node(layers=PinGroup[slot], audio=InputPin[Texture])

    if finding_code is None:
        declare()
        return
    assert _refusal_of(declare).finding_code == finding_code


def test_a_bare_pin_group_a_group_given_a_value_and_a_group_in_a_slot_are_refused() -> None:
    def bare() -> type:
        return _a_class_node(layers=PinGroup, audio=InputPin[Texture])

    def given_a_value() -> type:
        @node
        class Stacked:
            layers: PinGroup[Layer] = ()  # pyright: ignore[reportAssignmentType]
            audio: InputPin[Texture]

            def process(self) -> None: ...

        return Stacked

    def nested() -> type:
        return _a_class_node(layers=PinGroup[Nested], audio=InputPin[Texture])

    assert _refusal_of(bare).finding_code == "pin_group_slot_not_a_frozen_dataclass"
    assert _refusal_of(given_a_value).finding_code == "pin_has_a_value"
    refusal = _refusal_of(nested)
    assert (refusal.finding_code, refusal.pin) == ("pin_group_inside_a_slot", "layers.inner")


def test_a_slot_field_left_out_of_its_init_is_refused() -> None:
    """Fail-without-fix: the slot reads, and setup fails building `Derived(opacity=1.0)`."""
    refusal = _refusal_of(lambda: _a_class_node(layers=PinGroup[Derived], audio=InputPin[Texture]))
    assert (refusal.finding_code, refusal.pin) == (
        "pin_group_slot_not_a_frozen_dataclass",
        "layers.opacity",
    )


def test_a_slot_fields_factory_gives_its_port_and_its_member_one_default() -> None:
    """Fail-without-fix: call the factory for each, and the port's default is `[0.0]` while the
    slot is built with `[1.0]`."""
    stacked = _a_class_node(layers=PinGroup[Counted], audio=InputPin[Texture])
    (pin_group,) = class_node_pin_groups(stacked)
    (weights_port,) = [port for port in pin_group.declared["inputs"] if port["name"] == "weights"]
    (weights_member,) = [member for member in pin_group.members if member.port == "weights"]
    assert weights_port["pin_default"] == weights_member.default


def test_a_conversion_declaring_a_pin_group_is_refused() -> None:
    """Fail-without-fix: the conversion check reads only the node's own ports, so the slots'
    inputs that take a wire pass unseen."""

    def converts_with_layers() -> type:
        @node.meta(conversion=True)
        @node
        class Stacked:
            layers: PinGroup[Layer]
            audio: InputPin[Texture]
            mixed: OutputPin[Texture]

            def process(self) -> None: ...

        return Stacked

    def layers_under_meta() -> type:
        @node
        @node.meta(conversion=True)
        class Stacked:
            layers: PinGroup[Layer]
            audio: InputPin[Texture]
            mixed: OutputPin[Texture]

            def process(self) -> None: ...

        return Stacked

    for declare in (converts_with_layers, layers_under_meta):
        refusal = _refusal_of(declare)
        assert (refusal.finding_code, refusal.pin) == (
            "conversion_not_one_input_to_one_output",
            "layers",
        )


def test_the_declaration_check_answers_a_pin_group_on_a_function_node_with_its_finding(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    node_file = tmp_path / "nodes" / "stacked.py"
    node_file.parent.mkdir(parents=True)
    node_file.write_text(
        "from dataclasses import dataclass\n"
        "from tatolab.stream import InputPin, PinGroup, Texture, node\n\n\n"
        "@dataclass(frozen=True)\n"
        "class Layer:\n"
        "    picture: InputPin[Texture]\n\n\n"
        "@node\n"
        "def stacked(layers: PinGroup[Layer]) -> float:\n"
        "    return 0.0\n",
        encoding="utf-8",
    )
    monkeypatch.syspath_prepend(str(tmp_path))
    for module in ("nodes", "nodes.stacked"):
        monkeypatch.delitem(sys.modules, module, raising=False)
    lines: list[dict[str, Any]] = []

    check_the_node_declarations_of_a_project(tmp_path, ["nodes/stacked.py"], lines.append)

    (finding,) = lines[-1]["findings"]
    assert (finding["finding_code"], finding["port"]) == ("pin_group_on_a_function_node", "layers")
