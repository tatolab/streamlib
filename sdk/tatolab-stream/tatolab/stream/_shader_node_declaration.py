# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@node.shader` grammar: a Slang function from Textures and values to a Texture.

The decorated function's signature declares the ports and its body is `...`;
the Slang source passed to the decorator is the node's body, a function named
for the node. A `Texture` parameter arrives in Slang as an `ImageInput`
and each value under the typed grammar: a `float` as a `float`, a `Color` as a
`float4` of red, green, blue and alpha, and a `Vector2`, `Vector3` or `Vector4`
as a `float2`, `float3` or `float4` of its parts. The return is one
`Texture`, the output `result`, in the one pixel format the runtime writes.
Decorating registers nothing: it reads the signature once and stamps the
declaration the runtime reads, with the Slang source the runtime compiles.
No Python runs when the node is evaluated.
"""

from __future__ import annotations

import inspect
import re
from collections.abc import Callable
from typing import Any, Final, NoReturn, ParamSpec, TypeVar

from ._function_node_declaration import (
    SHADER_IMAGE_VALUE_TYPE,
    FunctionNode,
    _FunctionNodeSignatureReading,
)
from ._node_meta_declaration import (
    take_node_meta_held_for_the_kind_decorator,
    the_conversion_a_node_states,
)

__all__ = ["ShaderNode"]

_ShaderNodeParameters = ParamSpec("_ShaderNodeParameters")
_ShaderNodeReturn = TypeVar("_ShaderNodeReturn", covariant=True)

SHADER_NODE_KIND: Final = "shader"

# Wire contract with the runtime's reader, stamped beside the declaration
# stamps every node carries.
NODE_DECLARATION_SHADER_SOURCE_STAMP = "__tatolab_node_shader_source__"
NODE_DECLARATION_SHADER_SIGNATURE_STAMP = "__tatolab_node_shader_signature__"


# The value types a shader takes beside its Images, each read in Slang as the type named.
_SLANG_TYPE_OF_EACH_VALUE_TYPE: "dict[str, str]" = {
    "float": "float",
    "color": "float4",
    "vector2": "float2",
    "vector3": "float3",
    "vector4": "float4",
}


class ShaderNode(FunctionNode[_ShaderNodeParameters, _ShaderNodeReturn]):
    """A `@node.shader` function: added to a stream as a node the runtime evaluates on the GPU."""

    def __call__(  # type: ignore[override]
        self, *_arguments: object, **_keyword_arguments: object
    ) -> NoReturn:
        raise TypeError(
            f"{self.__name__} is a `@node.shader`: its body is Slang the runtime compiles and "
            f"runs on the GPU, so it is never called in Python. Add it to a stream instead."
        )

    def __repr__(self) -> str:
        return f"<shader node {self.__module__}:{self.__qualname__}>"


def declare_shader_node(
    function: Callable[_ShaderNodeParameters, _ShaderNodeReturn],
    *,
    shader_source: str,
    declaration_stamps: dict[str, str],
) -> ShaderNode[_ShaderNodeParameters, _ShaderNodeReturn]:
    """Read `function`'s signature into a shader node over `shader_source`, refusing by name what is not one."""
    _refuse_what_is_not_a_plain_module_level_function(function)
    declared_meta = take_node_meta_held_for_the_kind_decorator(function)
    name = function.__name__
    signature_reading = _FunctionNodeSignatureReading(function, reads_shader_images=True)
    signature = signature_reading.function_signature
    _refuse_a_signature_a_shader_cannot_take(name, signature)
    _refuse_a_source_that_defines_no_function_named(name, shader_source)
    shader_node = ShaderNode(function)
    stamps: dict[str, Any] = {
        declaration_stamps["declared"]: True,
        declaration_stamps["kind"]: SHADER_NODE_KIND,
        declaration_stamps["description"]: declared_meta.get("description", ""),
        declaration_stamps["category"]: declared_meta.get("category"),
        declaration_stamps["conversion"]: the_conversion_a_node_states(
            name,
            declared_meta,
            declared_by="@node.shader",
            kind=SHADER_NODE_KIND,
            input_ports=signature_reading.input_ports,
            output_ports=signature_reading.output_ports,
        ),
        declaration_stamps["declared_by"]: "@node.shader",
        declaration_stamps["input_ports"]: signature_reading.input_ports,
        declaration_stamps["output_ports"]: signature_reading.output_ports,
        NODE_DECLARATION_SHADER_SOURCE_STAMP: shader_source,
        NODE_DECLARATION_SHADER_SIGNATURE_STAMP: {
            "function_name": name,
            "inputs": [_shader_signature_input(each) for each in signature["inputs"]],
            "output_port": signature["outputs"][0]["port"],
        },
    }
    for stamp, value in stamps.items():
        setattr(shader_node, stamp, value)
    return shader_node


