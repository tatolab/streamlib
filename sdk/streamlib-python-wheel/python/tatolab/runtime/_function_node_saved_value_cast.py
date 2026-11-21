# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A saved value cast by a function node conversion, its body run once out of stream.

When a Graph Input is retyped, the runtime casts its saved value through the one conversion
between the two types. A module's `@node` or `@node.function` conversion computes its output
from its one input alone, so its body is run once here, in the stream's own interpreter, its
input holding the saved value, and its one output, in the stored JSON form a pin of its type
holds, is the cast.

The cast runs the conversion's Python body, called the way `FunctionNodeBodyRunAsPython`
calls it, even where the stream runs that body compiled with Numba; and it runs with the
conversion's default config, since no node of it is placed yet. A body that takes the stream's
time or keeps state gives no one value out of stream, and is refused saying so.
"""

from __future__ import annotations

from typing import Any

from tatolab.stream._function_node_declaration import FunctionNode
from tatolab.stream._rust_function_node_declaration import RustFunctionNode

from tatolab.runtime._function_node_body_run_as_python import FunctionNodeBodyRunAsPython
from tatolab.runtime._function_node_carried_values import (
    PART_VALUE_TYPES,
    FunctionNodeDoesNotCompile,
    pin_value_carried,
    value_class_of_a_part_value_type,
)
from tatolab.runtime._function_node_preparation import function_node_run_as_python_in_its_reader
from tatolab.runtime._function_node_stored_values import STORED_VALUE_RECORD_KEY, STORED_VALUE_TYPES

__all__ = [
    "SAVED_VALUE_CAST_REQUEST_INPUT_KEY",
    "SAVED_VALUE_CAST_REQUEST_VALUE_KEY",
    "SAVED_VALUE_CAST_DOCUMENT_CAST_KEY",
    "SAVED_VALUE_CAST_DOCUMENT_NOT_CAST_BECAUSE_KEY",
    "SAVED_VALUE_CAST_DOCUMENT_NOT_RUN_BECAUSE_KEY",
    "SavedValueNotCast",
    "SavedValueCastDidNotRun",
    "the_saved_value_a_function_node_casts",
]

# The keys of the request the engine passes a cast and of the document the cast prints back;
# the engine's `SavedValueCastRequest` and `SavedValueCastDocument` name the same fields.
SAVED_VALUE_CAST_REQUEST_INPUT_KEY = "input"
SAVED_VALUE_CAST_REQUEST_VALUE_KEY = "value"
SAVED_VALUE_CAST_DOCUMENT_CAST_KEY = "cast"
SAVED_VALUE_CAST_DOCUMENT_NOT_CAST_BECAUSE_KEY = "not_cast_because"
SAVED_VALUE_CAST_DOCUMENT_NOT_RUN_BECAUSE_KEY = "not_run_because"

# The stream time a body is called at out of stream; never read, since a body that takes the
# stream's time is refused.
_NO_STREAM_TIME = 0


class SavedValueNotCast(Exception):
    """Why a function node conversion's body, run out of stream, gives no cast of a saved
    value: its input does not take it, or its body raised or gave no value a Graph Input
    saves."""


class SavedValueCastDidNotRun(SavedValueNotCast):
    """Why a function node conversion's body cannot run out of stream at all, whatever the
    value."""


def the_saved_value_a_function_node_casts(
    function_node: Any, import_path: str, input_port: str, saved_value: Any
) -> Any:
    """`saved_value` cast by `function_node`, the conversion `import_path` names, whose one
    input is `input_port`: its body called once with the value on that input, its output in
    stored JSON form.

    Refused as `SavedValueCastDidNotRun`, saying why, when the conversion is no Python function
    node, takes the stream's time or keeps state; as `SavedValueNotCast` when the value is not
    one its input takes, or its body raises or gives no value a Graph Input saves.
    """
    if isinstance(function_node, RustFunctionNode):
        raise SavedValueCastDidNotRun(
            f"`{import_path}`'s body is Rust, which runs only through its evaluate entry inside "
            f"a started stream"
        )
    if not isinstance(function_node, FunctionNode):
        raise SavedValueCastDidNotRun(
            f"`{import_path}` is no function node, so its body runs only inside a started stream"
        )
    try:
        run_as_python = function_node_run_as_python_in_its_reader(function_node, import_path, "")
    except FunctionNodeDoesNotCompile as not_run:
        raise SavedValueCastDidNotRun(str(not_run)) from not_run
    signature = run_as_python.function_signature
    if signature["time_parameter"] is not None:
        raise SavedValueCastDidNotRun(
            f"`{import_path}` takes the stream's time as `{signature['time_parameter']}`, which "
            f"no stream gives out of stream"
        )
    if run_as_python.is_stateful:
        raise SavedValueCastDidNotRun(
            f"`{import_path}` keeps state between calls, so one call out of stream does not give "
            f"what it gives in a stream"
        )
    [the_input] = signature["inputs"]
    if the_input["port"] != input_port:
        raise SavedValueCastDidNotRun(f"`{import_path}` has no input `{input_port}`")
    try:
        carried = pin_value_carried(
            the_input["value_type"],
            saved_value,
            f"`{import_path}`'s input `{input_port}`",
            the_input.get("pin_type"),
        )
    except (FunctionNodeDoesNotCompile, TypeError, ValueError) as not_taken:
        raise SavedValueNotCast(
            f"`{import_path}`'s input `{input_port}` does not take {saved_value!r}: {not_taken}"
        ) from not_taken
    [the_output] = signature["outputs"]
    body = FunctionNodeBodyRunAsPython(run_as_python, import_path, None)
    try:
        [answered] = body.evaluate([carried], _NO_STREAM_TIME)
    except Exception as raised:
        raise SavedValueNotCast(
            f"`{import_path}`'s body raised {type(raised).__name__}: {raised}"
        ) from raised
    return _the_stored_value_of_an_output(the_output, answered, import_path)


def _the_stored_value_of_an_output(
    the_output: "dict[str, Any]", answered: Any, import_path: str
) -> Any:
    """`answered`, what the body gave on `the_output` as its island carries it, in stored JSON
    form."""
    value_type = the_output["value_type"]
    what_it_is = f"`{import_path}`'s output `{the_output['port']}`"
    if answered is None:
        raise SavedValueNotCast(f"{what_it_is} handed out nothing")
    if value_type in STORED_VALUE_TYPES:
        return answered[STORED_VALUE_RECORD_KEY]
    if value_type in PART_VALUE_TYPES:
        return value_class_of_a_part_value_type(value_type).from_parts(answered).stored_value()
    if value_type == "int":
        return int(answered)
    if value_type == "bool":
        return answered != 0.0
    if value_type == "float":
        return float(answered)
    raise SavedValueNotCast(f"{what_it_is} gives a {value_type} value, which no Graph Input saves")
