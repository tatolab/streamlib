# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The function grammar of bare `@node`: a function whose signature is the node.

Each plain parameter is an input pin, read by `read_pin_declaration` into the pin type it
declares and what it states besides, which its port carries to the runtime as
`pin_declaration`; its default is the pin's value until one is set or linked. One returned value
is the output `result`, and a `NamedTuple` one output per field. A frozen dataclass, taken or
returned, is one bundle pin whose members are its fields, and the signature names the dataclass
so the body is handed one and gives one back. A pin typed by a type parameter of the function,
`def Gate[PinValue](value: PinValue) -> PinValue`, hands on unread what its link brings, and its
output takes the type linked into its input. `Time` and `State[...]` are found by type and are
never pins. Decorating registers nothing: it reads the signature once and stamps the
declaration the runtime reads, plus the signature the runtime calls the body with.

A shader's signature is read the same way, except that each `Texture` it takes or returns is a
picture it computes over, which arrives as an Image.
"""

from __future__ import annotations

import dataclasses
import functools
import inspect
import typing
from typing import Any, Callable, Final, Generic, Optional, ParamSpec, TypeVar

from ._class_node_pin_group import refuse_a_pin_group_on_a_function_node
from ._contract_pin_types import PinDeclarationOnTheWire
from ._description import description_and_annotation_without_it
from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe
from ._function_node_side_effect_check import refuse_a_function_node_body_with_side_effects
from ._named_pin_type_declaration import named_pin_type_declared_by
from ._node_meta_declaration import (
    NodeDeclaredBy,
    take_node_meta_held_for_the_kind_decorator,
    the_conversion_a_node_states,
)
from ._bundle_pin_type import dataclass_a_bundle_pin_is_written_as
from ._pin_declaration import (
    NO_DEFAULT,
    DeclaredPin,
    NodeDeclarationRefusal,
    read_pin_declaration,
)
from ._former_name_check import refuse_former_names_that_find_no_one_pin
from ._pin_family import family_a_pin_type_travels_on, family_declaration_of_a_typed_output
from ._pin_types import PIN_VALUE_CLASSES, PinValueOfParts

__all__ = [
    "FunctionNode",
    "State",
    "Stateful",
    "Time",
]

_FunctionNodeParameters = ParamSpec("_FunctionNodeParameters")
_FunctionNodeReturn = TypeVar("_FunctionNodeReturn", covariant=True)

type Time = int
"""The reader's stream time at the read, in integer nanoseconds on the stream's monotonic clock."""

type State[StateRecord] = StateRecord
"""The state a stateful function node's last call returned, or its record's defaults on the first call."""

type Stateful[FunctionNodeOutput, StateRecord] = tuple[FunctionNodeOutput, StateRecord]
"""What a stateful function node returns: its output, then the state its next call takes."""

FUNCTION_NODE_KIND: Final = "function"
PYTHON_FUNCTION_NODE_LANGUAGE = "python"

# Wire contract with the runtime's reader: how the signature is called, stamped
# beside the declaration stamps every node carries.
NODE_DECLARATION_LANGUAGE_STAMP = "__tatolab_node_language__"
NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP = "__tatolab_node_function_signature__"
NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP = "__tatolab_node_function_state_fields__"

# The value type of a message a function node hands on unread.
UNREAD_MESSAGE_VALUE_TYPE = "unread_message"

# The family an input that hands on what its link brings is declared with, beside every other
# family it also takes.
UNREAD_MESSAGE_INPUT_FAMILY = {
    "family": "image",
    "also_takes_families": ["array", "channels", "packets", "records"],
}

_FUNCTION_LOCAL_MARKER = "<locals>"

# The value type a shader's picture arrives as.
SHADER_IMAGE_VALUE_TYPE = "image_frame"

# The output a function's one returned value is.
_SINGLE_OUTPUT_NAME = "result"

# The value a typed pin of each built-in number type hands the body, as the signature stamp
# names it; a value class of parts names its own.
_SIGNATURE_VALUE_TYPE_OF_EACH_PIN_TYPE_NAME: "dict[str, str]" = {
    "boolean": "bool",
    "integer": "int",
    "float": "float",
    "string": "string",
    "menu": "menu",
    "asset": "asset",
    "list_of": "list_of",
    "bundle": "bundle",
    "named": "named",
}


class FunctionNode(Generic[_FunctionNodeParameters, _FunctionNodeReturn]):
    """A function under bare `@node`: called as the function it wraps, added to a stream as a node."""

    __wrapped__: Callable[_FunctionNodeParameters, _FunctionNodeReturn]
    __name__: str
    __qualname__: str

    def __init__(self, function: Callable[_FunctionNodeParameters, _FunctionNodeReturn]) -> None:
        functools.update_wrapper(self, function)

    def __call__(
        self,
        *arguments: _FunctionNodeParameters.args,
        **keyword_arguments: _FunctionNodeParameters.kwargs,
    ) -> _FunctionNodeReturn:
        return self.__wrapped__(*arguments, **keyword_arguments)

    def __repr__(self) -> str:
        return f"<function node {self.__module__}:{self.__qualname__}>"


def declare_function_node(
    function: Callable[_FunctionNodeParameters, _FunctionNodeReturn],
    *,
    declaration_stamps: "dict[str, str]",
) -> FunctionNode[_FunctionNodeParameters, _FunctionNodeReturn]:
    """Read `function`'s signature into a function node, refusing by name what is not one.

    `declaration_stamps` names the stamps every node carries, as `_node_declaration` spells
    them, so every kind stamps one set. Each pin declares its pin type, and a mistake is a
    `NodeDeclarationRefusal`.
    """
    decorator: NodeDeclaredBy = "@node"
    _refuse_what_is_not_a_plain_module_level_function(function, decorator)
    declared_meta = take_node_meta_held_for_the_kind_decorator(function)
    signature_reading = _FunctionNodeSignatureReading(function)
    refuse_a_function_node_body_with_side_effects(function)
    function_node = FunctionNode(function)
    stamps: "dict[str, Any]" = {
        declaration_stamps["declared"]: True,
        declaration_stamps["kind"]: FUNCTION_NODE_KIND,
        NODE_DECLARATION_LANGUAGE_STAMP: PYTHON_FUNCTION_NODE_LANGUAGE,
        declaration_stamps["description"]: declared_meta.get("description", ""),
        declaration_stamps["category"]: declared_meta.get("category"),
        declaration_stamps["conversion"]: the_conversion_a_node_states(
            function.__name__,
            declared_meta,
            declared_by=decorator,
            kind=FUNCTION_NODE_KIND,
            input_ports=signature_reading.input_ports,
            output_ports=signature_reading.output_ports,
        ),
        declaration_stamps["declared_by"]: decorator,
        declaration_stamps["input_ports"]: signature_reading.input_ports,
        declaration_stamps["output_ports"]: signature_reading.output_ports,
        NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP: signature_reading.function_signature,
        NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP: signature_reading.state_fields,
    }
    for stamp, value in stamps.items():
        setattr(function_node, stamp, value)
    return function_node


def _refuse_what_is_not_a_plain_module_level_function(function: object, decorator: str) -> None:
    if isinstance(function, type):
        raise TypeError(
            f"{decorator} takes a function, and {function.__name__} is a class, which bare "
            f"`@node` reads as a class node."
        )
    if not inspect.isfunction(function):
        raise TypeError(
            f"{decorator} takes a function defined with `def`, got {function!r}."
        )
    name = function.__name__
    if name == "<lambda>":
        raise TypeError(
            f"{decorator} takes a function defined with `def` at module scope, and a lambda "
            "has no name a reader can import. To fix: write it as a `def`."
        )
    if _FUNCTION_LOCAL_MARKER in function.__qualname__ or function.__closure__:
        raise TypeError(
            f"{name} is defined inside another function, so its reader cannot import it and "
            f"anything it closes over never reaches the reader. To fix: move it to module "
            f"scope and pass what it closed over as an input."
        )
    if inspect.iscoroutinefunction(function) or inspect.isasyncgenfunction(function):
        raise TypeError(
            f"{name} is `async def`; a function node computes its outputs and returns. "
            f"To fix: make it a plain `def`, or a class node if it waits on I/O."
        )
    if inspect.isgeneratorfunction(function):
        raise TypeError(
            f"{name} is a generator; a function node returns its outputs once per call. "
            f"To fix: `return` the value, and keep what carries between calls in `State[...]`."
        )


class _FunctionNodeSignatureReading:
    """A function's signature read into its ports and how it is called."""

    def __init__(self, function: Callable[..., Any], *, reads_shader_images: bool = False) -> None:
        """Read `function`'s signature, each pin typed by its pin type.

        `reads_shader_images` reads it as a shader's: each `Texture` it takes or returns, written
        `Texture` or `ImageFrame`, is a picture it computes over, which arrives as an Image.
        """
        self._function_name = function.__name__
        self._reads_shader_images = reads_shader_images
        annotations = self._resolved_annotations(function)
        self.input_ports: "list[dict[str, Any]]" = []
        self._signature_inputs: "list[dict[str, Any]]" = []
        self._time_parameter: Optional[str] = None
        self._state_parameter: Optional[str] = None
        self._state_record_class: Optional[type] = None
        self._port_spelling_by_cast_name: "dict[str, str]" = {}
        self._typed_pins: "list[tuple[str, str, PinDeclarationOnTheWire]]" = []
        self._input_ports_of_each_type_parameter: "dict[typing.TypeVar, list[str]]" = {}

        for parameter in inspect.signature(function).parameters.values():
            self._read_parameter(parameter, annotations)

        if "return" not in annotations:
            raise TypeError(
                f"{self._function_name} has no return annotation, so nothing names its "
                f"output. To fix: annotate the return, `-> float`, or a `NamedTuple` for "
                f"several outputs."
            )
        self.output_ports, self._signature_outputs = self._read_return(annotations["return"])
        refuse_former_names_that_find_no_one_pin(self._function_name, self._typed_pins)

    @property
    def function_signature(self) -> "dict[str, Any]":
        """The stamp the runtime calls the body by."""
        return {
            "inputs": self._signature_inputs,
            "outputs": self._signature_outputs,
            "time_parameter": self._time_parameter,
            "state_parameter": self._state_parameter,
            "state_record_class": (
                None
                if self._state_record_class is None
                else f"{self._state_record_class.__module__}:{self._state_record_class.__qualname__}"
            ),
        }

    @property
    def state_fields(self) -> "list[str]":
        """The state record's fields, in the order the runtime carries their values; none when stateless."""
        if self._state_record_class is None:
            return []
        return [field.name for field in dataclasses.fields(self._state_record_class)]

    def _resolved_annotations(self, function: Callable[..., Any]) -> "dict[str, Any]":
        try:
            # A type parameter is named in the function's own scope, which Python 3.12 does not
            # read a string annotation in.
            type_parameters = {
                type_parameter.__name__: type_parameter
                for type_parameter in getattr(function, "__type_params__", ())
            }
            return typing.get_type_hints(function, localns=type_parameters, include_extras=True)
        except Exception as unresolvable:
            raise TypeError(
                f"{self._function_name} carries an annotation that cannot be resolved, so its "
                f"ports cannot be read: {unresolvable}. Every annotation must be importable "
                f"at run time, not only under `TYPE_CHECKING`."
            ) from unresolvable

    def _read_parameter(
        self, parameter: inspect.Parameter, annotations: "dict[str, Any]"
    ) -> None:
        if parameter.kind is inspect.Parameter.VAR_POSITIONAL:
            raise TypeError(
                f"{self._function_name} takes `*{parameter.name}`; every input of a function "
                f"node is a named pin. To fix: name each input as its own parameter."
            )
        if parameter.kind is inspect.Parameter.VAR_KEYWORD:
            raise TypeError(
                f"{self._function_name} takes `**{parameter.name}`; every input of a function "
                f"node is a named pin. To fix: name each input as its own parameter."
            )
        if parameter.name not in annotations:
            raise NodeDeclarationRefusal(
                f"`{self._function_name}`'s parameter `{parameter.name}` has no type, so "
                f"nothing names the pin's type. To fix: annotate it, "
                f"`{parameter.name}: float = 0.0`.",
                finding_code="pin_has_no_type",
                node=self._function_name,
                pin=parameter.name,
            )
        refuse_a_pin_group_on_a_function_node(self._function_name, parameter.name, annotations[parameter.name])
        port_description, annotation = self._port_description_and_annotation_without_it(
            annotations[parameter.name], f"input `{parameter.name}`"
        )
        if port_description is not None and (
            annotation is Time or typing.get_origin(annotation) is State
        ):
            raise TypeError(
                f"{self._function_name}'s parameter `{parameter.name}` carries a "
                f"`Description`, and only a port takes one. To fix: drop it, or describe "
                f"the node with `@node.meta(description=...)`."
            )
        if annotation is Time:
            self._read_time_parameter(parameter)
        elif typing.get_origin(annotation) is State:
            self._read_state_parameter(parameter, annotation)
        elif parameter.name == "config":
            raise NodeDeclarationRefusal(
                f"`{self._function_name}` takes `config`, and a node takes each setting as an "
                f"input of its own, with its default. To fix: write each setting as a "
                f"parameter, `radius: float = 4.0`.",
                finding_code="parameter_not_a_pin",
                node=self._function_name,
                pin=parameter.name,
            )
        elif self._reads_shader_images and _is_a_shader_image(annotation):
            self._read_shader_image_input(parameter, port_description or "")
        else:
            self._read_typed_input_pin(parameter, annotation, port_description or "")

    def _port_description_and_annotation_without_it(
        self, annotation: Any, what_it_types: str
    ) -> "tuple[Optional[str], Any]":
        """The `Description` on a port's annotation, also on `T` of `T | None`, and the annotation without it."""
        described = f"{self._function_name}'s {what_it_types}"
        description, annotation_without_it = description_and_annotation_without_it(
            annotation, described
        )
        if description is not None:
            return description, annotation_without_it
        value_annotation, optional = _value_annotation_and_whether_optional(annotation)
        if not optional:
            return None, annotation
        description, value_annotation_without_it = description_and_annotation_without_it(
            value_annotation, described
        )
        if description is None:
            return None, annotation
        return description, Optional[value_annotation_without_it]

    def _read_time_parameter(self, parameter: inspect.Parameter) -> None:
        if self._time_parameter is not None:
            raise TypeError(
                f"{self._function_name} takes `Time` twice, as `{self._time_parameter}` and "
                f"`{parameter.name}`; it is handed once. To fix: keep one."
            )
        self._time_parameter = parameter.name

    def _read_state_parameter(self, parameter: inspect.Parameter, annotation: Any) -> None:
        if self._state_parameter is not None:
            raise TypeError(
                f"{self._function_name} takes `State[...]` twice, as `{self._state_parameter}` "
                f"and `{parameter.name}`; a function node has one state record. To fix: put "
                f"every field on one dataclass."
            )
        (state_record_class,) = typing.get_args(annotation)
        self._refuse_a_state_record_that_is_not_one(state_record_class)
        self._state_parameter = parameter.name
        self._state_record_class = state_record_class

    def _refuse_a_state_record_that_is_not_one(self, state_record_class: Any) -> None:
        if not (isinstance(state_record_class, type) and dataclasses.is_dataclass(state_record_class)):
            raise TypeError(
                f"{self._function_name} takes `State[{state_record_class!r}]`, and a state is a "
                f"dataclass of numbers. To fix: declare `@dataclass(frozen=True) class ...` and "
                f"name it in `State[...]` and `Stateful[..., ...]`."
            )
        fields_without_a_default = [
            field.name
            for field in dataclasses.fields(state_record_class)
            if field.default is dataclasses.MISSING
            and field.default_factory is dataclasses.MISSING
        ]
        if fields_without_a_default:
            raise TypeError(
                f"{self._function_name}'s state record {state_record_class.__name__} gives no "
                f"default for {', '.join(fields_without_a_default)}, and the first call takes "
                f"the record's defaults. To fix: give every field a default."
            )

    def _read_shader_image_input(self, parameter: inspect.Parameter, port_description: str) -> None:
        """Read a shader's `Texture` parameter, the picture it computes over, as an Image input."""
        port_name = self._cast_port_name(parameter.name)
        self._signature_inputs.append(
            {
                "parameter": parameter.name,
                "port": port_name,
                "value_type": SHADER_IMAGE_VALUE_TYPE,
                "optional": False,
            }
        )
        self.input_ports.append(
            {
                "name": port_name,
                "description": port_description,
                "delivery_profile": "newest",
                "family": {"family": "image"},
            }
        )

    def _read_typed_input_pin(
        self, parameter: inspect.Parameter, annotation: Any, port_description: str
    ) -> None:
        port_name = self._cast_port_name(parameter.name)
        declared = read_pin_declaration(
            self._function_name,
            parameter.name,
            annotation,
            default=(
                NO_DEFAULT if parameter.default is inspect.Parameter.empty else parameter.default
            ),
            direction="input",
        )
        self._typed_pins.append((parameter.name, port_name, declared.declaration))
        if declared.type_parameter is not None:
            self._input_ports_of_each_type_parameter.setdefault(declared.type_parameter, []).append(
                port_name
            )
        signature_input: "dict[str, Any]" = {
            "parameter": parameter.name,
            "port": port_name,
            "value_type": _signature_value_type_of(declared),
        }
        if signature_input["value_type"] != UNREAD_MESSAGE_VALUE_TYPE and _is_exactly_one_type(
            declared.declaration
        ):
            signature_input["pin_type"] = _the_pin_type_a_pin_declares(declared.declaration)
            self._state_the_dataclass_of_a_bundle(signature_input, parameter.name, annotation)
        signature_input["optional"] = declared.optional
        input_port: "dict[str, Any]" = {
            "name": port_name,
            "description": port_description,
            "family": _family_an_input_travels_on(declared),
            "pin_declaration": declared.declaration,
        }
        if declared.default_value is not NO_DEFAULT:
            signature_input["default"] = declared.default_value
            input_port["pin_default"] = declared.default_value
        self._signature_inputs.append(signature_input)
        self.input_ports.append(input_port)

    def _state_the_dataclass_of_a_bundle(
        self, signature_entry: "dict[str, Any]", pin_name: str, annotation: Any
    ) -> None:
        """Name on `signature_entry` the dataclass its bundle, or its List's bundle elements,
        are written as, `module:QualifiedName`, which the body is handed and gives back."""
        pin_type = signature_entry["pin_type"]
        if pin_type["type"] == "list_of":
            pin_type = pin_type["element_type"]
        if pin_type["type"] != "bundle":
            return
        bundle_class = dataclass_a_bundle_pin_is_written_as(annotation)
        assert bundle_class is not None, "a bundle pin is written as a dataclass"
        if _FUNCTION_LOCAL_MARKER in bundle_class.__qualname__.split("."):
            raise NodeDeclarationRefusal(
                f"`{self._function_name}`'s pin `{pin_name}` is typed `{bundle_class.__name__}`, "
                f"a dataclass declared inside a function, which the node's reader does not "
                f"find by its import path. To fix: declare `{bundle_class.__name__}` at the top "
                f"level of its module.",
                finding_code="pin_type_unknown",
                node=self._function_name,
                pin=pin_name,
            )
        signature_entry["bundle_class"] = f"{bundle_class.__module__}:{bundle_class.__qualname__}"

    def _read_return(
        self, return_annotation: Any
    ) -> "tuple[list[dict[str, Any]], list[dict[str, Any]]]":
        returned_description, returned_without_it = self._port_description_and_annotation_without_it(
            return_annotation, "return annotation"
        )
        if returned_description is not None and (
            typing.get_origin(returned_without_it) is Stateful
            or _is_a_named_tuple(returned_without_it)
        ):
            raise TypeError(
                f"{self._function_name}'s return annotation carries a `Description` on what "
                f"is not one output. To fix: describe each output where it is typed, "
                f"`field: Annotated[T, Description(\"...\")]` on the named tuple, or "
                f"`Stateful[Annotated[T, Description(\"...\")], Record]`."
            )
        if return_annotation is type(None):
            raise TypeError(
                f"{self._function_name} returns `None`, so it has no output and computes "
                f"nothing a graph can read. To fix: return the value it computes, or make it "
                f"a class node if its work is a side effect."
            )
        if typing.get_origin(return_annotation) is Stateful:
            output_annotation, returned_state_record_class = typing.get_args(return_annotation)
            if self._state_record_class is None:
                raise TypeError(
                    f"{self._function_name} returns `Stateful[...]` but takes no `State[...]`, "
                    f"so the state it returns never reaches its next call. To fix: take "
                    f"`previous: State[{_name_of(returned_state_record_class)}]`."
                )
            if returned_state_record_class is not self._state_record_class:
                raise TypeError(
                    f"{self._function_name} takes `State[{_name_of(self._state_record_class)}]` "
                    f"but returns `Stateful[..., {_name_of(returned_state_record_class)}]`; the "
                    f"state it returns is the one its next call takes. To fix: name one record "
                    f"in both."
                )
        else:
            output_annotation = return_annotation
            if self._state_record_class is not None:
                raise TypeError(
                    f"{self._function_name} takes `State[{_name_of(self._state_record_class)}]` "
                    f"but returns no `Stateful[...]`, so its next call never sees a new state. "
                    f"To fix: `-> Stateful[<output>, {_name_of(self._state_record_class)}]`, "
                    f"returning `(output, new_state)`."
                )
        if self._reads_shader_images and _is_a_shader_image(output_annotation):
            return self._shader_image_output(returned_description or "")
        return self._typed_output_ports(output_annotation)

    def _shader_image_output(
        self, port_description: str
    ) -> "tuple[list[dict[str, Any]], list[dict[str, Any]]]":
        """A shader's returned `Texture`, the picture it computes, as its one Image output."""
        port_name = self._cast_port_name(_SINGLE_OUTPUT_NAME)
        return (
            [{"name": port_name, "description": port_description, "family": {"family": "image"}}],
            [
                {
                    "port": port_name,
                    "field": None,
                    "value_type": SHADER_IMAGE_VALUE_TYPE,
                    "may_hand_out_nothing": False,
                }
            ],
        )

    def _typed_output_ports(
        self, output_annotation: Any
    ) -> "tuple[list[dict[str, Any]], list[dict[str, Any]]]":
        """The outputs a typed return names: `result`, or one per field of a `NamedTuple`."""
        record_that_may_be_none, may_be_none = _value_annotation_and_whether_optional(
            output_annotation
        )
        if _is_a_named_tuple(record_that_may_be_none):
            if may_be_none:
                raise NodeDeclarationRefusal(
                    f"`{self._function_name}` returns {_name_of(record_that_may_be_none)} | None, "
                    f"and a named tuple of outputs hands out each of its fields, so it is never "
                    f"`None` as a whole. To fix: return {_name_of(record_that_may_be_none)} and "
                    f"type each field that may hand out nothing `T | None`.",
                    finding_code="return_not_read_yet",
                    node=self._function_name,
                    pin=_SINGLE_OUTPUT_NAME,
                )
            field_annotations = typing.get_type_hints(record_that_may_be_none, include_extras=True)
            fields_of = f"{record_that_may_be_none.__module__}:{record_that_may_be_none.__qualname__}"
            output_ports: "list[dict[str, Any]]" = []
            signature_outputs: "list[dict[str, Any]]" = []
            for field_name in record_that_may_be_none._fields:
                output_port, signature_output = self._typed_output_port(
                    field_name, field_annotations[field_name]
                )
                signature_output["field"] = field_name
                signature_output["fields_of"] = fields_of
                output_ports.append(output_port)
                signature_outputs.append(signature_output)
            return output_ports, signature_outputs
        output_port, signature_output = self._typed_output_port(_SINGLE_OUTPUT_NAME, output_annotation)
        return [output_port], [signature_output]

    def _typed_output_port(
        self, output_name: str, described_output_annotation: Any
    ) -> "tuple[dict[str, Any], dict[str, Any]]":
        port_description, output_annotation = self._port_description_and_annotation_without_it(
            described_output_annotation, f"output `{output_name}`"
        )
        value_annotation, _ = _value_annotation_and_whether_optional(output_annotation)
        if (
            value_annotation not in PIN_VALUE_CLASSES
            and named_pin_type_declared_by(value_annotation) is None
        ) and (
            typing.get_origin(value_annotation) is tuple
            or value_annotation is tuple
            or (isinstance(value_annotation, type) and issubclass(value_annotation, tuple))
        ):
            raise NodeDeclarationRefusal(
                f"`{self._function_name}`'s output `{output_name}` is typed "
                f"{_name_of(value_annotation)}, and a tuple does not name the outputs it holds. "
                f"To fix: return a `NamedTuple`, `class Bounds(NamedTuple): low: float; high: "
                f"float`, whose fields name its outputs.",
                finding_code="outputs_not_named",
                node=self._function_name,
                pin=output_name,
            )
        port_name = self._cast_port_name(output_name)
        declared = read_pin_declaration(
            self._function_name,
            output_name,
            output_annotation,
            direction="output",
            input_ports_of_each_type_parameter=self._input_ports_of_each_type_parameter,
        )
        self._typed_pins.append((output_name, port_name, declared.declaration))
        output_port = {
            "name": port_name,
            "description": port_description or "",
            "family": self._family_an_output_travels_on(declared, port_name),
            "pin_declaration": declared.declaration,
        }
        signature_output: "dict[str, Any]" = {
            "port": port_name,
            "field": None,
            "value_type": _signature_value_type_of(declared),
        }
        if signature_output["value_type"] != UNREAD_MESSAGE_VALUE_TYPE:
            signature_output["pin_type"] = _the_pin_type_a_pin_declares(declared.declaration)
            self._state_the_dataclass_of_a_bundle(signature_output, output_name, output_annotation)
        signature_output["may_hand_out_nothing"] = declared.optional
        return output_port, signature_output

    def _family_an_output_travels_on(self, declared: DeclaredPin, port_name: str) -> "dict[str, Any]":
        declared_pin_type = declared.declaration["pin_type"]
        if declared_pin_type["kind"] == "same_as_input":
            return {
                "family": UNREAD_MESSAGE_INPUT_FAMILY["family"],
                "family_follows_input_port": declared_pin_type["input_port_name"],
            }
        return family_declaration_of_a_typed_output(port_name, declared.declaration)

    def _cast_port_name(self, spelling: str) -> str:
        try:
            port_name = cast_exposed_name_to_url_safe(spelling)
        except ExposedNameCastsToNothingError as names_nothing:
            raise ValueError(
                f"{self._function_name} declares a port {spelling!r}: {names_nothing}"
            ) from names_nothing
        first_spelling = self._port_spelling_by_cast_name.get(port_name)
        if first_spelling is not None:
            raise ValueError(
                f"{self._function_name} declares the ports {first_spelling!r} and "
                f"{spelling!r}, which both cast to {port_name!r}; two ports need names that "
                f"stay apart once cast."
            )
        self._port_spelling_by_cast_name[port_name] = spelling
        return port_name


