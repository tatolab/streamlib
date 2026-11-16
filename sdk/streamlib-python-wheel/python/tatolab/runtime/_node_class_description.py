# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


from __future__ import annotations

import json
import traceback
from typing import Any

from tatolab.stream._function_node_declaration import (
    NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP,
    NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP,
    NODE_DECLARATION_LANGUAGE_STAMP,
    FunctionNode,
)
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_DECLARED_STAMP,
    NODE_DECLARATION_CONFIG_SCHEMA_STAMP,
    NODE_DECLARATION_DESCRIPTION_STAMP,
    NODE_DECLARATION_EXECUTION_STAMP,
    NODE_DECLARATION_KIND_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
    NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP,
    NODE_DECLARATION_SCHEDULING_PRIORITY_STAMP,
)

from tatolab.runtime._function_node_compilation import (
    FunctionNodeDoesNotCompile,
    FunctionNodeRunAsPythonInItsReader,
    prepare_function_node_to_evaluate,
)
from tatolab.runtime._node_type_import_folder import (
    NodeTypeModuleImportedFromTwoPlaces,
    import_folder_and_class_import_path,
    import_the_module_of_a_node_type,
)
from tatolab.runtime._helper_process_environment_variables import ENTRYPOINT_ENV
from tatolab.runtime._parent_process_bridge import HelperProcessProtocolError


def load_node_class(import_path: str) -> type:
    """Import the class `import_path` names, as `module:qualname`, led by an import's folder
    when the class is in that import's own node code.

    The qualname is walked attribute by attribute, so a class nested inside
    another resolves — `stream_builder.add` deliberately admits `Outer.Inner`, because
    a fresh interpreter can reach it.
    """
    import_folder, class_import_path = import_folder_and_class_import_path(import_path)
    module_name, _, qualname = class_import_path.partition(":")
    if not module_name or not qualname:
        raise HelperProcessProtocolError(
            f"{ENTRYPOINT_ENV} must be `module:qualname`, got {import_path!r}"
        )
    try:
        resolved: Any = import_the_module_of_a_node_type(module_name, import_folder)
    except NodeTypeModuleImportedFromTwoPlaces as imported_from_two_places:
        raise HelperProcessProtocolError(str(imported_from_two_places)) from None
    walked = module_name
    for attribute_name in qualname.split("."):
        try:
            resolved = getattr(resolved, attribute_name)
        except AttributeError as missing:
            raise HelperProcessProtocolError(
                f"{import_path!r} does not resolve: {walked} has no attribute "
                f"{attribute_name!r}"
            ) from missing
        walked = f"{walked}.{attribute_name}"
    return resolved



#: Each key of a described process node type the class's own `@node.process` stamp fills, in wire order.
DESCRIBED_NODE_TYPE_KEYS_AND_THEIR_NODE_STAMPS = (
    ("description", NODE_DECLARATION_DESCRIPTION_STAMP),
    ("kind", NODE_DECLARATION_KIND_STAMP),
    ("execution", NODE_DECLARATION_EXECUTION_STAMP),
    ("scheduling_priority", NODE_DECLARATION_SCHEDULING_PRIORITY_STAMP),
    ("paced_by_real_time", NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP),
    ("config_schema", NODE_DECLARATION_CONFIG_SCHEMA_STAMP),
    ("input_ports", NODE_DECLARATION_INPUT_PORTS_STAMP),
    ("output_ports", NODE_DECLARATION_OUTPUT_PORTS_STAMP),
)


#: Each key of a described function node type its `@node.function` stamp fills, in wire order.
DESCRIBED_FUNCTION_NODE_TYPE_KEYS_AND_THEIR_NODE_STAMPS = (
    ("description", NODE_DECLARATION_DESCRIPTION_STAMP),
    ("kind", NODE_DECLARATION_KIND_STAMP),
    ("language", NODE_DECLARATION_LANGUAGE_STAMP),
    ("config_schema", NODE_DECLARATION_CONFIG_SCHEMA_STAMP),
    ("input_ports", NODE_DECLARATION_INPUT_PORTS_STAMP),
    ("output_ports", NODE_DECLARATION_OUTPUT_PORTS_STAMP),
    ("function_signature", NODE_DECLARATION_FUNCTION_SIGNATURE_STAMP),
    ("state_fields", NODE_DECLARATION_FUNCTION_STATE_FIELDS_STAMP),
)


#: Each key of a described shader node type its `@node.shader` stamp fills, in wire order.
#: The shader stamps are spelled here, not imported, because the `tatolab-stream` in a
#: project's venv may predate `@node.shader`, and their attribute names are the wire
#: contract both sides hold.
DESCRIBED_SHADER_NODE_TYPE_KEYS_AND_THEIR_NODE_STAMPS = (
    ("description", NODE_DECLARATION_DESCRIPTION_STAMP),
    ("kind", NODE_DECLARATION_KIND_STAMP),
    ("config_schema", NODE_DECLARATION_CONFIG_SCHEMA_STAMP),
    ("input_ports", NODE_DECLARATION_INPUT_PORTS_STAMP),
    ("output_ports", NODE_DECLARATION_OUTPUT_PORTS_STAMP),
    ("shader_source", "__tatolab_node_shader_source__"),
    ("shader_signature", "__tatolab_node_shader_signature__"),
)

#: The kind stamp a `@node.shader` carries.
SHADER_NODE_KIND = "shader"


