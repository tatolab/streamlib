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
import typing
from typing import Any, ClassVar, Final, Optional

from ._class_node_pins import InputPin, OutputPin
from ._description import description_and_annotation_without_it
from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe
from ._input_declaration_annotations import (
    InputValueLifetime,
    SettingOnly,
    StreamDelivery,
    WireOnly,
)
from ._pin_declaration import (
    NO_DEFAULT,
    DeclaredPin,
    NodeDeclarationFindingCode,
    NodeDeclarationRefusal,
    PinDirection,
    read_pin_declaration,
)
from ._pin_family import family_a_pin_travels_on, family_declaration_of_a_typed_output
from ._port_family_declaration import Input, Output

__all__ = ["read_class_node_ports"]

type DeclaredPort = dict[str, Any]
"""One port as the engine reads it off a described node type."""

# The direction each pin form declares.
_DIRECTION_OF_EACH_PIN_FORM: Final[dict[type, PinDirection]] = {
    InputPin: "input",
    OutputPin: "output",
}


def read_class_node_ports(node_class: type) -> tuple[list[DeclaredPort], list[DeclaredPort]]:
    """The input and output ports `node_class`'s fields and pins declare, as the engine reads them.

    Refuses, as a `NodeDeclarationRefusal`, a port declared with `Input(...)` or `Output(...)`,
    an `__init__` that takes a parameter, and each field or pin `read_pin_declaration` refuses.
    """
    return _ClassNodeReading(node_class).ports()


class _ClassNodeReading:
    """One class's fields and pins, read once."""

    def __init__(self, node_class: type) -> None:
        self._node_class = node_class
        self._node = node_class.__name__
        self._port_spelling_by_cast_name: dict[str, str] = {}

    def ports(self) -> tuple[list[DeclaredPort], list[DeclaredPort]]:
        self._refuse_a_port_declared_in_the_retired_form()
        self._refuse_an_init_taking_parameters()
        input_ports: list[DeclaredPort] = []
        output_ports: list[DeclaredPort] = []
        for attribute_name, annotation in self._resolved_annotations().items():
            if attribute_name.startswith("_") or _is_a_class_variable(annotation):
                continue
            description, annotation = description_and_annotation_without_it(
                annotation, f"`{self._node}`'s attribute `{attribute_name}`"
            )
            pin_form, pin_type_annotation = self._pin_form_and_its_type(attribute_name, annotation)
            if pin_form is None:
                input_ports.append(self._field(attribute_name, annotation, description))
            elif _DIRECTION_OF_EACH_PIN_FORM[pin_form] == "input":
                input_ports.append(self._input_pin(attribute_name, pin_type_annotation, description))
            else:
                output_ports.append(self._output_pin(attribute_name, pin_type_annotation, description))
        return input_ports, output_ports

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
        try:
            return typing.get_type_hints(self._node_class, include_extras=True)
        except Exception as unresolvable:
            raise TypeError(
                f"{self._node} carries an annotation that cannot be resolved, so its fields and "
                f"pins cannot be read: {unresolvable}. Every annotation must be importable at "
                f"run time, not only under `TYPE_CHECKING`."
            ) from unresolvable

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

    def _field(self, attribute_name: str, annotation: Any, description: Optional[str]) -> DeclaredPort:
        input_modes, annotation = _taken_off(annotation, (SettingOnly, WireOnly))
        lifetimes, annotation = _taken_off(annotation, (InputValueLifetime,))
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
        declared = self._read_field(attribute_name, annotation, default)
        port: DeclaredPort = {
            "name": self._cast_port_name(attribute_name),
            "description": description or "",
            "family": {"family": family_a_pin_travels_on(declared.declaration)},
            "pin_declaration": declared.declaration,
            "pin_default": declared.default_value,
        }
        if input_modes:
            port["input_mode"] = input_modes[0].input_mode()  # pyright: ignore[reportAttributeAccessIssue]
        if lifetimes:
            port["lifetime"] = lifetimes[0].lifetime  # pyright: ignore[reportAttributeAccessIssue]
        return port

    def _read_field(self, attribute_name: str, annotation: Any, default: object) -> DeclaredPin:
        try:
            return read_pin_declaration(
                self._node, attribute_name, annotation, default=default, direction="input", form="field"
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
            self._node, attribute_name, pin_type_annotation, direction="input", form="pin"
        )
        port_name = self._cast_port_name(attribute_name)
        port: DeclaredPort = {
            "name": port_name,
            "description": description or "",
            "family": {"family": family_a_pin_travels_on(declared.declaration)},
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
            self._node, attribute_name, pin_type_annotation, direction="output", form="pin"
        )
        port_name = self._cast_port_name(attribute_name)
        return {
            "name": port_name,
            "description": description or "",
            "family": family_declaration_of_a_typed_output(port_name, declared.declaration),
            "pin_declaration": declared.declaration,
        }

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


def _is_a_class_variable(annotation: Any) -> bool:
    return annotation is ClassVar or typing.get_origin(annotation) is ClassVar


def _taken_off(annotation: Any, annotation_classes: tuple[type, ...]) -> tuple[list[object], Any]:
    """The entries of `annotation_classes` an `Annotated[...]` carries, and the annotation with them taken off."""
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
