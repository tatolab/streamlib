# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""Read the node types and graph generators a module declares from its source, never running it.

A module is parsed with `ast` and its decorators are matched against the names it imports
from `tatolab.stream`: `@node.process` and bare `@node` classes, `@node.function`, bare `@node` and
`@node.shader` functions, `X = node.rust_function(...)` assignments and `@graph_generator` functions, each
node's `@node.meta` beside its kind decorator, and `@named_pin_type` classes, each a pin type
of the module named by its import path. A
port is listed where the declaration states it in source; one whose family is computed at
import is listed with no family, and a node whose ports only its native half knows is listed
with none. A record a function node returns is read where it is defined: in its own module or
in another of the same source that the module imports it from, so a `NamedTuple` imported from
another module lists one output per field as the loaded node does. The answer is a JSON-ready
dict the runtime's node-type listing passes on.
"""

from __future__ import annotations

import ast
import inspect
import json
from collections.abc import Iterable
from pathlib import PurePosixPath
from typing import Any, Optional

from ._exposed_name_cast import ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe
from ._node_type_source_records import (
    NAMED_TUPLE_BASE,
    FieldOfARecord,
    ModulesOfOneSource,
    RecordOfOutputs,
    imported_modules_by_local_name,
    imported_names_by_local_name,
    named_tuples_called_by_name,
)
from ._retired_declarations import RETIRED_DECLARATIONS, RetiredDeclaration

__all__ = [
    "module_name_of_a_source_path",
    "read_node_types_and_graph_generators_from_module_sources",
    "read_retired_declarations_from_module_source",
]

SDK_MODULE_NAME = "tatolab.stream"

TYPED_NODE_DECORATOR = f"{SDK_MODULE_NAME}.node"
PROCESS_NODE_DECORATOR = f"{SDK_MODULE_NAME}.node.process"
FUNCTION_NODE_DECORATOR = f"{SDK_MODULE_NAME}.node.function"
SHADER_NODE_DECORATOR = f"{SDK_MODULE_NAME}.node.shader"
NODE_META_DECORATOR = f"{SDK_MODULE_NAME}.node.meta"
RUST_FUNCTION_NODE_DECLARATION = f"{SDK_MODULE_NAME}.node.rust_function"
GRAPH_GENERATOR_DECORATOR = f"{SDK_MODULE_NAME}.graph_generator"
NAMED_PIN_TYPE_DECORATOR = f"{SDK_MODULE_NAME}.named_pin_type"
DESCRIPTION_DECLARATION = f"{SDK_MODULE_NAME}.Description"
INPUT_PORT_DECLARATION = f"{SDK_MODULE_NAME}.Input"
OUTPUT_PORT_DECLARATION = f"{SDK_MODULE_NAME}.Output"

# The text a module that declares a node or a generator must hold: it imports the SDK.
SDK_IMPORT_MARKER = "tatolab"

# Each port family by the name a declaration spells it, as the wire names it.
_PORT_FAMILY_BY_DECLARATION_NAME = {
    "Image": "image",
    "Array": "array",
    "Channels": "channels",
    "Packets": "packets",
    "Records": "records",
}

# Each family a function or shader port is, by the type its annotation names.
_PORT_FAMILY_BY_ANNOTATED_TYPE = {
    "float": "channels",
    "int": "channels",
    "bool": "channels",
    "Color": "channels",
    "Vector2": "channels",
    "Vector3": "channels",
    "Vector4": "channels",
    "dict": "records",
    "ImageFrame": "image",
    "ArrayFrame": "array",
    "ChannelsBlock": "channels",
    "PacketsUnit": "packets",
    "Texture": "image",
    "Audio": "channels",
    "Tensor": "array",
    "EncodedVideo": "packets",
    "EncodedAudio": "packets",
    "Trigger": "records",
    "str": "records",
    "Asset": "records",
    "Literal": "records",
    "Color": "channels",
    "Vector2": "channels",
    "Vector3": "channels",
    "Vector4": "channels",
    "Rectangle": "channels",
    "Transform": "channels",
}

_NODE_META_KEYS = ("category", "description")

_RETIRED_DECLARATIONS_BY_DECORATOR: dict[str, RetiredDeclaration] = {
    retired.decorator: retired for retired in RETIRED_DECLARATIONS
}

# Annotations a function node's signature reads that are never pins.
_FUNCTION_NODE_NON_PIN_ANNOTATIONS = ("Time", "State")
_FUNCTION_NODE_CONFIG_PARAMETER = "config"
_STATEFUL_RETURN = "Stateful"
_OPTIONAL_ANNOTATION = "Optional"
_INPUT_PIN_ANNOTATION = "InputPin"
_OUTPUT_PIN_ANNOTATION = "OutputPin"
_CLASS_VARIABLE_ANNOTATION = "ClassVar"
_ANNOTATED_ANNOTATION = "Annotated"
_UNREAD_MESSAGE_ANNOTATION = "UnreadMessage"
_SINGLE_OUTPUT_PORT_NAME = "result"
_DATACLASS_DECORATOR = "dataclass"


def module_name_of_a_source_path(relative_path: str) -> Optional[str]:
    """The import name of the `.py` file at `relative_path` under an import root, or `None` for one no import names."""
    path = PurePosixPath(relative_path)
    if path.suffix != ".py":
        return None
    parts = list(path.with_suffix("").parts)
    if parts and parts[-1] == "__init__":
        parts.pop()
    if not parts or not all(part.isidentifier() for part in parts):
        return None
    return ".".join(parts)


def read_node_types_and_graph_generators_from_module_sources(
    module_sources: Iterable[tuple[str, str, str | OSError]],
) -> dict[str, Any]:
    """Read each `(module name, path as listed, source text or why it could not be read)` that imports the SDK.

    Answers `{"node_types", "graph_generators", "named_pin_types", "unreadable_modules"}`, each
    sorted, a module that does not parse listed with why and where and the rest read regardless.
    """
    node_types: list[dict[str, Any]] = []
    graph_generators: list[dict[str, Any]] = []
    named_pin_types: list[dict[str, Any]] = []
    unreadable_modules: list[dict[str, Any]] = []
    module_sources = list(module_sources)
    modules_of_the_source = ModulesOfOneSource(
        _ModuleDeclarationReading.of_the_names_a_module_binds,
        {
            module_name: (listed_path, source_text)
            for module_name, listed_path, source_text in module_sources
            if isinstance(source_text, str)
        }
    )
    for module_name, listed_path, source_text in module_sources:
        if isinstance(source_text, OSError):
            unreadable_modules.append(
                {
                    "module": module_name,
                    "path": listed_path,
                    "line": None,
                    "column": None,
                    "why": f"{type(source_text).__name__}: {source_text.strerror or source_text}",
                }
            )
            continue
        if SDK_IMPORT_MARKER not in source_text:
            continue
        try:
            module_tree = ast.parse(source_text, filename=listed_path)
        except SyntaxError as not_python:
            unreadable_modules.append(
                {
                    "module": module_name,
                    "path": listed_path,
                    "line": not_python.lineno,
                    "column": not_python.offset,
                    "why": f"{type(not_python).__name__}: {not_python.msg}",
                }
            )
            continue
        except ValueError as not_python:
            unreadable_modules.append(
                {
                    "module": module_name,
                    "path": listed_path,
                    "line": None,
                    "column": None,
                    "why": f"{type(not_python).__name__}: {not_python}",
                }
            )
            continue
        module_reading = _ModuleDeclarationReading(
            module_name, listed_path, module_tree, modules_of_its_source=modules_of_the_source
        )
        node_types.extend(module_reading.node_types)
        graph_generators.extend(module_reading.graph_generators)
        named_pin_types.extend(module_reading.named_pin_types)
    node_types.sort(key=lambda node_type: node_type["type"])
    graph_generators.sort(key=lambda graph_generator: graph_generator["generator"])
    named_pin_types.sort(key=lambda named_pin_type: named_pin_type["named_pin_type"])
    unreadable_modules.sort(key=lambda unreadable_module: unreadable_module["path"])
    return {
        "node_types": node_types,
        "graph_generators": graph_generators,
        "named_pin_types": named_pin_types,
        "unreadable_modules": unreadable_modules,
    }


def read_retired_declarations_from_module_source(
    module_name: str, listed_path: str, source_text: str
) -> list[dict[str, Any]]:
    """Each node the module declares in a retired form: `{"node_type", "retired", "line", "column"}`.

    `retired` is the `RetiredDeclaration`, and the place is its decorator's `@`, line and column
    from 1. A module that does not import the SDK or does not parse declares none here.
    """
    if SDK_IMPORT_MARKER not in source_text:
        return []
    try:
        module_tree = ast.parse(source_text, filename=listed_path)
    except (SyntaxError, ValueError):
        return []
    return _ModuleDeclarationReading(module_name, listed_path, module_tree).retired_declarations


class _ModuleDeclarationReading:
    """One module's node types and graph generators, read from its parsed tree; with
    `reads_its_declarations` false, only the names it binds, which another module reads."""

    def __init__(
        self,
        module_name: str,
        listed_path: str,
        module_tree: ast.Module,
        *,
        modules_of_its_source: Optional[ModulesOfOneSource] = None,
        reads_its_declarations: bool = True,
    ) -> None:
        self._module_name = module_name
        self._listed_path = listed_path
        self._modules_of_its_source = modules_of_its_source
        self._sdk_names_by_local_name = _sdk_names_by_local_name(module_tree)
        self._module_level_strings_by_name = _module_level_strings_by_name(module_tree)
        self._classes_by_name = {
            statement.name: statement
            for statement in module_tree.body
            if isinstance(statement, ast.ClassDef)
        }
        self._named_tuples_called_by_name = named_tuples_called_by_name(module_tree)
        self._imported_names = imported_names_by_local_name(module_tree)
        self._imported_modules = imported_modules_by_local_name(module_tree)
        self.node_types: list[dict[str, Any]] = []
        self.graph_generators: list[dict[str, Any]] = []
        self.named_pin_types: list[dict[str, Any]] = []
        self.retired_declarations: list[dict[str, Any]] = []
        if reads_its_declarations:
            self._read_statements(module_tree.body, qualname_prefix="", at_module_scope=True)

    @classmethod
    def of_the_names_a_module_binds(
        cls,
        module_name: str,
        listed_path: str,
        module_tree: ast.Module,
        modules_of_its_source: ModulesOfOneSource,
    ) -> "_ModuleDeclarationReading":
        """The names the module `module_name` binds, read for another module of its source."""
        return cls(
            module_name,
            listed_path,
            module_tree,
            modules_of_its_source=modules_of_its_source,
            reads_its_declarations=False,
        )

    def _read_statements(
        self, statements: list[ast.stmt], qualname_prefix: str, at_module_scope: bool
    ) -> None:
        for statement in statements:
            if isinstance(statement, ast.ClassDef):
                qualname = f"{qualname_prefix}{statement.name}"
                decorator = self._decorated_with(statement, PROCESS_NODE_DECORATOR)
                if decorator is not None:
                    self.node_types.append(self._process_node_type(statement, qualname))
                    self._note_a_retired_form(decorator, PROCESS_NODE_DECORATOR, qualname)
                elif self._decorated_with(statement, TYPED_NODE_DECORATOR) is not None:
                    self.node_types.append(self._typed_class_node_type(statement, qualname))
                named_pin_type = self._decorated_with(statement, NAMED_PIN_TYPE_DECORATOR)
                if named_pin_type is not None:
                    self.named_pin_types.append(
                        {
                            "named_pin_type": f"{self._module_name}:{qualname}",
                            "description": self._keyword_string(named_pin_type, "description"),
                            "path": self._listed_path,
                            "line": statement.lineno,
                        }
                    )
                self._read_statements(
                    statement.body, qualname_prefix=f"{qualname}.", at_module_scope=False
                )
            elif isinstance(statement, (ast.FunctionDef, ast.AsyncFunctionDef)):
                self._read_function(statement, qualname_prefix, at_module_scope)
            elif at_module_scope and isinstance(statement, (ast.Assign, ast.AnnAssign)):
                self._read_rust_function_node_assignment(statement)

    def _read_function(
        self,
        function: ast.FunctionDef | ast.AsyncFunctionDef,
        qualname_prefix: str,
        at_module_scope: bool,
    ) -> None:
        qualname = f"{qualname_prefix}{function.name}"
        if at_module_scope and self._decorated_with(function, GRAPH_GENERATOR_DECORATOR) is not None:
            self.graph_generators.append(self._graph_generator(function, qualname))
        for decorator_name, kind, reads_pin_types in (
            (FUNCTION_NODE_DECORATOR, "function", False),
            (TYPED_NODE_DECORATOR, "function", True),
            (SHADER_NODE_DECORATOR, "shader", False),
        ):
            decorator = self._decorated_with(function, decorator_name)
            if decorator is not None:
                self.node_types.append(
                    self._function_or_shader_node_type(
                        function, qualname, kind, reads_pin_types=reads_pin_types
                    )
                )
                self._note_a_retired_form(decorator, decorator_name, qualname)

    def _note_a_retired_form(self, decorator: ast.expr, sdk_name: str, qualname: str) -> None:
        retired = _RETIRED_DECLARATIONS_BY_DECORATOR.get(sdk_name)
        if retired is None:
            return
        self.retired_declarations.append(
            {
                "node_type": f"{self._module_name}:{qualname}",
                "retired": retired,
                "line": decorator.lineno,
                # A decorator's expression starts just after its `@`, so its 0-based offset is
                # the `@`'s column from 1.
                "column": decorator.col_offset,
            }
        )

    def _decorated_with(
        self, declaration: ast.ClassDef | ast.FunctionDef | ast.AsyncFunctionDef, sdk_name: str
    ) -> Optional[ast.expr]:
        for decorator in declaration.decorator_list:
            called = decorator.func if isinstance(decorator, ast.Call) else decorator
            if self._sdk_name_of(called) == sdk_name:
                return decorator
        return None

    def _sdk_name_of(self, expression: ast.expr) -> Optional[str]:
        dotted_name = _dotted_name_of(expression)
        if dotted_name is None:
            return None
        first_name, _, rest = dotted_name.partition(".")
        sdk_name = self._sdk_names_by_local_name.get(first_name)
        if sdk_name is None:
            return None
        return f"{sdk_name}.{rest}" if rest else sdk_name

    def _process_node_type(self, node_class: ast.ClassDef, qualname: str) -> dict[str, Any]:
        inputs: list[dict[str, Any]] = []
        outputs: list[dict[str, Any]] = []
        for statement in node_class.body:
            if isinstance(statement, ast.Assign) and len(statement.targets) == 1:
                target, value = statement.targets[0], statement.value
            elif isinstance(statement, ast.AnnAssign) and statement.value is not None:
                target, value = statement.target, statement.value
            else:
                continue
            if not isinstance(target, ast.Name) or not isinstance(value, ast.Call):
                continue
            declared_as = self._sdk_name_of(value.func)
            if declared_as == INPUT_PORT_DECLARATION:
                inputs.append(self._declared_port(target.id, value))
            elif declared_as == OUTPUT_PORT_DECLARATION:
                outputs.append(self._declared_port(target.id, value))
        return self._node_type(
            qualname,
            "process",
            declaration=node_class,
            inputs=inputs,
            outputs=outputs,
        )

    def _typed_class_node_type(self, node_class: ast.ClassDef, qualname: str) -> dict[str, Any]:
        """A class under bare `@node`: each public annotated attribute a field, an `InputPin`
        or an `OutputPin`, in the order written; `_private` and `ClassVar` ones are state."""
        inputs: list[dict[str, Any]] = []
        outputs: list[dict[str, Any]] = []
        bound_of_each_type_parameter = _the_bound_of_each_type_parameter_of(node_class)
        for statement in node_class.body:
            if not isinstance(statement, ast.AnnAssign) or not isinstance(statement.target, ast.Name):
                continue
            attribute_name = statement.target.id
            pin_form, type_annotation = _class_node_pin_form_and_its_type(statement.annotation)
            if attribute_name.startswith("_") or pin_form == _CLASS_VARIABLE_ANNOTATION:
                continue
            port: dict[str, Any] = {
                "name": _port_name_as_cast(attribute_name),
                "family": _function_port_family(
                    _read_with_each_type_parameter_as_its_bound(
                        type_annotation, bound_of_each_type_parameter
                    )
                ),
                "description": self._described_text_of(statement.annotation)
                or self._described_text_of(type_annotation),
                "annotation": ast.unparse(statement.annotation),
            }
            if pin_form == _OUTPUT_PIN_ANNOTATION:
                outputs.append(port)
                continue
            if pin_form is None and statement.value is not None:
                port.update(_default_as_listed(statement.value, literal_key="pin_default"))
            inputs.append(port)
        return self._node_type(
            qualname, "process", declaration=node_class, inputs=inputs, outputs=outputs
        )

    def _declared_port(self, attribute_name: str, port_declaration: ast.Call) -> dict[str, Any]:
        family = None
        if port_declaration.args:
            family_name = _dotted_name_of(port_declaration.args[0])
            if family_name is not None:
                family = _PORT_FAMILY_BY_DECLARATION_NAME.get(family_name.rsplit(".", 1)[-1])
        return {
            "name": _port_name_as_cast(attribute_name),
            "family": family,
            "description": self._keyword_string(port_declaration, "description"),
            "annotation": None,
        }

    def _function_or_shader_node_type(
        self,
        function: ast.FunctionDef | ast.AsyncFunctionDef,
        qualname: str,
        kind: str,
        *,
        reads_pin_types: bool,
    ) -> dict[str, Any]:
        inputs = [
            self._function_port(parameter.arg, parameter.annotation, pin_default=default)
            for parameter, default in _named_parameters_with_defaults(function)
            if parameter.arg != _FUNCTION_NODE_CONFIG_PARAMETER
            and _annotated_type_name(parameter.annotation) not in _FUNCTION_NODE_NON_PIN_ANNOTATIONS
        ]
        return self._node_type(
            qualname,
            kind,
            declaration=function,
            inputs=inputs,
            outputs=self._function_outputs(function.returns, reads_pin_types=reads_pin_types),
        )

    def _function_outputs(
        self, return_annotation: Optional[ast.expr], *, reads_pin_types: bool
    ) -> Optional[list[dict[str, Any]]]:
        if return_annotation is None:
            return None
        output_annotation = return_annotation
        if (
            isinstance(output_annotation, ast.Subscript)
            and _annotated_type_name(output_annotation) == _STATEFUL_RETURN
        ):
            stateful_arguments = _subscript_arguments(output_annotation)
            if not stateful_arguments:
                return None
            output_annotation = stateful_arguments[0]
        # Under the family grammar a returned dataclass is one output per field; under bare
        # `@node` a returned `NamedTuple` is, and a dataclass is one bundle pin. A named pin
        # type is one pin under either.
        record_of_outputs = self._the_record_an_annotation_names(output_annotation)
        if record_of_outputs is not None and not (
            (record_of_outputs.is_a_named_tuple if reads_pin_types else record_of_outputs.is_a_dataclass)
            and not record_of_outputs.is_a_named_pin_type
        ):
            record_of_outputs = None
        if record_of_outputs is not None:
            return [
                field.typed_in._function_port(field.name, field.annotation)
                for field in record_of_outputs.fields
            ]
        return [self._function_port(_SINGLE_OUTPUT_PORT_NAME, output_annotation)]

    def _the_record_an_annotation_names(
        self, annotation: Optional[ast.expr]
    ) -> Optional[RecordOfOutputs]:
        """The record `annotation` names, defined in this module or imported from another of its
        source; `None` for any other annotation."""
        if isinstance(annotation, ast.Constant) and isinstance(annotation.value, str):
            try:
                annotation = ast.parse(annotation.value, mode="eval").body
            except SyntaxError:
                return None
        dotted_name = None if annotation is None else _dotted_name_of(annotation)
        if dotted_name is None:
            return None
        owner, _, name = dotted_name.rpartition(".")
        return self._the_record_a_dotted_name_names(dotted_name, frozenset())

    def _the_record_a_dotted_name_names(
        self, dotted_name: str, seen: frozenset[tuple[str, str]]
    ) -> Optional[RecordOfOutputs]:
        """The record `dotted_name` names here: a name this module binds, or a name of a module of
        its source this module imports; `None` for a module the source does not hold."""
        owner, _, name = dotted_name.rpartition(".")
        if not owner:
            return self._the_record_named(name, seen)
        owner_module = self._the_module_a_name_binds(owner)
        reading = None if owner_module is None else self._reading_of(owner_module)
        return None if reading is None else reading._the_record_named(name, seen)

    def _the_record_named(
        self, name: str, seen: frozenset[tuple[str, str]]
    ) -> Optional[RecordOfOutputs]:
        """The record this module binds to `name`: a class it defines, a `NamedTuple` it calls
        into being, or one it imports from another module of its source; `seen` holds each
        `(module, name)` already followed, so an import cycle reads as no record."""
        if (self._module_name, name) in seen:
            return None
        seen = seen | {(self._module_name, name)}
        record_class = self._classes_by_name.get(name)
        if record_class is not None:
            return self._the_record_a_class_defines(record_class, seen)
        named_tuple_fields = self._named_tuples_called_by_name.get(name)
        if named_tuple_fields is not None:
            return RecordOfOutputs(
                fields=[
                    FieldOfARecord(field_name, field_annotation, self)
                    for field_name, field_annotation in named_tuple_fields
                ],
                is_a_named_tuple=True,
                is_a_dataclass=False,
                is_a_named_pin_type=False,
            )
        imported = self._imported_names.get(name)
        if imported is None:
            return None
        relative_level, from_module, imported_name = imported
        reading = self._reading_of(self._the_module_imported_from(relative_level, from_module))
        return None if reading is None else reading._the_record_named(imported_name, seen)

    def _the_record_a_class_defines(
        self, record_class: ast.ClassDef, seen: frozenset[tuple[str, str]]
    ) -> RecordOfOutputs:
        """The record `record_class` is: a `NamedTuple` of the fields it annotates, or of those of
        the `NamedTuple` it subclasses; else a record of the fields it annotates."""
        is_a_named_pin_type = self._decorated_with(record_class, NAMED_PIN_TYPE_DECORATOR) is not None
        is_a_dataclass = any(
            _annotated_type_name(
                decorator.func if isinstance(decorator, ast.Call) else decorator
            )
            == _DATACLASS_DECORATOR
            for decorator in record_class.decorator_list
        )
        own_fields = [
            FieldOfARecord(field.target.id, field.annotation, self)
            for field in record_class.body
            if isinstance(field, ast.AnnAssign) and isinstance(field.target, ast.Name)
        ]
        if any(_annotated_type_name(base) == NAMED_TUPLE_BASE for base in record_class.bases):
            return RecordOfOutputs(own_fields, True, False, is_a_named_pin_type)
        for base in record_class.bases:
            base_name = _dotted_name_of(base)
            base_record = (
                None if base_name is None else self._the_record_a_dotted_name_names(base_name, seen)
            )
            if base_record is not None and base_record.is_a_named_tuple:
                # A class subclassing a `NamedTuple` holds the fields that `NamedTuple` names.
                return RecordOfOutputs(base_record.fields, True, False, is_a_named_pin_type)
        return RecordOfOutputs(own_fields, False, is_a_dataclass, is_a_named_pin_type)

    def _the_module_a_name_binds(self, local_name: str) -> Optional[str]:
        """The module `local_name` binds by an import: `import a.b as c` binds `c` to `a.b`, and
        `from a import b` binds `b` to `a.b`, when that is a module."""
        first_name, _, rest = local_name.partition(".")
        bound = self._imported_modules.get(first_name)
        if bound is None and first_name in self._imported_names:
            relative_level, from_module, imported_name = self._imported_names[first_name]
            parent = self._the_module_imported_from(relative_level, from_module)
            bound = f"{parent}.{imported_name}" if parent else imported_name
        if bound is None:
            return None
        return f"{bound}.{rest}" if rest else bound

    def _the_module_imported_from(self, relative_level: int, from_module: str) -> str:
        """The absolute name of the module `from <dots><from_module> import ...` names here."""
        if relative_level == 0:
            return from_module
        package_parts = self._module_name.split(".")
        if self._modules_of_its_source is None or not self._modules_of_its_source.is_a_package(
            self._module_name
        ):
            package_parts = package_parts[:-1]
        package_parts = package_parts[: len(package_parts) - (relative_level - 1)]
        return ".".join([*package_parts, *([from_module] if from_module else [])])

    def _reading_of(self, module_name: str) -> Optional["_ModuleDeclarationReading"]:
        """The reading of `module_name`, this module or another of its source."""
        if module_name == self._module_name:
            return self
        if self._modules_of_its_source is None:
            return None
        return self._modules_of_its_source.reading_of(module_name)

    def _function_port(
        self,
        spelling: str,
        annotation: Optional[ast.expr],
        pin_default: Optional[ast.expr] = None,
    ) -> dict[str, Any]:
        function_port: dict[str, Any] = {
            "name": _port_name_as_cast(spelling),
            "family": _function_port_family(annotation),
            "description": self._described_text_of(annotation),
            "annotation": None if annotation is None else ast.unparse(annotation),
        }
        if pin_default is not None:
            function_port.update(_default_as_listed(pin_default, literal_key="pin_default"))
        return function_port

    def _node_meta_declared_beside(self, declaration: ast.stmt) -> dict[str, str]:
        """What the `@node.meta` decorators on `declaration` declare, by key: each a string its source states, the topmost winning."""
        if not isinstance(declaration, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            return {}
        declared: dict[str, str] = {}
        for decorator in reversed(declaration.decorator_list):
            if (
                not isinstance(decorator, ast.Call)
                or self._sdk_name_of(decorator.func) != NODE_META_DECORATOR
            ):
                continue
            for meta_key in _NODE_META_KEYS:
                stated = self._keyword_string(decorator, meta_key)
                if stated:
                    declared[meta_key] = stated
        return declared

    def _keyword_string(self, call: Optional[ast.expr], keyword_name: str) -> str:
        if not isinstance(call, ast.Call):
            return ""
        for keyword in call.keywords:
            if keyword.arg == keyword_name:
                return self._string_value_of(keyword.value)
        return ""

    def _string_value_of(self, expression: ast.expr) -> str:
        """A string literal, or a module-level name bound to one; the empty string otherwise."""
        if isinstance(expression, ast.Constant) and isinstance(expression.value, str):
            return expression.value
        if isinstance(expression, ast.Name):
            return self._module_level_strings_by_name.get(expression.id, "")
        return ""

    def _described_text_of(self, annotation: Optional[ast.expr]) -> str:
        """The text of the `Description(...)` an annotation carries, also on `T` of `T | None`."""
        if annotation is None:
            return ""
        if isinstance(annotation, ast.Constant) and isinstance(annotation.value, str):
            try:
                return self._described_text_of(ast.parse(annotation.value, mode="eval").body)
            except SyntaxError:
                return ""
        if isinstance(annotation, ast.BinOp) and isinstance(annotation.op, ast.BitOr):
            return self._described_text_of(annotation.left) or self._described_text_of(
                annotation.right
            )
        if not isinstance(annotation, ast.Subscript):
            return ""
        type_name = _annotated_type_name(annotation)
        arguments = _subscript_arguments(annotation)
        if type_name == _OPTIONAL_ANNOTATION and arguments:
            return self._described_text_of(arguments[0])
        if type_name != _ANNOTATED_ANNOTATION or not arguments:
            return ""
        for metadata in arguments[1:]:
            if (
                isinstance(metadata, ast.Call)
                and self._sdk_name_of(metadata.func) == DESCRIPTION_DECLARATION
                and metadata.args
            ):
                return self._string_value_of(metadata.args[0])
        return self._described_text_of(arguments[0])

    def _read_rust_function_node_assignment(self, statement: ast.Assign | ast.AnnAssign) -> None:
        targets = statement.targets if isinstance(statement, ast.Assign) else [statement.target]
        value = statement.value
        if (
            len(targets) != 1
            or not isinstance(targets[0], ast.Name)
            or not isinstance(value, ast.Call)
            or self._sdk_name_of(value.func) != RUST_FUNCTION_NODE_DECLARATION
        ):
            return
        self.node_types.append(
            self._node_type(
                targets[0].id,
                "function",
                declaration=statement,
                inputs=None,
                outputs=None,
            )
        )

    def _node_type(
        self,
        qualname: str,
        kind: str,
        *,
        declaration: ast.stmt,
        inputs: Optional[list[dict[str, Any]]],
        outputs: Optional[list[dict[str, Any]]],
    ) -> dict[str, Any]:
        declared_meta = self._node_meta_declared_beside(declaration)
        return {
            "type": f"{self._module_name}:{qualname}",
            "kind": kind,
            "description": declared_meta.get("description", ""),
            "category": declared_meta.get("category"),
            "inputs": inputs,
            "outputs": outputs,
            "path": self._listed_path,
            "line": declaration.lineno,
        }

    def _graph_generator(
        self, function: ast.FunctionDef | ast.AsyncFunctionDef, qualname: str
    ) -> dict[str, Any]:
        promoted_config = [
            _promoted_setting(parameter, default)
            for parameter, default in _named_parameters_with_defaults(function)[1:]
        ]
        return {
            "generator": f"{self._module_name}:{qualname}",
            "description": inspect.cleandoc(ast.get_docstring(function, clean=False) or ""),
            "promoted_config": promoted_config,
            "path": self._listed_path,
            "line": function.lineno,
        }


def _sdk_names_by_local_name(module_tree: ast.Module) -> dict[str, str]:
    """Each name the module binds at its top level to the SDK or one of its members, by that name."""
    sdk_names_by_local_name: dict[str, str] = {}
    for statement in module_tree.body:
        if isinstance(statement, ast.ImportFrom) and statement.level == 0:
            for imported in statement.names:
                imported_as = imported.asname or imported.name
                if statement.module == SDK_MODULE_NAME:
                    sdk_names_by_local_name[imported_as] = f"{SDK_MODULE_NAME}.{imported.name}"
                elif statement.module == "tatolab" and imported.name == "stream":
                    sdk_names_by_local_name[imported_as] = SDK_MODULE_NAME
        elif isinstance(statement, ast.Import):
            for imported in statement.names:
                if imported.name == SDK_MODULE_NAME and imported.asname is not None:
                    sdk_names_by_local_name[imported.asname] = SDK_MODULE_NAME
                elif imported.name in (SDK_MODULE_NAME, "tatolab") and imported.asname is None:
                    sdk_names_by_local_name["tatolab"] = "tatolab"
    return sdk_names_by_local_name


def _dotted_name_of(expression: ast.expr) -> Optional[str]:
    if isinstance(expression, ast.Name):
        return expression.id
    if isinstance(expression, ast.Attribute):
        owner = _dotted_name_of(expression.value)
        return None if owner is None else f"{owner}.{expression.attr}"
    return None


def _module_level_strings_by_name(module_tree: ast.Module) -> dict[str, str]:
    """Each name the module binds once at its top level to a string literal, by that name."""
    strings_by_name: dict[str, str] = {}
    names_bound_more_than_once: set[str] = set()
    for statement in module_tree.body:
        if isinstance(statement, ast.Assign) and len(statement.targets) == 1:
            target, value = statement.targets[0], statement.value
        elif isinstance(statement, ast.AnnAssign) and statement.value is not None:
            target, value = statement.target, statement.value
        else:
            continue
        if not isinstance(target, ast.Name):
            continue
        if target.id in strings_by_name:
            names_bound_more_than_once.add(target.id)
        if isinstance(value, ast.Constant) and isinstance(value.value, str):
            strings_by_name[target.id] = value.value
    for name in names_bound_more_than_once:
        strings_by_name.pop(name, None)
    return strings_by_name


def _port_name_as_cast(spelling: str) -> str:
    try:
        return cast_exposed_name_to_url_safe(spelling)
    except ExposedNameCastsToNothingError:
        return spelling


def _named_parameters_with_defaults(
    function: ast.FunctionDef | ast.AsyncFunctionDef,
) -> list[tuple[ast.arg, Optional[ast.expr]]]:
    arguments = function.args
    positional = [*arguments.posonlyargs, *arguments.args]
    positional_defaults: list[Optional[ast.expr]] = [None] * (
        len(positional) - len(arguments.defaults)
    ) + list(arguments.defaults)
    return [
        *zip(positional, positional_defaults),
        *zip(arguments.kwonlyargs, arguments.kw_defaults),
    ]


def _promoted_setting(parameter: ast.arg, default: Optional[ast.expr]) -> dict[str, Any]:
    setting: dict[str, Any] = {
        "name": parameter.arg,
        "annotation": None if parameter.annotation is None else ast.unparse(parameter.annotation),
    }
    if default is not None:
        setting.update(_default_as_listed(default, literal_key="default"))
    return setting


def _default_as_listed(default: ast.expr, literal_key: str) -> dict[str, Any]:
    """A default under `literal_key` where it is a literal, else its source under `<literal_key>_as_written`."""
    try:
        literal = ast.literal_eval(default)
        json.dumps(literal, allow_nan=False)
    except (ValueError, TypeError, SyntaxError, MemoryError, RecursionError):
        return {f"{literal_key}_as_written": ast.unparse(default)}
    return {literal_key: literal}


def _subscript_arguments(subscript: ast.Subscript) -> list[ast.expr]:
    if isinstance(subscript.slice, ast.Tuple):
        return list(subscript.slice.elts)
    return [subscript.slice]


def _annotated_type_name(annotation: Optional[ast.expr]) -> Optional[str]:
    """The last name of what an annotation names, its subscript stripped: `State` for `State[Rec]`."""
    if annotation is None:
        return None
    if isinstance(annotation, ast.Subscript):
        return _annotated_type_name(annotation.value)
    if isinstance(annotation, ast.Constant) and isinstance(annotation.value, str):
        try:
            return _annotated_type_name(ast.parse(annotation.value, mode="eval").body)
        except SyntaxError:
            return None
    dotted_name = _dotted_name_of(annotation)
    return None if dotted_name is None else dotted_name.rsplit(".", 1)[-1]


def _function_port_family(annotation: Optional[ast.expr]) -> Optional[str]:
    """The family a function or shader port's annotation types, or `None` where source alone does not say."""
    if annotation is None:
        return None
    if isinstance(annotation, ast.Constant) and isinstance(annotation.value, str):
        try:
            return _function_port_family(ast.parse(annotation.value, mode="eval").body)
        except SyntaxError:
            return None
    if isinstance(annotation, ast.BinOp) and isinstance(annotation.op, ast.BitOr):
        for side, other_side in ((annotation.left, annotation.right), (annotation.right, annotation.left)):
            if isinstance(other_side, ast.Constant) and other_side.value is None:
                return _function_port_family(side)
        return None
    type_name = _annotated_type_name(annotation)
    if isinstance(annotation, ast.Subscript):
        arguments = _subscript_arguments(annotation)
        if type_name == _OPTIONAL_ANNOTATION and arguments:
            return _function_port_family(arguments[0])
        if type_name == _ANNOTATED_ANNOTATION and arguments:
            if _annotated_type_name(arguments[0]) == _UNREAD_MESSAGE_ANNOTATION:
                for metadata in arguments[1:]:
                    family_name = _annotated_type_name(metadata)
                    if family_name in _PORT_FAMILY_BY_DECLARATION_NAME:
                        return _PORT_FAMILY_BY_DECLARATION_NAME[family_name]
                return None
            return _function_port_family(arguments[0])
    if type_name is None:
        return None
    return _PORT_FAMILY_BY_ANNOTATED_TYPE.get(type_name)