#: Each key of a described node type, process or function, filled from a stamp an older
#: `tatolab-stream` does not write, and left out when the node carries none: the engine reads an
#: absent key as undeclared. Spelled here, not imported, because the `tatolab-stream` in a
#: project's venv may predate the stamp, and its attribute name is the wire contract both sides hold.
DESCRIBED_NODE_TYPE_KEYS_AND_THE_STAMPS_AN_OLDER_TATOLAB_STREAM_DOES_NOT_WRITE = (
    ("category", "__tatolab_node_category__"),
)


class NodeClassNotDescribable(Exception):
    """A requested import path names nothing this interpreter can describe."""


def describe_one_node_class(import_path: str) -> "dict[str, Any]":
    import_folder, class_import_path = import_folder_and_class_import_path(import_path)
    module_name, _, qualname = class_import_path.partition(":")
    if not module_name or not qualname:
        raise NodeClassNotDescribable("an import path is `module:qualname`")
    try:
        import_the_module_of_a_node_type(module_name, import_folder)
    except NodeTypeModuleImportedFromTwoPlaces as imported_from_two_places:
        raise NodeClassNotDescribable(str(imported_from_two_places)) from None
    except (Exception, SystemExit):
        raise NodeClassNotDescribable(
            f"its module `{module_name}` did not import:\n{traceback.format_exc()}"
        ) from None
    try:
        return _describe_the_node_class_its_imported_module_defines(import_path)
    except NodeClassNotDescribable:
        raise
    except (Exception, SystemExit):
        raise NodeClassNotDescribable(
            f"reading its declaration raised:\n{traceback.format_exc()}"
        ) from None


def _describe_the_node_class_its_imported_module_defines(
    import_path: str,
) -> "dict[str, Any]":
    try:
        resolved: Any = load_node_class(import_path)
    except HelperProcessProtocolError as unresolvable:
        raise NodeClassNotDescribable(str(unresolvable)) from None
    if (
        isinstance(resolved, FunctionNode)
        and getattr(resolved, NODE_DECLARATION_KIND_STAMP, None) == SHADER_NODE_KIND
    ):
        described_keys_and_their_stamps = DESCRIBED_SHADER_NODE_TYPE_KEYS_AND_THEIR_NODE_STAMPS
        the_decorator_that_stamps_them = "@node.shader"
        what_it_names = "function"
    elif isinstance(resolved, FunctionNode):
        described_keys_and_their_stamps = DESCRIBED_FUNCTION_NODE_TYPE_KEYS_AND_THEIR_NODE_STAMPS
        the_decorator_that_stamps_them = "@node.function"
        what_it_names = "function"
    elif isinstance(resolved, type):
        described_keys_and_their_stamps = DESCRIBED_NODE_TYPE_KEYS_AND_THEIR_NODE_STAMPS
        the_decorator_that_stamps_them = "@node.process"
        what_it_names = "class"
    else:
        raise NodeClassNotDescribable(
            f"it names a {type(resolved).__name__}, not a class or a `@node.function`"
        )
    if getattr(resolved, NODE_DECLARATION_DECLARED_STAMP, False) is not True:
        raise NodeClassNotDescribable(
            f"the {what_it_names} `{resolved.__qualname__}` carries no "
            f"`{the_decorator_that_stamps_them}` declaration, so it is not a node"
        )
    own_import_path = (
        f"{getattr(resolved, '__module__', None)}:{getattr(resolved, '__qualname__', None)}"
    )
    if own_import_path != import_folder_and_class_import_path(import_path)[1]:
        raise NodeClassNotDescribable(
            f"the {what_it_names} it names identifies as `{own_import_path}`; a node is named by "
            f"its own import path, the one its node interpreter imports it back by"
        )
    described_node_type: "dict[str, Any]" = {
        "import_path": import_path,
        "short_name": resolved.__name__,
    }
    for (
        described_node_type_key,
        node_declaration_stamp,
    ) in described_keys_and_their_stamps:
        try:
            described_node_type[described_node_type_key] = getattr(
                resolved, node_declaration_stamp
            )
        except AttributeError:
            raise NodeClassNotDescribable(
                f"the {what_it_names} `{resolved.__qualname__}` carries no "
                f"`{node_declaration_stamp}`, which `{the_decorator_that_stamps_them}` sets; a "
                f"{what_it_names} stamped by hand is not a node"
            ) from None
    for (
        described_node_type_key,
        node_declaration_stamp,
    ) in DESCRIBED_NODE_TYPE_KEYS_AND_THE_STAMPS_AN_OLDER_TATOLAB_STREAM_DOES_NOT_WRITE:
        if hasattr(resolved, node_declaration_stamp):
            described_node_type[described_node_type_key] = getattr(
                resolved, node_declaration_stamp
            )
    if described_node_type.get("language") == "python":
        try:
            prepared = prepare_function_node_to_evaluate(resolved, import_path)
        except FunctionNodeDoesNotCompile as refused:
            raise NodeClassNotDescribable(str(refused)) from None
        described_node_type["runs_as_python"] = isinstance(
            prepared, FunctionNodeRunAsPythonInItsReader
        )
    try:
        json.dumps(described_node_type, allow_nan=False)
    except (TypeError, ValueError) as not_json:
        raise NodeClassNotDescribable(
            f"its declaration is not JSON data: {not_json}"
        ) from None
    return described_node_type
