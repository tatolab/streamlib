# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@node.function` grammar: a function whose signature is the node.

A plain parameter is an input pin and its default the pin's value; the return
annotation is the output `result`, or one output per field of a dataclass. An
output typed `T | None` hands out nothing on a call that returns `None`. An
`UnreadMessage` input takes a message of any family, which the body can only
hand on, to an `UnreadMessage` output of the family linked into that input;
`Annotated[UnreadMessage, Image]` takes the one family it names. A state field
may hold an `UnreadMessage`, which a helper restart drops. `config` names the
node's one config dataclass. `Time` and `State[...]` are found by type and are
never pins. Decorating registers nothing: it reads the
signature once and stamps the declaration the runtime reads, the same stamps a
`@node.process` class carries plus the signature the runtime calls the body
with.

Bare `@node` reads the same signature in the typed grammar: each pin is read by
`read_pin_declaration` into the pin type it declares and what it states besides,
which its port carries to the runtime as `pin_declaration`, and no spelling of
the family grammar is read there. One returned value is the output `result`, and
a `NamedTuple` one output per field. A pin typed by a type parameter of the
function, `def Gate[PinValue](value: PinValue) -> PinValue`, hands on unread what
its link brings, as an `UnreadMessage` does, and its output takes the type linked
into its input.
"""

from __future__ import annotations

import dataclasses
import functools
import inspect
import typing
from typing import Any, Callable, Generic, Optional, ParamSpec, TypeVar

from ._contract_pin_types import PinDeclarationOnTheWire
from ._description import description_and_annotation_without_it
from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe
from ._function_node_side_effect_check import refuse_a_function_node_body_with_side_effects
from ._node_config_schema import (
    derive_node_config_json_schema_refusing_a_key_with_no_default,
    json_schema_for_a_node_declaring_no_config,
)
from ._named_pin_type_declaration import named_pin_type_declared_by
from ._node_meta_declaration import take_node_meta_held_for_the_kind_decorator
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

FUNCTION_NODE_KIND = "function"
PYTHON_FUNCTION_NODE_LANGUAGE = "python"

# Wire contract with the runtime's reader: how the signature is called, stamped
# beside the declaration stamps every node carries.
NODE_DECLARATION_LANGUAGE_STAMP = "__tatolab_node_language__"
NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP = "__tatolab_node_function_signature__"
NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP = "__tatolab_node_function_state_fields__"

# The value a scalar pin carries, by the annotation that names it.
_SCALAR_PIN_VALUE_TYPES: "dict[Any, str]" = {float: "float", int: "int", bool: "bool"}

# The value type of a message a function node hands on unread.
UNREAD_MESSAGE_VALUE_TYPE = "unread_message"

# The family an `UnreadMessage` input is declared with, beside every other family it also takes.
UNREAD_MESSAGE_INPUT_FAMILY = {
    "family": "image",
    "also_takes_families": ["array", "channels", "packets", "records"],
}

_FUNCTION_LOCAL_MARKER = "<locals>"

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
    "named": "named",
}


class FunctionNode(Generic[_FunctionNodeParameters, _FunctionNodeReturn]):
    """A `@node.function` function: called as the function it wraps, added to a stream as a node."""

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
    reads_pin_types: bool = False,
) -> FunctionNode[_FunctionNodeParameters, _FunctionNodeReturn]:
    """Read `function`'s signature into a function node, refusing by name what is not one.

    `declaration_stamps` names the stamps every node carries, as
    `_node_declaration` spells them, so both kinds stamp one set. `reads_pin_types` reads the
    typed grammar of bare `@node`, where each pin declares its pin type and a mistake is a
    `NodeDeclarationRefusal`; otherwise it reads the family grammar of `@node.function`.
    """
    decorator = "@node" if reads_pin_types else "@node.function"
    _refuse_what_is_not_a_plain_module_level_function(function, decorator)
    declared_meta = take_node_meta_held_for_the_kind_decorator(function)
    signature_reading = _FunctionNodeSignatureReading(function, reads_pin_types=reads_pin_types)
    refuse_a_function_node_body_with_side_effects(function)
    function_node = FunctionNode(function)
    config_class = signature_reading.config_class
    stamps: "dict[str, Any]" = {
        declaration_stamps["declared"]: True,
        declaration_stamps["kind"]: FUNCTION_NODE_KIND,
        NODE_DECLARATION_LANGUAGE_STAMP: PYTHON_FUNCTION_NODE_LANGUAGE,
        declaration_stamps["config_class"]: config_class,
        declaration_stamps["config_schema"]: (
            json_schema_for_a_node_declaring_no_config()
            if config_class is None
            else derive_node_config_json_schema_refusing_a_key_with_no_default(
                config_class, f"{decorator} on {function.__name__}"
            )
        ),
        declaration_stamps["description"]: declared_meta.get("description", ""),
        declaration_stamps["category"]: declared_meta.get("category"),
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
            f"{decorator} takes a function, and {function.__name__} is a class. A class "
            f"with `process()` and ports is `@node.process`."
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
            f"scope and pass what it closed over as an input or as `config`."
        )
    if inspect.iscoroutinefunction(function) or inspect.isasyncgenfunction(function):
        raise TypeError(
            f"{name} is `async def`; a function node computes its outputs and returns. "
            f"To fix: make it a plain `def`, or a `@node.process` class if it waits on I/O."
        )
    if inspect.isgeneratorfunction(function):
        raise TypeError(
            f"{name} is a generator; a function node returns its outputs once per call. "
            f"To fix: `return` the value, and keep what carries between calls in `State[...]`."
        )


class _FunctionNodeSignatureReading:
    """A function's signature read into its ports, its config class and how it is called."""

    def __init__(
        self,
        function: Callable[..., Any],
        *,
        reads_pin_types: bool,
        reads_values_typed_beside_images: bool = False,
    ) -> None:
        """Read `function`'s signature, under the typed grammar when `reads_pin_types`.

        `reads_values_typed_beside_images` reads each input typed by a port family's read type,
        an `ImageFrame` among them, under the family grammar and every other under the typed
        grammar, as a shader's signature is read: its Images are the pictures it computes over,
        and its values carry their pin types.
        """
        self._function_name = function.__name__
        self._reads_pin_types = reads_pin_types
        self._reads_values_typed_beside_images = reads_values_typed_beside_images
        annotations = self._resolved_annotations(function)
        self.input_ports: "list[dict[str, Any]]" = []
        self._signature_inputs: "list[dict[str, Any]]" = []
        self._time_parameter: Optional[str] = None
        self._state_parameter: Optional[str] = None
        self._state_record_class: Optional[type] = None
        self._config_parameter: Optional[str] = None
        self.config_class: Optional[type] = None
        self._port_spelling_by_cast_name: "dict[str, str]" = {}
        self._unread_message_input_ports: "list[str]" = []
        self._typed_pins: "list[tuple[str, str, PinDeclarationOnTheWire]]" = []
        self._input_ports_of_each_type_parameter: "dict[typing.TypeVar, list[str]]" = {}

        for parameter in inspect.signature(function).parameters.values():
            self._read_parameter(parameter, annotations)

        if "return" not in annotations:
            raise TypeError(
                f"{self._function_name} has no return annotation, so nothing names its "
                f"output. To fix: annotate the return, `-> float`, or a dataclass for "
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
            "config_parameter": self._config_parameter,
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
                f"node is a named pin. To fix: name each input as its own parameter, and put "
                f"settings on a `config` dataclass."
            )
        if parameter.name not in annotations:
            if self._reads_pin_types:
                raise NodeDeclarationRefusal(
                    f"`{self._function_name}`'s parameter `{parameter.name}` has no type, so "
                    f"nothing names the pin's type. To fix: annotate it, "
                    f"`{parameter.name}: float = 0.0`.",
                    finding_code="pin_has_no_type",
                    node=self._function_name,
                    pin=parameter.name,
                )
            raise TypeError(
                f"{self._function_name}'s parameter `{parameter.name}` has no type, so nothing "
                f"names the pin's family. To fix: annotate it, `{parameter.name}: float`."
            )
        port_description, annotation = self._port_description_and_annotation_without_it(
            annotations[parameter.name], f"input `{parameter.name}`"
        )
        if port_description is not None and (
            annotation is Time
            or typing.get_origin(annotation) is State
            or parameter.name == "config"
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
        elif parameter.name == "config" and self._reads_pin_types:
            raise NodeDeclarationRefusal(
                f"`{self._function_name}` takes `config`, and a function under bare `@node` "
                f"takes each setting as an input of its own, with its default. To fix: write "
                f"each field of the config class as a parameter.",
                finding_code="parameter_not_a_pin",
                node=self._function_name,
                pin=parameter.name,
            )
        elif parameter.name == "config":
            self._read_config_parameter(annotation)
        elif self._reads_pin_types or (
            self._reads_values_typed_beside_images and not _names_a_port_family(annotation)
        ):
            self._read_typed_input_pin(parameter, annotation, port_description or "")
        else:
            self._read_input_pin(parameter, annotation, port_description or "")

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

    def _read_config_parameter(self, annotation: Any) -> None:
        if not (isinstance(annotation, type) and dataclasses.is_dataclass(annotation)):
            raise TypeError(
                f"{self._function_name} annotates `config` as {annotation!r}, and a function "
                f"node's config is one dataclass. To fix: `config: "
                f"{_upper_camel_case(self._function_name)}Config`, a `@dataclass(frozen=True)`."
            )
        self._config_parameter = "config"
        self.config_class = annotation

    def _read_input_pin(
        self, parameter: inspect.Parameter, annotation: Any, port_description: str
    ) -> None:
        port_name = self._cast_port_name(parameter.name)
        value_annotation, optional = _value_annotation_and_whether_optional(annotation)
        if optional and parameter.default is not None:
            raise TypeError(
                f"{self._function_name}'s input `{parameter.name}` is optional, so its pin "
                f"takes `None` when nothing is wired. To fix: `{parameter.name}: ... | None = "
                f"None`."
            )
        pin_value_type, family_declaration = self._pin_value_type_and_family(
            value_annotation, f"input `{parameter.name}`"
        )
        if pin_value_type == UNREAD_MESSAGE_VALUE_TYPE:
            self._unread_message_input_ports.append(port_name)
        signature_input: "dict[str, Any]" = {
            "parameter": parameter.name,
            "port": port_name,
            "value_type": pin_value_type,
            "optional": optional,
        }
        if optional:
            signature_input["default"] = None
        elif parameter.default is not inspect.Parameter.empty:
            signature_input["default"] = self._pin_default(parameter, pin_value_type)
        self._signature_inputs.append(signature_input)
        input_port: "dict[str, Any]" = {
            "name": port_name,
            "description": port_description,
            "delivery_profile": "newest",
            "family": dict(family_declaration),
        }
        if "default" in signature_input:
            input_port["pin_default"] = signature_input["default"]
        self.input_ports.append(input_port)

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

    def _pin_default(self, parameter: inspect.Parameter, pin_value_type: str) -> Any:
        default = parameter.default
        if default is None:
            raise TypeError(
                f"{self._function_name}'s input `{parameter.name}` defaults to `None` but is "
                f"not typed optional, so its pin would carry no value when nothing is wired. "
                f"To fix: `{parameter.name}: ... | None = None`, or a default of its type."
            )
        if pin_value_type in ("float", "int", "bool") and isinstance(default, (bool, int, float)):
            return default
        raise TypeError(
            f"{self._function_name}'s input `{parameter.name}` defaults to {default!r}, and "
            f"only a number or a bool pin takes a default. To fix: drop the default and "
            f"wire the input."
        )

    def _read_return(
        self, return_annotation: Any
    ) -> "tuple[list[dict[str, Any]], list[dict[str, Any]]]":
        returned_description, returned_without_it = self._port_description_and_annotation_without_it(
            return_annotation, "return annotation"
        )
        if returned_description is not None and (
            typing.get_origin(returned_without_it) is Stateful
            or _is_a_record_of_outputs(returned_without_it)
            or (self._reads_pin_types and _is_a_named_tuple(returned_without_it))
        ):
            raise TypeError(
                f"{self._function_name}'s return annotation carries a `Description` on what "
                f"is not one output. To fix: describe each output where it is typed, "
                f"`field: Annotated[T, Description(\"...\")]` on the record, or "
                f"`Stateful[Annotated[T, Description(\"...\")], Record]`."
            )
        if return_annotation is type(None):
            raise TypeError(
                f"{self._function_name} returns `None`, so it has no output and computes "
                f"nothing a graph can read. To fix: return the value it computes, or make it "
                f"a `@node.process` class if its work is a side effect."
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
        if self._reads_pin_types:
            return self._typed_output_ports(output_annotation)
        record_that_may_be_none, may_be_none = _value_annotation_and_whether_optional(output_annotation)
        if (
            may_be_none
            and isinstance(record_that_may_be_none, type)
            and dataclasses.is_dataclass(record_that_may_be_none)
            and record_that_may_be_none not in _families_by_read_type()
        ):
            raise TypeError(
                f"{self._function_name} returns `{_name_of(record_that_may_be_none)} | None`, and "
                f"a record of outputs hands out each of its fields, so it is never `None` as a "
                f"whole. To fix: return `{_name_of(record_that_may_be_none)}` and type each field "
                f"that may hand out nothing `T | None`."
            )
        if (
            isinstance(output_annotation, type)
            and dataclasses.is_dataclass(output_annotation)
            and output_annotation not in _families_by_read_type()
        ):
            field_annotations = typing.get_type_hints(output_annotation, include_extras=True)
            return self._output_ports_named(
                [(field.name, field_annotations[field.name]) for field in dataclasses.fields(output_annotation)],
                fields_of=output_annotation,
            )
        return self._output_ports_named([(_SINGLE_OUTPUT_NAME, output_annotation)], fields_of=None)

    def _output_ports_named(
        self, outputs: "list[tuple[str, Any]]", fields_of: Optional[type]
    ) -> "tuple[list[dict[str, Any]], list[dict[str, Any]]]":
        output_ports: "list[dict[str, Any]]" = []
        signature_outputs: "list[dict[str, Any]]" = []
        for output_name, described_output_annotation in outputs:
            port_name = self._cast_port_name(output_name)
            port_description, output_annotation = self._port_description_and_annotation_without_it(
                described_output_annotation, f"output `{output_name}`"
            )
            value_annotation, may_hand_out_nothing = _value_annotation_and_whether_optional(
                output_annotation
            )
            pin_value_type, family_declaration = self._pin_value_type_and_family(
                value_annotation, f"output `{output_name}`"
            )
            if pin_value_type == UNREAD_MESSAGE_VALUE_TYPE:
                family_declaration = {
                    "family": "image",
                    "family_follows_input_port": self._the_unread_message_input_an_output_follows(
                        output_name
                    ),
                }
            if family_declaration["family"] == "channels" and pin_value_type != "channels_block":
                family_declaration = {
                    "family": "channels",
                    "names": {"stated": [port_name]},
                    "dtype": "f64",
                }
            output_ports.append(
                {
                    "name": port_name,
                    "description": port_description or "",
                    "family": family_declaration,
                }
            )
            signature_outputs.append(
                {
                    "port": port_name,
                    "field": None if fields_of is None else output_name,
                    "value_type": pin_value_type,
                    "may_hand_out_nothing": may_hand_out_nothing,
                }
            )
        if fields_of is not None:
            signature_outputs_class = f"{fields_of.__module__}:{fields_of.__qualname__}"
            for signature_output in signature_outputs:
                signature_output["fields_of"] = signature_outputs_class
        return output_ports, signature_outputs

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
        if _is_a_record_of_outputs(value_annotation) and value_annotation not in PIN_VALUE_CLASSES:
            raise NodeDeclarationRefusal(
                f"`{self._function_name}`'s output `{output_name}` is typed "
                f"{_name_of(value_annotation)}, a dataclass, which the typed grammar does not "
                f"read as a bundle yet. To fix: return a `NamedTuple` for several outputs, or "
                f"keep this node on `@node.function` until it does.",
                finding_code="return_not_read_yet",
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

    def _the_unread_message_input_an_output_follows(self, output_name: str) -> str:
        if len(self._unread_message_input_ports) != 1:
            raise TypeError(
                f"{self._function_name}'s output `{output_name}` is an `UnreadMessage`, which "
                f"takes the family of the `UnreadMessage` input it hands on, and "
                f"{self._function_name} takes {len(self._unread_message_input_ports)} of them. "
                f"To fix: take exactly one `UnreadMessage` input."
            )
        return self._unread_message_input_ports[0]

    def _pin_value_type_and_family(
        self, annotation: Any, what_it_types: str
    ) -> "tuple[str, dict[str, Any]]":
        from ._node_context_protocols import UnreadMessage
        from ._port_family_declaration import PortFamily

        if typing.get_origin(annotation) is typing.Annotated:
            annotated, *metadata = typing.get_args(annotation)
            families_named = [each for each in metadata if isinstance(each, PortFamily)]
            if annotated is UnreadMessage and len(families_named) == 1:
                return UNREAD_MESSAGE_VALUE_TYPE, {
                    "family": families_named[0].declaration_name.lower()
                }
            if families_named:
                raise TypeError(
                    f"{self._function_name}'s {what_it_types} names a port family on "
                    f"{_name_of(annotated)}, and only an `UnreadMessage` takes one. To fix: "
                    f"`Annotated[UnreadMessage, {families_named[0]!r}]`."
                )
            return self._pin_value_type_and_family(annotated, what_it_types)
        if annotation is UnreadMessage:
            return UNREAD_MESSAGE_VALUE_TYPE, dict(UNREAD_MESSAGE_INPUT_FAMILY)
        if annotation in _SCALAR_PIN_VALUE_TYPES:
            return _SCALAR_PIN_VALUE_TYPES[annotation], {"family": "channels"}
        if annotation is dict or typing.get_origin(annotation) is dict:
            return "records", {"family": "records"}
        families_by_read_type = _families_by_read_type()
        if annotation in families_by_read_type:
            pin_value_type, family = families_by_read_type[annotation]
            return pin_value_type, {"family": family}
        raise TypeError(
            f"{self._function_name}'s {what_it_types} is typed {annotation!r}, which names no "
            f"port family. To fix: type it as `float`, `int` or `bool` (one channel), `dict` "
            f"(Records), a read type: `ImageFrame`, `ArrayFrame`, `ChannelsBlock`, "
            f"`PacketsUnit`, or `UnreadMessage` for a message it hands on unread."
        )

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

    An input that takes more than one family reads every family, as an `UnreadMessage` does.
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


def _families_by_read_type() -> "dict[Any, tuple[str, str]]":
    """Each port family's read type, with the pin value type and family wire name it is."""
    from ._node_context_protocols import ChannelsBlock, PacketsUnit
    from .array_frame import ArrayFrame
    from .image_frame import ImageFrame

    return {
        ImageFrame: ("image_frame", "image"),
        ArrayFrame: ("array_frame", "array"),
        ChannelsBlock: ("channels_block", "channels"),
        PacketsUnit: ("packets_unit", "packets"),
    }


def _names_a_port_family(annotation: Any) -> bool:
    """Whether a parameter's annotation types it by a port family, also as `T | None`.

    A family's read type, such as `ImageFrame`, an `UnreadMessage` or a `dict` of Records.
    """
    from ._node_context_protocols import UnreadMessage

    value_annotation, _ = _value_annotation_and_whether_optional(annotation)
    if typing.get_origin(value_annotation) is typing.Annotated:
        value_annotation = typing.get_args(value_annotation)[0]
    return (
        value_annotation in _families_by_read_type()
        or value_annotation is UnreadMessage
        or value_annotation is dict
        or typing.get_origin(value_annotation) is dict
    )


def _is_a_record_of_outputs(annotation: Any) -> bool:
    return (
        isinstance(annotation, type)
        and dataclasses.is_dataclass(annotation)
        and annotation not in _families_by_read_type()
        and named_pin_type_declared_by(annotation) is None
    )


def _value_annotation_and_whether_optional(annotation: Any) -> "tuple[Any, bool]":
    arguments = typing.get_args(annotation)
    if type(None) in arguments and len(arguments) == 2:
        (value_annotation,) = [each for each in arguments if each is not type(None)]
        return value_annotation, True
    return annotation, False


def _name_of(annotation: Any) -> str:
    return getattr(annotation, "__name__", repr(annotation))


def _upper_camel_case(snake_case_name: str) -> str:
    return "".join(word.capitalize() for word in snake_case_name.split("_"))