def _the_bound_of_each_type_parameter_of(node_class: ast.ClassDef) -> dict[str, ast.expr]:
    """Each type parameter the class statement binds to a type, `class X[Frame: Texture]`, by its name."""
    return {
        type_parameter.name: type_parameter.bound
        for type_parameter in node_class.type_params
        if isinstance(type_parameter, ast.TypeVar) and type_parameter.bound is not None
    }


def _read_with_each_type_parameter_as_its_bound(
    annotation: ast.expr, bound_of_each_type_parameter: dict[str, ast.expr]
) -> ast.expr:
    """`annotation` with each name of a bound type parameter written as its bound, so a pin
    typed `Frame` of `[Frame: Texture]` is read as a `Texture` pin."""
    if isinstance(annotation, ast.Name):
        return bound_of_each_type_parameter.get(annotation.id, annotation)
    if isinstance(annotation, ast.Subscript) and _annotated_type_name(annotation) == _ANNOTATED_ANNOTATION:
        arguments = _subscript_arguments(annotation)
        if arguments and isinstance(arguments[0], ast.Name):
            return bound_of_each_type_parameter.get(arguments[0].id, annotation)
    return annotation


def _class_node_pin_form_and_its_type(annotation: ast.expr) -> tuple[Optional[str], ast.expr]:
    """The form a class node's attribute annotation writes, `InputPin`, `OutputPin` or `ClassVar`,
    and the type inside it; `None` and the annotation itself for a field."""
    if isinstance(annotation, ast.Constant) and isinstance(annotation.value, str):
        try:
            return _class_node_pin_form_and_its_type(ast.parse(annotation.value, mode="eval").body)
        except SyntaxError:
            return None, annotation
    written = annotation
    if isinstance(written, ast.Subscript) and _annotated_type_name(written) == _ANNOTATED_ANNOTATION:
        arguments = _subscript_arguments(written)
        if arguments:
            written = arguments[0]
    form = _annotated_type_name(written)
    if form in (_INPUT_PIN_ANNOTATION, _OUTPUT_PIN_ANNOTATION, _CLASS_VARIABLE_ANNOTATION):
        arguments = _subscript_arguments(written) if isinstance(written, ast.Subscript) else []
        return form, arguments[0] if arguments else written
    return None, annotation