def _is_exactly_one_type(declaration: PinDeclarationOnTheWire) -> bool:
    return declaration["pin_type"]["kind"] == "is"


def _the_value_class_of_parts_a_pin_declares(
    declaration: PinDeclarationOnTheWire,
) -> "Optional[type[PinValueOfParts]]":
    """The value class of the type a typed pin declares, when it travels as its parts; `None` for a number."""
    pin_type = _the_pin_type_a_pin_declares(declaration)
    for value_class in PIN_VALUE_CLASSES:
        if value_class.pin_type == pin_type:
            return value_class
    return None


def _the_pin_type_a_pin_declares(declaration: PinDeclarationOnTheWire) -> "dict[str, Any]":
    """The one pin type a typed pin declares exactly, as `graph` serves it."""
    declared_pin_type = declaration["pin_type"]
    assert declared_pin_type["kind"] == "is", "the pin declares exactly one type"
    return dict(declared_pin_type["pin_type"])


def _family_an_input_travels_on(declared: DeclaredPin) -> "dict[str, Any]":
    """The family a typed input reads: its one type's, the one all its types share, or every family.

    An input that takes more than one family reads every family, as one that hands on what its
    link brings does.
    """
    declared_pin_type = declared.declaration["pin_type"]
    if declared_pin_type["kind"] == "is":
        pin_types = [declared_pin_type["pin_type"]]
    elif declared_pin_type["kind"] == "one_of":
        pin_types = declared_pin_type["pin_types"]
    else:
        pin_types = []
    families = {family_a_pin_type_travels_on(pin_type) for pin_type in pin_types}
    if len(families) == 1:
        (family,) = families
        return {"family": family}
    return dict(UNREAD_MESSAGE_INPUT_FAMILY)


