# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Bare `@node` on a class: each annotated field a value input, each `InputPin` a stream or trigger, each `OutputPin` an output.

A field is a public annotated attribute with its default, `radius: float = 4.0`, and reads as a
value: `SettingOnly()` beside its type makes it a setting read at setup, `WireOnly()` one that
takes only a wire, and `InputValueLifetime(...)` states how long its value lasts. A pin is an
annotation with no value, `video: InputPin[Texture]`, and how a Stream input reads its messages
is `StreamDelivery` inside it. Every pin and field goes through `read_pin_declaration`, as a
function's parameters do, so a mistake is a `NodeDeclarationRefusal` with its finding code.
State the node keeps is `_private` or a `ClassVar`, and `__init__` takes no parameters.
"""

from __future__ import annotations

import inspect
import types
import typing
from dataclasses import dataclass
from typing import Any, ClassVar, Final, Literal, Optional

from ._class_node_pins import InputPin, OutputPin
from ._contract_pin_types import PinDeclarationOnTheWire
from ._description import description_and_annotation_without_it
from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe
from ._input_declaration_annotations import (
    InputValueLifetime,
    PublishedOnAnOutput,
    SettingOnly,
    StreamDelivery,
    WireOnly,
)
from ._bundle_pin_type import dataclass_a_bundle_pin_is_written_as
from ._pin_declaration import (
    NO_DEFAULT,
    DeclaredPin,
    NodeDeclarationFindingCode,
    NodeDeclarationRefusal,
    PinDirection,
    read_pin_declaration,
)
from ._former_name_check import refuse_former_names_that_find_no_one_pin
from ._pin_family import family_a_pin_travels_on, family_declaration_of_a_typed_output
from ._port_family_declaration import Input, Output

__all__ = ["ClassNodeMember", "read_class_node_ports", "read_class_node_ports_and_members"]

type DeclaredPort = dict[str, Any]
"""One port as the engine reads it off a described node type."""

type ClassNodeMemberForm = Literal["field", "input_pin", "output_pin"]
"""How a class node declares one of its ports: a value field, an `InputPin` or an `OutputPin`."""


@dataclass(frozen=True, slots=True)
class ClassNodeMember:
    """One field or pin of a class node: the attribute that holds it on the running node, and its port."""

    attribute: str
    """The attribute's name, as the class spells it."""

    port: str
    """The port's name, the attribute's name cast, as the engine and a stored value name it."""

    form: ClassNodeMemberForm

    pin_declaration: PinDeclarationOnTheWire

    a_setting: bool
    """Whether the field is read at setup, so a change rebuilds the node and is never assigned live."""

    default: object
    """The field's default, `NO_DEFAULT` for a pin."""

    bundle_class: Optional[type] = None
    """The frozen dataclass its bundle, or its List's bundle elements, is written as, which the
    running node is handed and gives; `None` for a member that holds no bundle."""

# The direction each pin form declares.
_DIRECTION_OF_EACH_PIN_FORM: Final[dict[type, PinDirection]] = {
    InputPin: "input",
    OutputPin: "output",
}


def read_class_node_ports(
    node_class: type, *, constructed_in_python: bool = True
) -> tuple[list[DeclaredPort], list[DeclaredPort]]:
    """The input and output ports `node_class`'s fields and pins declare, as the engine reads them.

    Refuses, as a `NodeDeclarationRefusal`, a port declared with `Input(...)` or `Output(...)`,
    an `__init__` that takes a parameter when the class is `constructed_in_python`, and each field
    or pin `read_pin_declaration` refuses. A built-in node, which the runtime implements, is never
    constructed in Python, so its `__init__` is not read, and its fields say what the runtime
    declares: a setting with no default, which a graph must set, a `PinTypeChoice` setting, a
    pin typed `Annotated[object, PinTypeFromSetting("...")]`, a setting read each run, and a field
    `PublishedOnAnOutput()`, which declares an output of its name as well.
    """
    input_ports, output_ports, _ = read_class_node_ports_and_members(
        node_class, constructed_in_python=constructed_in_python
    )
    return input_ports, output_ports


def read_class_node_ports_and_members(
    node_class: type, *, constructed_in_python: bool = True
) -> tuple[list[DeclaredPort], list[DeclaredPort], tuple[ClassNodeMember, ...]]:
    """The ports `read_class_node_ports` reads, and the field or pin that declares each, in
    declaration order; refused as `read_class_node_ports` refuses."""
    return _ClassNodeReading(node_class, of_the_runtimes_own_node=not constructed_in_python).ports(
        constructed_in_python=constructed_in_python
    )


