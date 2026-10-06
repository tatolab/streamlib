# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Deriving a processor's config schema from the config class its author wrote.

The document is JSON Schema draft 2020-12 with no `$schema` key — the dialect
`sdk/streamlib-processor-schema/src/config_schema_document.rs` emits for a Rust
config type, so a node serves one dialect whichever language declared the
processor. Nested classes are inlined and `Optional[T]` is an `anyOf` with null,
so nothing here emits a `$ref`. One document can still carry `$defs`: a model
handed in as the config class contributes its own schema, and a pydantic model
writes its nested types that way. A model nested under a *property* is flattened
instead, because its pointers are root-relative and resolve against nothing once
the document is no longer the root.

The dialect is shared; two spellings inside it are not, and both are valid
2020-12. The Rust side writes a nullable as `"type": [T, "null"]` and stamps a
root `title` from the config type's name, because `schemars` does. Neither is
worth converting on either side, and a reader who takes one for drift would
break the other.

Stdlib only. A model is recognised by the `model_json_schema` method it carries
rather than by importing pydantic, which the wheel does not depend on.

`additionalProperties` is stated only where the config class refuses unknown
keys: a dataclass does, a TypedDict does not, and a model's own document speaks
for itself.
"""

from __future__ import annotations

import collections.abc
import dataclasses
import enum
import inspect
import math
import types
import typing
from typing import Any

__all__ = [
    "derive_config_class_json_schema",
    "json_schema_for_a_processor_declaring_no_config",
]

_SCALAR_JSON_TYPES = {
    bool: "boolean",
    int: "integer",
    float: "number",
    str: "string",
}

_SEQUENCE_ORIGINS = (
    list,
    set,
    frozenset,
    tuple,
    collections.abc.Sequence,
    collections.abc.MutableSequence,
    collections.abc.Set,
)

_MAPPING_ORIGINS = (
    dict,
    collections.abc.Mapping,
    collections.abc.MutableMapping,
)

# `get_type_hints` keeps a TypedDict key's requiredness qualifier, which says
# nothing about the value's type and hides it from the mapper. Recognised by
# the special form's own name rather than by identity, because the 3.10 floor
# has these from `typing_extensions` and 3.11+ from `typing`, and a config
# class may be written against either.
_REQUIREDNESS_QUALIFIER_NAMES = ("Required", "NotRequired")

# Where a model's own document points at its own definitions.
_DEFINITION_POINTER_PREFIX = "#/$defs/"


def _is_a_typed_dict(candidate: Any) -> bool:
    """Whether `candidate` is a TypedDict under either spelling.

    `typing.is_typeddict` recognises `typing.TypedDict` alone, and a
    `typing_extensions.TypedDict` subclass — what an author on the 3.10 floor
    reaches for, and what `Required` / `NotRequired` need there — is invisible
    to it. The pair of key sets is the structural signature of one; nothing
    else carries both.
    """
    return typing.is_typeddict(candidate) or (
        isinstance(candidate, type)
        and hasattr(candidate, "__required_keys__")
        and hasattr(candidate, "__optional_keys__")
    )


def json_schema_for_a_processor_declaring_no_config() -> "dict[str, Any]":
    """The document a processor that takes no configuration publishes.

    Mirrors what `EmptyConfig` publishes on the Rust side, description
    included, so the catalog reads the same for either language.
    """
    return {
        "type": "object",
        "description": "This processor declares no configuration.",
        "additionalProperties": False,
    }


def derive_config_class_json_schema(config_class: type) -> "dict[str, Any]":
    """The JSON Schema of `config_class`, derived from what its author wrote."""
    return _document_for_class(config_class, ())


def _document_for_class(
    config_class: type, classes_being_inlined: "tuple[type, ...]"
) -> "dict[str, Any]":
    if config_class in classes_being_inlined:
        # A config class that reaches itself. Inlining is the only nesting this
        # module emits, so a cycle has no fixed point — the walk stops here and
        # the key says only that it is an object. Without this the recursion
        # exhausts the stack at decoration, which is import time.
        return {"type": "object"}

    model_json_schema = getattr(config_class, "model_json_schema", None)
    if callable(model_json_schema):
        return _document_the_model_carries(config_class, model_json_schema)

    ancestry = classes_being_inlined + (config_class,)
    if _is_a_typed_dict(config_class):
        return _typed_dict_document(config_class, ancestry)
    if dataclasses.is_dataclass(config_class):
        return _dataclass_document(config_class, ancestry)
    # An open object says the configuration is a mapping and claims nothing
    # about its keys, which is all that can honestly be derived from a class of
    # a kind the deriver does not recognise.
    return {"type": "object"}


def _resolved_against_its_own_definitions(document: Any) -> Any:
    """`document` with each `#/$defs/` pointer replaced by what it points at.

    A model nested under a property carries pointers written when it was the
    root, so they name a `$defs` the enclosing document does not have. Nothing
    else here emits a `$ref`, so this only ever has a model's document to walk.
    """
    if not isinstance(document, dict) or "$defs" not in document:
        return document
    definitions = document["$defs"]
    resolved = {key: value for key, value in document.items() if key != "$defs"}
    return _with_pointers_followed(resolved, definitions, ())


def _with_pointers_followed(
    node: Any, definitions: "dict[str, Any]", pointers_being_followed: "tuple[str, ...]"
) -> Any:
    if isinstance(node, list):
        return [
            _with_pointers_followed(entry, definitions, pointers_being_followed)
            for entry in node
        ]
    if not isinstance(node, dict):
        return node

    pointer = node.get("$ref")
    if isinstance(pointer, str) and pointer.startswith(_DEFINITION_POINTER_PREFIX):
        name = pointer[len(_DEFINITION_POINTER_PREFIX) :]
        if name in pointers_being_followed or name not in definitions:
            # A definition that reaches itself, or one the document never
            # carried: an open object beats a pointer to nothing.
            return {"type": "object"}
        followed = _with_pointers_followed(
            definitions[name], definitions, pointers_being_followed + (name,)
        )
        beside_the_pointer = {
            key: _with_pointers_followed(value, definitions, pointers_being_followed)
            for key, value in node.items()
            if key != "$ref"
        }
        return {**followed, **beside_the_pointer}

    return {
        key: _with_pointers_followed(value, definitions, pointers_being_followed)
        for key, value in node.items()
    }


def _document_the_model_carries(
    config_class: type, model_json_schema: "typing.Callable[[], Any]"
) -> "dict[str, Any]":
    """A model's own schema, verbatim minus the two keys the catalog owns."""
    document = model_json_schema()
    if not isinstance(document, dict):
        raise TypeError(
            f"{config_class.__name__}.model_json_schema() returned "
            f"{type(document).__name__} rather than a dict, so it cannot be a "
            f"processor's config class. A config class is a TypedDict, a dataclass, "
            f"or a model whose `model_json_schema()` returns a JSON Schema document."
        )
    return {
        key: value for key, value in document.items() if key not in ("$schema", "title")
    }