def _shader_signature_input(signature_input: dict[str, Any]) -> dict[str, Any]:
    """One input as the runtime's shader reader takes it: its port and value type, and a value's pin type."""
    shader_input = {"port": signature_input["port"], "value_type": signature_input["value_type"]}
    if "pin_type" in signature_input:
        shader_input["pin_type"] = signature_input["pin_type"]
    return shader_input


def _refuse_what_is_not_a_plain_module_level_function(function: object) -> None:
    if isinstance(function, type) or not inspect.isfunction(function):
        raise TypeError(
            f"@node.shader takes a function defined with `def` whose signature declares the "
            f"ports, got {function!r}."
        )
    if function.__name__ == "<lambda>" or "<locals>" in function.__qualname__:
        raise TypeError(
            f"{function.__name__} is not defined with `def` at module scope, so its describe "
            f"cannot import it. To fix: write it as a module-level `def`."
        )


def _refuse_a_signature_a_shader_cannot_take(name: str, signature: dict[str, Any]) -> None:
    if signature["time_parameter"] is not None or signature["state_parameter"] is not None:
        raise TypeError(
            f"{name} takes `Time` or `State[...]`, which a shader is never handed: it computes "
            f"each pixel from its inputs alone. To fix: take the value as a `float` input."
        )
    for signature_input in signature["inputs"]:
        if signature_input["optional"]:
            raise TypeError(
                f"{name}'s input `{signature_input['parameter']}` is optional, and every input "
                f"of a shader is read on each frame. To fix: drop `| None`."
            )
        value_type = signature_input["value_type"]
        if value_type != SHADER_IMAGE_VALUE_TYPE and value_type not in _SLANG_TYPE_OF_EACH_VALUE_TYPE:
            raise TypeError(
                f"{name}'s input `{signature_input['parameter']}` is a {value_type}, and a "
                f"shader takes `Texture` inputs and `float`, `Color`, `Vector2`, `Vector3` "
                f"and `Vector4` values only. To fix: type it as one of them."
            )
    if not any(each["value_type"] == SHADER_IMAGE_VALUE_TYPE for each in signature["inputs"]):
        raise TypeError(
            f"{name} takes no `Texture`, so nothing sets the extent of the Texture it "
            f"returns. To fix: take at least one `Texture` input."
        )
    outputs = signature["outputs"]
    if (
        len(outputs) != 1
        or outputs[0]["value_type"] != SHADER_IMAGE_VALUE_TYPE
        or outputs[0]["may_hand_out_nothing"]
    ):
        raise TypeError(
            f"{name} must return one `Texture`, the picture its Slang function computes pixel "
            f"by pixel. To fix: `-> Texture`."
        )


def _refuse_a_source_that_defines_no_function_named(name: str, shader_source: str) -> None:
    if not isinstance(shader_source, str) or not shader_source.strip():
        raise TypeError(
            f"@node.shader on {name} takes the node's Slang source as its one argument: "
            f'`@node.shader("""float4 {name}(ImageInput image) {{ ... }}""")`.'
        )
    if re.search(rf"\bfloat4\s+{re.escape(name)}\s*\(", shader_source) is None:
        raise ValueError(
            f"the Slang source of {name} defines no `float4 {name}(...)`; the source's "
            f"function is named for the node and returns the pixel it computes."
        )