class _ClassNodeReading:
    """One class's fields and pins, read once."""

    def __init__(self, node_class: type, *, of_the_runtimes_own_node: bool) -> None:
        self._node_class = node_class
        self._node = node_class.__name__
        self._of_the_runtimes_own_node = of_the_runtimes_own_node
        self._port_spelling_by_cast_name: dict[str, str] = {}
        self._input_ports_of_each_type_parameter: dict[typing.TypeVar, list[str]] = {}
        self._input_port_by_name: dict[str, DeclaredPort] = {}

    def ports(
        self, *, constructed_in_python: bool
    ) -> tuple[list[DeclaredPort], list[DeclaredPort], tuple[ClassNodeMember, ...]]:
        self._refuse_a_port_declared_in_the_retired_form()
        if constructed_in_python:
            self._refuse_an_init_taking_parameters()
        input_ports: list[DeclaredPort] = []
        output_ports: list[DeclaredPort] = []
        members: list[ClassNodeMember] = []
        typed_pins: list[tuple[str, str, PinDeclarationOnTheWire]] = []
        annotations = self._resolved_annotations()
        self._note_the_input_pins_each_type_parameter_types(annotations)
        for attribute_name, annotation in annotations.items():
            if attribute_name.startswith("_") or _is_a_class_variable(annotation):
                continue
            description, annotation = description_and_annotation_without_it(
                annotation, f"`{self._node}`'s attribute `{attribute_name}`"
            )
            pin_form, pin_type_annotation = self._pin_form_and_its_type(attribute_name, annotation)
            form: ClassNodeMemberForm
            if pin_form is None:
                port, published_on = self._field(attribute_name, annotation, description)
                input_ports.append(port)
                if published_on is not None:
                    output_ports.append(published_on)
                form = "field"
            elif _DIRECTION_OF_EACH_PIN_FORM[pin_form] == "input":
                port = self._input_pin(attribute_name, pin_type_annotation, description)
                input_ports.append(port)
                self._input_port_by_name[port["name"]] = port
                form = "input_pin"
            else:
                port = self._output_pin(attribute_name, pin_type_annotation, description)
                output_ports.append(port)
                form = "output_pin"
            typed_pins.append((attribute_name, port["name"], port["pin_declaration"]))
            members.append(
                ClassNodeMember(
                    attribute=attribute_name,
                    port=port["name"],
                    form=form,
                    pin_declaration=port["pin_declaration"],
                    a_setting=port.get("input_mode", {}).get("mode") == "setting_only",
                    default=(
                        getattr(self._node_class, attribute_name, NO_DEFAULT)
                        if form == "field"
                        else NO_DEFAULT
                    ),
                    bundle_class=dataclass_a_bundle_pin_is_written_as(
                        annotation
                        if pin_form is None
                        else _with_a_type_parameter_read_as_its_bound(pin_type_annotation)
                    ),
                )
            )
        refuse_former_names_that_find_no_one_pin(self._node, typed_pins)
        for output_port in output_ports:
            followed = output_port["family"].get("family_follows_input_port")
            if followed is not None and "family" not in output_port["family"]:
                output_port["family"] = {
                    "family": self._input_port_by_name[followed]["family"]["family"],
                    "family_follows_input_port": followed,
                }
        return input_ports, output_ports, tuple(members)

    def _refusal(
        self, words: str, finding_code: NodeDeclarationFindingCode, pin: Optional[str] = None
    ) -> NodeDeclarationRefusal:
        return NodeDeclarationRefusal(words, finding_code=finding_code, node=self._node, pin=pin)

    def _refuse_a_port_declared_in_the_retired_form(self) -> None:
        for ancestor in reversed(self._node_class.__mro__):
            for attribute_name, attribute in vars(ancestor).items():
                if isinstance(attribute, (Input, Output)):
                    kind = "Input" if isinstance(attribute, Input) else "Output"
                    pin_form = "InputPin" if kind == "Input" else "OutputPin"
                    raise self._refusal(
                        f"`{self._node}` declares `{attribute_name}` as `{kind}(...)`, and a "
                        f"class under bare `@node` declares each stream, trigger and output as "
                        f"a pin and each value input as a field. To fix: "
                        f"`{attribute_name}: {pin_form}[Texture]` for a stream of pictures, or "
                        f"`{attribute_name}: float = 0.0` for a value; keep `@node.process` to "
                        f"keep `{kind}(...)`.",
                        "class_declares_a_retired_port",
                        pin=attribute_name,
                    )

    def _refuse_an_init_taking_parameters(self) -> None:
        if self._node_class.__init__ is object.__init__:
            return
        parameters = [
            name
            for name in inspect.signature(self._node_class.__init__).parameters
            if name != "self"
        ]
        if not parameters:
            return
        if "config" in parameters:
            raise self._refusal(
                f"`{self._node}.__init__` takes `config`, and a class under bare `@node` takes "
                f"each setting as a field of its own, with its default. To fix: write each key "
                f"of the config class as a field, `radius: Annotated[float, SettingOnly()] = "
                f"4.0`, and drop `config`.",
                "class_takes_a_config",
            )
        raise self._refusal(
            f"`{self._node}.__init__` takes {', '.join(f'`{name}`' for name in parameters)}, and "
            f"the runtime constructs a class node with no arguments. To fix: declare each value "
            f"the node reads as a field with its default, and keep state the node builds in "
            f"`_private` attributes.",
            "init_takes_parameters",
        )

    def _resolved_annotations(self) -> dict[str, Any]:
        type_parameters = {
            type_parameter.__name__: type_parameter
            for type_parameter in getattr(self._node_class, "__type_params__", ())
        }
        try:
            return typing.get_type_hints(
                self._node_class,
                localns={**vars(self._node_class), **type_parameters} if type_parameters else None,
                include_extras=True,
            )
        except Exception as unresolvable:
            raise TypeError(
                f"{self._node} carries an annotation that cannot be resolved, so its fields and "
                f"pins cannot be read: {unresolvable}. Every annotation must be importable at "
                f"run time, not only under `TYPE_CHECKING`."
            ) from unresolvable

    def _note_the_input_pins_each_type_parameter_types(self, annotations: dict[str, Any]) -> None:
        """Note, by port, each `InputPin` a type parameter of the class types, which an
        `OutputPin` typed by the same parameter follows, wherever the output is declared."""
        for attribute_name, annotation in annotations.items():
            if attribute_name.startswith("_") or _is_a_class_variable(annotation):
                continue
            _, annotation = description_and_annotation_without_it(
                annotation, f"`{self._node}`'s attribute `{attribute_name}`"
            )
            pin_form, pin_type_annotation = self._pin_form_and_its_type(attribute_name, annotation)
            if pin_form is None or _DIRECTION_OF_EACH_PIN_FORM[pin_form] != "input":
                continue
            type_parameter = _the_type_parameter_typing(pin_type_annotation)
            if type_parameter is not None:
                self._input_ports_of_each_type_parameter.setdefault(type_parameter, []).append(
                    cast_exposed_name_to_url_safe(attribute_name)
                )

    def _pin_form_and_its_type(
        self, attribute_name: str, annotation: Any
    ) -> tuple[Optional[type], Any]:
        """The pin form `annotation` writes, `InputPin` or `OutputPin`, and the type inside it;
        `None` and the annotation itself for a field."""
        written = annotation
        metadata: list[object] = []
        if typing.get_origin(written) is typing.Annotated:
            written, *metadata = typing.get_args(written)
        pin_form = typing.get_origin(written) or written
        if pin_form not in _DIRECTION_OF_EACH_PIN_FORM:
            return None, annotation
        if metadata:
            raise self._refusal(
                f"`{self._node}`'s pin `{attribute_name}` carries "
                f"{', '.join(f'`{entry!r}`' for entry in metadata)} outside its "
                f"`{pin_form.__name__}[...]`. To fix: write what the pin states inside it, "
                f"`{pin_form.__name__}[Annotated[T, ...]]`.",
                "pin_annotation_not_taken",
                pin=attribute_name,
            )
        pin_type_arguments = typing.get_args(written)
        if not pin_type_arguments:
            raise self._refusal(
                f"`{self._node}`'s pin `{attribute_name}` is a bare `{pin_form.__name__}`, so "
                f"nothing names its type. To fix: `{attribute_name}: "
                f"{pin_form.__name__}[Texture]`, or the pin type it carries.",
                "pin_has_no_type",
                pin=attribute_name,
            )
        if self._declares_a_value(attribute_name):
            raise self._refusal(
                f"`{self._node}`'s pin `{attribute_name}` is given a value, and a pin carries "
                f"what its wire does. To fix: drop the value, or declare a value input as a "
                f"field, `{attribute_name}: float = 0.0`.",
                "pin_has_a_value",
                pin=attribute_name,
            )
        return pin_form, pin_type_arguments[0]

    def _declares_a_value(self, attribute_name: str) -> bool:
        return any(attribute_name in vars(ancestor) for ancestor in self._node_class.__mro__)

    def _field(
        self, attribute_name: str, annotation: Any, description: Optional[str]
    ) -> tuple[DeclaredPort, Optional[DeclaredPort]]:
        """The input port a field declares, and the output of its name it is published on when
        it is `PublishedOnAnOutput()`."""
        input_modes, annotation = _taken_off(annotation, (SettingOnly, WireOnly))
        lifetimes, annotation = _taken_off(annotation, (InputValueLifetime,))
        published, annotation = _taken_off(annotation, (PublishedOnAnOutput,))
        self._refuse_what_only_the_runtimes_own_node_declares(attribute_name, input_modes, published)
        if len(input_modes) > 1:
            raise self._refusal(
                f"`{self._node}`'s field `{attribute_name}` carries "
                f"{' and '.join(f'`{mode!r}`' for mode in input_modes)}, and a field takes one "
                f"mode. To fix: keep one.",
                "pin_annotation_repeated",
                pin=attribute_name,
            )
        if len(lifetimes) > 1:
            raise self._refusal(
                f"`{self._node}`'s field `{attribute_name}` carries {len(lifetimes)} "
                f"`InputValueLifetime`s; it takes one. To fix: keep one.",
                "pin_annotation_repeated",
                pin=attribute_name,
            )
        default = getattr(self._node_class, attribute_name, NO_DEFAULT)
        a_setting = bool(input_modes) and isinstance(input_modes[0], SettingOnly)
        declared = self._read_field(attribute_name, annotation, default, a_setting=a_setting)
        port: DeclaredPort = {
            "name": self._cast_port_name(attribute_name),
            "description": description or "",
            "family": {"family": _family_a_declared_pin_travels_on(declared.declaration)},
            "pin_declaration": declared.declaration,
        }
        if declared.default_value is not NO_DEFAULT:
            port["pin_default"] = declared.default_value
        if input_modes:
            port["input_mode"] = input_modes[0].input_mode()  # pyright: ignore[reportAttributeAccessIssue]
        elif _chooses_a_pin_type(declared.declaration):
            port["input_mode"] = SettingOnly().input_mode()
        else:
            # Stated, not left to the engine's default for a node of its kind, since the field
            # reads whole any value its pin type takes, from its wire or set on it.
            port["input_mode"] = {"mode": "wire_or_value"}
        if lifetimes:
            port["lifetime"] = lifetimes[0].lifetime  # pyright: ignore[reportAttributeAccessIssue]
        if not published:
            return port, None
        output_declaration: PinDeclarationOnTheWire = {
            "pin_type": declared.declaration["pin_type"],
            "flow": declared.declaration["flow"],
        }
        return port, {
            "name": port["name"],
            "description": port["description"],
            "family": dict(port["family"]),
            "pin_declaration": output_declaration,
        }

    def _refuse_what_only_the_runtimes_own_node_declares(
        self, attribute_name: str, input_modes: list[object], published: list[object]
    ) -> None:
        if self._of_the_runtimes_own_node:
            return
        only_the_runtimes = [*published] + [
            mode for mode in input_modes if isinstance(mode, SettingOnly) and mode.read != "at_setup"
        ]
        if only_the_runtimes:
            raise self._refusal(
                f"`{self._node}`'s field `{attribute_name}` carries `{only_the_runtimes[0]!r}`, "
                f"which only the runtime's own nodes declare. To fix: drop it; a class node's "
                f"setting is `SettingOnly()`, read at setup.",
                "pin_annotation_not_taken",
                pin=attribute_name,
            )

    def _read_field(
        self, attribute_name: str, annotation: Any, default: object, *, a_setting: bool
    ) -> DeclaredPin:
        try:
            return read_pin_declaration(
                self._node,
                attribute_name,
                annotation,
                default=default,
                direction="input",
                form="field",
                of_the_runtimes_own_node=self._of_the_runtimes_own_node,
                a_setting=a_setting,
            )
        except NodeDeclarationRefusal as refusal:
            if refusal.finding_code != "pin_type_unknown":
                raise
            raise self._refusal(
                f"{refusal} A class node's public annotated attribute is a field or a pin; state "
                f"the node keeps is `_{attribute_name}`, or a `ClassVar[...]`.",
                "attribute_not_a_pin",
                pin=attribute_name,
            ) from refusal

    def _input_pin(
        self, attribute_name: str, pin_type_annotation: Any, description: Optional[str]
    ) -> DeclaredPort:
        from ._node_declaration import AudioWindowContract, _audio_window_declaration  # pyright: ignore[reportPrivateUsage]

        if description is None:
            description, pin_type_annotation = description_and_annotation_without_it(
                pin_type_annotation, f"`{self._node}`'s pin `{attribute_name}`"
            )
        deliveries, pin_type_annotation = _taken_off(pin_type_annotation, (StreamDelivery,))
        audio_windows, pin_type_annotation = _taken_off(pin_type_annotation, (AudioWindowContract,))
        for repeated, what in ((deliveries, "`StreamDelivery`"), (audio_windows, "`AudioWindowContract`")):
            if len(repeated) > 1:
                raise self._refusal(
                    f"`{self._node}`'s pin `{attribute_name}` carries {len(repeated)} {what}s; "
                    f"it takes one. To fix: keep one.",
                    "pin_annotation_repeated",
                    pin=attribute_name,
                )
        declared = read_pin_declaration(
            self._node,
            attribute_name,
            pin_type_annotation,
            direction="input",
            form="pin",
            of_the_runtimes_own_node=self._of_the_runtimes_own_node,
        )
        port_name = self._cast_port_name(attribute_name)
        port: DeclaredPort = {
            "name": port_name,
            "description": description or "",
            "family": {"family": _family_a_declared_pin_travels_on(declared.declaration)},
            "pin_declaration": declared.declaration,
        }
        delivery: Optional[StreamDelivery] = deliveries[0] if deliveries else None  # pyright: ignore[reportAssignmentType]
        if delivery is not None:
            if declared.declaration["flow"] == "trigger":
                raise self._refusal(
                    f"`{self._node}`'s pin `{attribute_name}` is a Trigger, which fires once for "
                    f"each event and reads no messages in a delivery. To fix: drop "
                    f"`{delivery!r}`.",
                    "pin_annotation_not_taken",
                    pin=attribute_name,
                )
            port["delivery_profile"] = delivery.profile
            if delivery.borrow_quota is not None:
                port["borrow_quota"] = delivery.borrow_quota
        if audio_windows:
            port["audio_window"] = self._audio_window(
                attribute_name, declared, delivery, audio_windows[0], _audio_window_declaration
            )
        return port

    def _audio_window(
        self,
        attribute_name: str,
        declared: DeclaredPin,
        delivery: Optional[StreamDelivery],
        audio_window: object,
        audio_window_declaration: Any,
    ) -> dict[str, Any]:
        declared_pin_type = declared.declaration["pin_type"]
        if declared_pin_type["kind"] != "is" or declared_pin_type["pin_type"]["type"] != "audio":
            raise self._refusal(
                f"`{self._node}`'s pin `{attribute_name}` carries an `AudioWindowContract`, "
                f"which only an `InputPin[Audio]` takes. To fix: drop it.",
                "pin_annotation_not_taken",
                pin=attribute_name,
            )
        if delivery is not None and delivery.profile == "newest":
            raise self._refusal(
                f"`{self._node}`'s pin `{attribute_name}` carries an `AudioWindowContract` "
                f"beside `{delivery!r}`, which skips to the latest message, and a window "
                f"needs contiguous samples. To fix: `StreamDelivery(\"ordered\")`, or drop it "
                f"to read by Audio's default.",
                "pin_annotation_not_taken",
                pin=attribute_name,
            )
        return audio_window_declaration(
            audio_window, attribute_name, None if delivery is None else delivery.profile
        )

    def _output_pin(
        self, attribute_name: str, pin_type_annotation: Any, description: Optional[str]
    ) -> DeclaredPort:
        if description is None:
            description, pin_type_annotation = description_and_annotation_without_it(
                pin_type_annotation, f"`{self._node}`'s pin `{attribute_name}`"
            )
        declared = read_pin_declaration(
            self._node,
            attribute_name,
            pin_type_annotation,
            direction="output",
            form="pin",
            input_ports_of_each_type_parameter=self._input_ports_of_each_type_parameter,
            of_the_runtimes_own_node=self._of_the_runtimes_own_node,
        )
        port_name = self._cast_port_name(attribute_name)
        return {
            "name": port_name,
            "description": description or "",
            "family": self._family_of_an_output_pin(port_name, declared.declaration),
            "pin_declaration": declared.declaration,
        }

    def _family_of_an_output_pin(
        self, port_name: str, declaration: PinDeclarationOnTheWire
    ) -> dict[str, Any]:
        declared_pin_type = declaration["pin_type"]
        if declared_pin_type["kind"] == "is":
            return family_declaration_of_a_typed_output(port_name, declaration)
        if declared_pin_type["kind"] == "same_as_input":
            # The family of the input it follows, filled in once every input is read.
            return {"family_follows_input_port": declared_pin_type["input_port_name"]}
        return {"family": _family_a_declared_pin_travels_on(declaration)}

    def _cast_port_name(self, spelling: str) -> str:
        try:
            port_name = cast_exposed_name_to_url_safe(spelling)
        except ExposedNameCastsToNothingError as names_nothing:
            raise ValueError(f"{self._node} declares a port {spelling!r}: {names_nothing}") from names_nothing
        first_spelling = self._port_spelling_by_cast_name.get(port_name)
        if first_spelling is not None:
            raise ValueError(
                f"{self._node} declares the ports {first_spelling!r} and {spelling!r}, which "
                f"both cast to {port_name!r}; two ports need names that stay apart once cast."
            )
        self._port_spelling_by_cast_name[port_name] = spelling
        return port_name


