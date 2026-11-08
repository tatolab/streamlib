# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@node.function` grammar: a function whose signature is the node.

A plain parameter is an input pin and its default the pin's value; the return
annotation is the output `result`, or one output per field of a dataclass. An
output typed `T | None` hands out nothing on a call that returns `None`. An
`UnreadMessage` input takes a message of any family, which the body can only
hand on, to an `UnreadMessage` output of the family linked into that input.
`config` names the node's one config dataclass. `Time` and `State[...]` are
found by type and are never pins. Decorating registers nothing: it reads the
signature once and stamps the declaration the runtime reads, the same stamps a
`@node.process` class carries plus the signature the runtime calls the body
with.
"""

from __future__ import annotations

import dataclasses
import functools
import inspect
import typing
from typing import Any, Callable, Generic, Optional, ParamSpec, TypeVar

from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe
from ._node_config_schema import (
    derive_config_class_json_schema,
    json_schema_for_a_node_declaring_no_config,
)

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
_UNREAD_MESSAGE_INPUT_FAMILY = {
    "family": "image",
    "also_takes_families": ["array", "channels", "packets", "records"],
}

_FUNCTION_LOCAL_MARKER = "<locals>"


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
    description: str,
    category: str,
    declaration_stamps: "dict[str, str]",
) -> FunctionNode[_FunctionNodeParameters, _FunctionNodeReturn]:
    """Read `function`'s signature into a function node, refusing by name what is not one.

    `declaration_stamps` names the stamps every node carries, as
    `_node_declaration` spells them, so both kinds stamp one set.
    """
    _refuse_what_is_not_a_plain_module_level_function(function)
    signature_reading = _FunctionNodeSignatureReading(function)
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
            else derive_config_class_json_schema(config_class)
        ),
        declaration_stamps["description"]: description or inspect.getdoc(function) or "",
        declaration_stamps["category"]: category or function.__module__,
        declaration_stamps["input_ports"]: signature_reading.input_ports,
        declaration_stamps["output_ports"]: signature_reading.output_ports,
        NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP: signature_reading.function_signature,
        NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP: signature_reading.state_fields,
    }
    for stamp, value in stamps.items():
        setattr(function_node, stamp, value)
    return function_node


def _refuse_what_is_not_a_plain_module_level_function(function: object) -> None:
    if isinstance(function, type):
        raise TypeError(
            f"@node.function takes a function, and {function.__name__} is a class. A class "
            f"with `process()` and ports is `@node.process`."
        )
    if not inspect.isfunction(function):
        raise TypeError(
            f"@node.function takes a function defined with `def`, got {function!r}."
        )
    name = function.__name__
    if name == "<lambda>":
        raise TypeError(
            "@node.function takes a function defined with `def` at module scope, and a lambda "
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

    def __init__(self, function: Callable[..., Any]) -> None:
        self._function_name = function.__name__
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

        for parameter in inspect.signature(function).parameters.values():
            self._read_parameter(parameter, annotations)

        if "return" not in annotations:
            raise TypeError(
                f"{self._function_name} has no return annotation, so nothing names its "
                f"output. To fix: annotate the return, `-> float`, or a dataclass for "
                f"several outputs."
            )
        self.output_ports, self._signature_outputs = self._read_return(annotations["return"])

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
            return typing.get_type_hints(function)
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
            raise TypeError(
                f"{self._function_name}'s parameter `{parameter.name}` has no type, so nothing "
                f"names the pin's family. To fix: annotate it, `{parameter.name}: float`."
            )
        annotation = annotations[parameter.name]
        if annotation is Time:
            self._read_time_parameter(parameter)
        elif typing.get_origin(annotation) is State:
            self._read_state_parameter(parameter, annotation)
        elif parameter.name == "config":
            self._read_config_parameter(annotation)
        else:
            self._read_input_pin(parameter, annotation)

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

    def _read_input_pin(self, parameter: inspect.Parameter, annotation: Any) -> None:
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
            "description": "",
            "delivery_profile": "newest",
            "family": dict(family_declaration),
        }
        if "default" in signature_input:
            input_port["pin_default"] = signature_input["default"]
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
        if (
            isinstance(output_annotation, type)
            and dataclasses.is_dataclass(output_annotation)
            and output_annotation not in _families_by_read_type()
        ):
            field_annotations = typing.get_type_hints(output_annotation)
            return self._output_ports_named(
                [(field.name, field_annotations[field.name]) for field in dataclasses.fields(output_annotation)],
                fields_of=output_annotation,
            )
        return self._output_ports_named([("result", output_annotation)], fields_of=None)

    def _output_ports_named(
        self, outputs: "list[tuple[str, Any]]", fields_of: Optional[type]
    ) -> "tuple[list[dict[str, Any]], list[dict[str, Any]]]":
        output_ports: "list[dict[str, Any]]" = []
        signature_outputs: "list[dict[str, Any]]" = []
        for output_name, output_annotation in outputs:
            port_name = self._cast_port_name(output_name)
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
                {"name": port_name, "description": "", "family": family_declaration}
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

        if annotation is UnreadMessage:
            return UNREAD_MESSAGE_VALUE_TYPE, dict(_UNREAD_MESSAGE_INPUT_FAMILY)
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