def _typed_dict_document(
    config_class: type, ancestry: "tuple[type, ...]"
) -> "dict[str, Any]":
    annotations = _resolved_class_annotations(config_class)
    required_keys = getattr(config_class, "__required_keys__", frozenset())
    document: "dict[str, Any]" = {
        "type": "object",
        "properties": {
            key: _json_schema_for_annotation(annotation, ancestry)
            for key, annotation in annotations.items()
        },
    }
    # Declaration order rather than the set's, so deriving one class twice
    # gives one document.
    required = [key for key in annotations if key in required_keys]
    if required:
        document["required"] = required
    return document


def _dataclass_document(
    config_class: type, ancestry: "tuple[type, ...]"
) -> "dict[str, Any]":
    annotations = _resolved_class_annotations(config_class)
    fields_by_name = {field.name: field for field in dataclasses.fields(config_class)}
    # An `InitVar` is a constructor input that `dataclasses.fields()` omits. Left
    # out it would be absent from `properties` while `additionalProperties: false`
    # forbade it — a catalog telling an agent that a required key is illegal.
    init_parameters = inspect.signature(config_class).parameters
    # And the constructor has the final say on what a configuration may carry,
    # because that is what `config_class(**configuration)` calls. A generated
    # `__init__` takes exactly the `init=True` fields, so this narrows nothing
    # for an ordinary dataclass; `init=False` or a hand-written constructor is
    # where the field list and the callable disagree, and documenting a key the
    # class refuses is the same lie as omitting one it requires.
    accepts_any_key = any(
        parameter.kind is inspect.Parameter.VAR_KEYWORD
        for parameter in init_parameters.values()
    )

    properties: "dict[str, Any]" = {}
    required: "list[str]" = []
    for name, annotation in annotations.items():
        if not accepts_any_key and name not in init_parameters:
            continue
        field = fields_by_name.get(name)
        if field is not None:
            # An `init=False` field is not a constructor input, so a
            # configuration cannot carry it and it is not documented.
            if not field.init:
                continue
            declared_default = (
                field.default if field.default is not dataclasses.MISSING else _ABSENT
            )
            has_default_factory = field.default_factory is not dataclasses.MISSING
            annotation = annotations.get(name, field.type)
        elif isinstance(annotation, dataclasses.InitVar):
            parameter = init_parameters.get(name)
            declared_default = (
                _ABSENT
                if parameter is None or parameter.default is inspect.Parameter.empty
                else parameter.default
            )
            has_default_factory = False
            annotation = annotation.type
        else:
            # A `ClassVar` or a bare annotation the dataclass machinery ignored:
            # not a constructor input either way.
            continue

        field_schema = _json_schema_for_annotation(annotation, ancestry)
        if declared_default is not _ABSENT:
            rendered_default = _json_representable(declared_default)
            if rendered_default is not _NOT_JSON_REPRESENTABLE:
                field_schema["default"] = rendered_default
        elif not has_default_factory:
            required.append(name)
        properties[name] = field_schema

    document: "dict[str, Any]" = {
        "type": "object",
        "properties": properties,
        "additionalProperties": False,
    }
    if required:
        document["required"] = required
    return document