# The family a pin whose type a setting or its link chooses declares until the runtime resolves
# that type.
_FAMILY_OF_A_PIN_TYPED_ELSEWHERE: Final = "records"


def _family_a_declared_pin_travels_on(declaration: PinDeclarationOnTheWire) -> str:
    """The family of a pin of one type, or of the Records family for one a setting or its link
    types."""
    if declaration["pin_type"]["kind"] in ("set_by_input", "any"):
        return _FAMILY_OF_A_PIN_TYPED_ELSEWHERE
    return family_a_pin_travels_on(declaration)


def _chooses_a_pin_type(declaration: PinDeclarationOnTheWire) -> bool:
    """Whether a field is a `PinTypeChoice`, a setting by its type."""
    declared_pin_type = declaration["pin_type"]
    return declared_pin_type["kind"] == "is" and declared_pin_type["pin_type"]["type"] == "pin_type_choice"


def _the_type_parameter_typing(pin_type_annotation: Any) -> Optional[typing.TypeVar]:
    """The type parameter a pin's type is, inside any `Annotated[...]`; `None` for another type."""
    written = pin_type_annotation
    while typing.get_origin(written) is typing.Annotated:
        written = typing.get_args(written)[0]
    return written if isinstance(written, typing.TypeVar) else None


def _with_a_type_parameter_read_as_its_bound(pin_type_annotation: Any) -> Any:
    """A pin's type with a type parameter of the class read as the type it is bound to."""
    type_parameter = _the_type_parameter_typing(pin_type_annotation)
    return pin_type_annotation if type_parameter is None else type_parameter.__bound__