# The pin types whose value travels as one f64, which a pin of several of them is handed as.
_TYPES_OF_ONE_NUMBER = frozenset({"boolean", "integer", "float"})


def _signature_value_type_of(declared: DeclaredPin) -> str:
    """The value a typed pin hands the body, as the signature stamp names it.

    A pin typed by a type parameter, or taking any type or types of more than one form, hands
    the body what its link brings, unread; one taking several number types, the number.
    """
    declared_pin_type = declared.declaration["pin_type"]
    if declared.type_parameter is not None or declared_pin_type["kind"] in ("any", "same_as_input"):
        return UNREAD_MESSAGE_VALUE_TYPE
    if declared_pin_type["kind"] == "one_of":
        if all(pin_type["type"] in _TYPES_OF_ONE_NUMBER for pin_type in declared_pin_type["pin_types"]):
            return "float"
        return UNREAD_MESSAGE_VALUE_TYPE
    value_class = _the_value_class_of_parts_a_pin_declares(declared.declaration)
    if value_class is not None:
        return value_class.value_type
    return _SIGNATURE_VALUE_TYPE_OF_EACH_PIN_TYPE_NAME[
        _the_pin_type_a_pin_declares(declared.declaration)["type"]
    ]


def _is_a_named_tuple(annotation: Any) -> bool:
    return (
        isinstance(annotation, type)
        and issubclass(annotation, tuple)
        and hasattr(annotation, "_fields")
        and annotation not in PIN_VALUE_CLASSES
        and named_pin_type_declared_by(annotation) is None
    )


def _is_a_shader_image(annotation: Any) -> bool:
    """Whether a shader's annotation is a picture it computes over: `Texture` or `ImageFrame`, also `Annotated`."""
    from ._pin_types import Texture
    from .image_frame import ImageFrame

    if typing.get_origin(annotation) is typing.Annotated:
        annotation = typing.get_args(annotation)[0]
    return annotation is Texture or annotation is ImageFrame


def _value_annotation_and_whether_optional(annotation: Any) -> "tuple[Any, bool]":
    arguments = typing.get_args(annotation)
    if type(None) in arguments and len(arguments) == 2:
        (value_annotation,) = [each for each in arguments if each is not type(None)]
        return value_annotation, True
    return annotation, False


def _name_of(annotation: Any) -> str:
    return getattr(annotation, "__name__", repr(annotation))