def _resolved_class_annotations(config_class: type) -> "dict[str, Any]":
    """Every annotated field of `config_class`, with forward references resolved."""
    try:
        return typing.get_type_hints(config_class, include_extras=True)
    except Exception as unresolvable:
        raise TypeError(
            f"{config_class.__name__} carries an annotation that cannot be resolved, "
            f"so its config schema cannot be derived: {unresolvable}. Every "
            f"annotation on a config class must name something importable at run "
            f"time, not only under `TYPE_CHECKING`."
        ) from unresolvable


def _json_schema_for_annotation(
    annotation: Any, ancestry: "tuple[type, ...]" = ()
) -> "dict[str, Any]":
    """The schema of one annotated field.

    An annotation the deriver does not recognise renders as an empty schema
    rather than refusing the class: a config class is worth publishing long
    before every type in it is describable.
    """
    origin_name = getattr(typing.get_origin(annotation), "_name", None)
    if origin_name in _REQUIREDNESS_QUALIFIER_NAMES:
        return _json_schema_for_annotation(typing.get_args(annotation)[0], ancestry)

    metadata = getattr(annotation, "__metadata__", None)
    if metadata is not None:
        described = _json_schema_for_annotation(typing.get_args(annotation)[0], ancestry)
        description = next((entry for entry in metadata if isinstance(entry, str)), None)
        if description is not None:
            described["description"] = description
        return described

    if annotation is None or annotation is type(None):
        return {"type": "null"}
    if annotation is Any:
        return {}

    origin = typing.get_origin(annotation)
    if origin is typing.Union or origin is types.UnionType:
        return {
            "anyOf": [
                _json_schema_for_annotation(member, ancestry)
                for member in typing.get_args(annotation)
            ]
        }
    if origin is typing.Literal:
        return _enumerated_schema(typing.get_args(annotation))
    if origin is not None:
        if origin in _SEQUENCE_ORIGINS:
            return _sequence_schema(annotation, origin, ancestry)
        if origin in _MAPPING_ORIGINS:
            return {"type": "object"}
        # A parameterized generic the deriver does not know. This branch is
        # also what keeps one out of the nested-class branch below on Python
        # 3.10, where `isinstance(dict[str, int], type)` is still True.
        return {}

    if isinstance(annotation, type):
        if annotation in _SCALAR_JSON_TYPES:
            return {"type": _SCALAR_JSON_TYPES[annotation]}
        if annotation in _SEQUENCE_ORIGINS:
            return {"type": "array"}
        if annotation in _MAPPING_ORIGINS:
            return {"type": "object"}
        if issubclass(annotation, enum.Enum):
            return _enumerated_schema(tuple(member.value for member in annotation))
        if (
            _is_a_typed_dict(annotation)
            or dataclasses.is_dataclass(annotation)
            or callable(getattr(annotation, "model_json_schema", None))
        ):
            # A model's own document points at its own `$defs` with a
            # root-relative pointer, which resolves against nothing once the
            # document sits under a property. Every other kind is already
            # self-contained, so this is a no-op for them.
            return _resolved_against_its_own_definitions(
                _document_for_class(annotation, ancestry)
            )

    return {}