def _is_a_class_variable(annotation: Any) -> bool:
    if typing.get_origin(annotation) is typing.Annotated:
        return _is_a_class_variable(typing.get_args(annotation)[0])
    return annotation is ClassVar or typing.get_origin(annotation) is ClassVar


def _taken_off(annotation: Any, annotation_classes: tuple[type, ...]) -> tuple[list[object], Any]:
    """The entries of `annotation_classes` an `Annotated[...]` carries, and the annotation with them taken off;
    an optional `Annotated[...] | None` keeps its `| None`."""
    optional_member = _the_member_an_optional_type_makes_optional(annotation)
    if optional_member is not None:
        taken, kept = _taken_off(optional_member, annotation_classes)
        return (taken, Optional[kept]) if taken else ([], annotation)
    if typing.get_origin(annotation) is not typing.Annotated:
        return [], annotation
    annotated, *metadata = typing.get_args(annotation)
    taken = [entry for entry in metadata if isinstance(entry, annotation_classes)]
    if not taken:
        return [], annotation
    kept = [entry for entry in metadata if not isinstance(entry, annotation_classes)]
    if not kept:
        return taken, annotated
    return taken, typing.Annotated[annotated, *kept]  # pyright: ignore[reportInvalidTypeArguments]


def _the_member_an_optional_type_makes_optional(annotation: Any) -> Any:
    """`T` for `T | None` or `Optional[T]`; `None` for any other annotation."""
    if typing.get_origin(annotation) not in (typing.Union, types.UnionType):
        return None
    members = [member for member in typing.get_args(annotation) if member is not type(None)]
    return members[0] if len(members) == 1 else None