def _enumerated_schema(members: "tuple[Any, ...]") -> "dict[str, Any]":
    rendered = [_json_representable(member) for member in members]
    if any(member is _NOT_JSON_REPRESENTABLE for member in rendered):
        return {}
    return {"enum": rendered}


def _sequence_schema(
    annotation: Any, origin: Any, ancestry: "tuple[type, ...]"
) -> "dict[str, Any]":
    document: "dict[str, Any]" = {"type": "array"}
    element_annotations = typing.get_args(annotation)
    if origin is tuple:
        # `tuple[T, ...]` is a homogeneous sequence; every other tuple is
        # positional, which 2020-12 spells `prefixItems`.
        if len(element_annotations) == 2 and element_annotations[1] is Ellipsis:
            document["items"] = _json_schema_for_annotation(
                element_annotations[0], ancestry
            )
        elif element_annotations:
            positional_item_schemas = [
                _json_schema_for_annotation(element, ancestry)
                for element in element_annotations
            ]
            document["prefixItems"] = positional_item_schemas
            # `prefixItems` says what each position holds and nothing about how
            # many there are, so a fixed-length tuple that stated only that
            # would validate a shorter or longer array. The Rust seam bounds
            # its tuples the same way.
            document["minItems"] = len(positional_item_schemas)
            document["maxItems"] = len(positional_item_schemas)
        return document
    if len(element_annotations) == 1:
        document["items"] = _json_schema_for_annotation(
            element_annotations[0], ancestry
        )
    return document


class _NotJsonRepresentableSentinel:
    """The type of [`_NOT_JSON_REPRESENTABLE`] — never constructed by an author."""

    __slots__ = ()

    def __repr__(self) -> str:
        return "_NOT_JSON_REPRESENTABLE"


# Distinct from `None`, which is itself a representable default.
_NOT_JSON_REPRESENTABLE = _NotJsonRepresentableSentinel()

# Distinct from `None` for the same reason: a field may default to it.
_ABSENT = object()

# The document crosses into the descriptor through the msgpack value tree the
# data plane uses, which carries no integer wider than 64 bits and would refuse
# the whole declaration rather than the one default.
_WIDEST_REPRESENTABLE_INTEGERS = range(-(2**63), 2**64)


def _json_representable(value: Any) -> Any:
    """`value` as JSON, or [`_NOT_JSON_REPRESENTABLE`] if it is not expressible.

    The derived document crosses into Rust as JSON, so a default the wire
    cannot carry is dropped rather than left to fail the whole declaration.
    """
    if isinstance(value, enum.Enum):
        return _json_representable(value.value)
    if value is None or isinstance(value, (bool, str)):
        return value
    if isinstance(value, int):
        return value if value in _WIDEST_REPRESENTABLE_INTEGERS else _NOT_JSON_REPRESENTABLE
    if isinstance(value, float):
        # JSON has no infinity and no NaN, so a default that is one cannot
        # cross into the descriptor and is dropped rather than rewritten.
        return value if math.isfinite(value) else _NOT_JSON_REPRESENTABLE
    if isinstance(value, (list, tuple)):
        rendered = [_json_representable(entry) for entry in value]
        if any(entry is _NOT_JSON_REPRESENTABLE for entry in rendered):
            return _NOT_JSON_REPRESENTABLE
        return rendered
    if isinstance(value, dict):
        if not all(isinstance(key, str) for key in value):
            return _NOT_JSON_REPRESENTABLE
        rendered = {key: _json_representable(entry) for key, entry in value.items()}
        if any(entry is _NOT_JSON_REPRESENTABLE for entry in rendered.values()):
            return _NOT_JSON_REPRESENTABLE
        return rendered
    return _NOT_JSON_REPRESENTABLE
