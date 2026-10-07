# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A node's config class: how it is declared and the document derived from it.

`@node` reads the config class off `__init__` and refuses every other
signature, and the deriver turns that class into the JSON Schema the catalog
publishes. Constructing the class from a graph node's config mapping is the
runtime's, and is tested beside it.
"""

import dataclasses
from typing import Annotated, Any, Literal, Optional, TypedDict

import pydantic
import pytest

# The 3.10 floor spells per-key requiredness from here, and it is what the
# stream suite's test group installs; `typing.Required` arrives only at 3.11.
from typing_extensions import NotRequired, Required

from tatolab.stream import node

# ---------------------------------------------------------------------------
# The config classes the cases below are declared against
# ---------------------------------------------------------------------------


class BlurConfigTypedDict(TypedDict):
    """Admits anything at run time — the loosest dial an author can pick."""

    width: int
    label: Annotated[str, "What to call this blur."]


class PartialConfigTypedDict(TypedDict, total=False):
    width: int


@dataclasses.dataclass
class BlurConfigDataclass:
    """Refuses an unknown key but not a mistyped value."""

    width: int
    label: str = "unlabelled"
    tags: "list[str]" = dataclasses.field(default_factory=list)
    quality: Literal["fast", "good"] = "fast"
    fallback: Optional[str] = None
    derived: int = dataclasses.field(init=False, default=7)


class BlurConfigModel(pydantic.BaseModel):
    """Validates values — the strictest dial, and it carries its own schema."""

    width: int
    label: str = "unlabelled"


@dataclasses.dataclass
class NestedConfig:
    inner: BlurConfigDataclass
    count: int = 0


# At module scope, not inside their tests: `typing.get_type_hints` resolves a
# class's forward references against its module's globals and never against an
# enclosing function's locals, so a self-reference written in a test body cannot
# resolve at all.
@dataclasses.dataclass
class TreeConfig:
    child: "Optional[TreeConfig]" = None
    depth: int = 0


@dataclasses.dataclass
class LeftConfig:
    right: "Optional[RightConfig]" = None


@dataclasses.dataclass
class RightConfig:
    left: "Optional[LeftConfig]" = None


# ---------------------------------------------------------------------------
# Declaration: which `__init__` signatures name a config class
# ---------------------------------------------------------------------------


def test_an_annotated_config_parameter_names_the_config_class():
    @node(execution="manual")
    class Blur:
        def __init__(self, config: BlurConfigDataclass) -> None:
            self.config = config

    assert Blur.__tatolab_node_config_class__ is BlurConfigDataclass


def test_an_init_taking_nothing_beyond_self_declares_no_config():
    @node(execution="manual")
    class Counter:
        def __init__(self) -> None:
            self.seen = 0

    assert Counter.__tatolab_node_config_class__ is None


def test_a_class_defining_no_init_at_all_declares_no_config():
    """`object.__init__` reports `(self, /, *args, **kwargs)`.

    Read literally that is a variadic signature, which the rule below refuses —
    so the commonest class in the suite would be refused for a signature its
    author never wrote.
    """

    @node(execution="manual")
    class Bare:
        pass

    assert Bare.__tatolab_node_config_class__ is None


def test_a_keyword_parameter_is_refused_with_the_fix_named():
    with pytest.raises(TypeError) as refusal:

        @node(execution="manual")
        class Blur:
            def __init__(self, width: int = 1) -> None:
                self.width = width

    assert "Blur" in str(refusal.value)
    assert "`width`" in str(refusal.value)
    assert "must be named `config`" in str(refusal.value)
    assert "BlurConfig" in str(refusal.value), "the fix must show the shape wanted"


def test_several_parameters_are_refused_and_all_of_them_named():
    with pytest.raises(TypeError, match="width, height"):

        @node(execution="manual")
        class Blur:
            def __init__(self, width: int, height: int) -> None:
                self.size = (width, height)


def test_an_unannotated_config_parameter_is_refused():
    with pytest.raises(TypeError, match="with no annotation"):

        @node(execution="manual")
        class Blur:
            def __init__(self, config) -> None:  # noqa: ANN001
                self.config = config


def test_a_config_annotated_as_a_parameterized_generic_is_refused():
    """`dict[str, Any]` is a mapping, not a class the helper can construct.

    The guard is on the annotation's origin rather than on `isinstance(_, type)`
    because on Python 3.10 — the package's floor — `isinstance(dict[str, Any],
    type)` is still True.
    """
    with pytest.raises(TypeError, match="is not a class"):

        @node(execution="manual")
        class Blur:
            def __init__(self, config: "dict[str, Any]") -> None:
                self.config = config


def test_keyword_variadic_configuration_is_refused_by_name():
    with pytest.raises(TypeError, match=r"\*\*config"):

        @node(execution="manual")
        class Blur:
            def __init__(self, **config: Any) -> None:
                self.config = config


def test_a_positional_only_config_is_refused_because_the_helper_passes_it_by_name():
    with pytest.raises(TypeError, match="positionally only"):

        @node(execution="manual")
        class Blur:
            def __init__(self, config: BlurConfigDataclass, /) -> None:
                self.config = config


def test_an_unresolvable_annotation_is_refused_where_the_author_can_see_it():
    with pytest.raises(TypeError, match="cannot be resolved"):

        @node(execution="manual")
        class Blur:
            def __init__(
                self,
                config: "NeverImported",  # noqa: F821  # pyright: ignore[reportUndefinedVariable]
            ) -> None:
                self.config = config


# ---------------------------------------------------------------------------
# Derivation: the document each kind of config class publishes
# ---------------------------------------------------------------------------


def schema_of(config_class: "Optional[type]") -> "dict[str, Any]":
    """The document a node taking `config_class` publishes."""
    if config_class is None:

        @node(execution="manual")
        class SubjectDeclaringNoConfig:
            def __init__(self) -> None:
                self.seen = 0

        return SubjectDeclaringNoConfig.__tatolab_node_config_schema__

    @node(execution="manual")
    class SubjectTakingAConfigClass:
        def __init__(self, config: config_class) -> None:  # type: ignore[valid-type]
            self.config = config

    return SubjectTakingAConfigClass.__tatolab_node_config_schema__


def test_a_typed_dict_yields_its_annotations_and_its_required_keys():
    document = schema_of(BlurConfigTypedDict)

    assert document["type"] == "object"
    assert document["properties"]["width"] == {"type": "integer"}
    assert document["properties"]["label"] == {
        "type": "string",
        "description": "What to call this blur.",
    }
    assert document["required"] == ["width", "label"]
    # A TypedDict admits an unknown key at run time, so claiming otherwise
    # would be a promise the class does not keep.
    assert "additionalProperties" not in document


def test_a_total_false_typed_dict_requires_nothing():
    assert "required" not in schema_of(PartialConfigTypedDict)


def test_a_dataclass_yields_its_init_fields_defaults_and_required_names():
    document = schema_of(BlurConfigDataclass)

    assert document["properties"]["width"] == {"type": "integer"}
    assert document["properties"]["label"] == {"type": "string", "default": "unlabelled"}
    assert document["properties"]["quality"] == {
        "enum": ["fast", "good"],
        "default": "fast",
    }
    assert document["properties"]["fallback"] == {
        "anyOf": [{"type": "string"}, {"type": "null"}],
        "default": None,
    }
    assert document["required"] == ["width"]
    # A dataclass raises on an unknown key, so the document may say so.
    assert document["additionalProperties"] is False


def test_a_field_with_a_default_factory_is_optional_and_carries_no_default():
    """A factory's result is not a default — calling one to document it would
    run the author's code at import."""
    tags = schema_of(BlurConfigDataclass)["properties"]["tags"]

    assert tags == {"type": "array", "items": {"type": "string"}}
    assert "tags" not in schema_of(BlurConfigDataclass)["required"]


def test_an_init_false_field_is_not_documented_because_it_is_not_an_input():
    assert "derived" not in schema_of(BlurConfigDataclass)["properties"]


def test_a_model_contributes_its_own_document_without_the_two_catalog_keys():
    document = schema_of(BlurConfigModel)

    assert document["properties"]["width"]["type"] == "integer"
    assert document["properties"]["label"]["default"] == "unlabelled"
    assert document["required"] == ["width"]
    assert "$schema" not in document
    assert "title" not in document, "the catalog names a node, not its config type"


def test_a_nested_config_class_is_inlined_rather_than_referenced():
    """Nothing here emits a `$ref`, so no document carries a `$defs` for one to
    point into."""
    document = schema_of(NestedConfig)

    assert document["properties"]["inner"]["properties"]["width"] == {"type": "integer"}
    assert "$defs" not in document
    assert "$ref" not in repr(document)


def test_an_annotation_the_deriver_does_not_know_renders_as_an_open_schema():
    """A config class is worth publishing long before every type in it is
    describable."""

    @dataclasses.dataclass
    class WithAnOpaqueField:
        handle: complex
        width: int = 2

    document = schema_of(WithAnOpaqueField)

    assert document["properties"]["handle"] == {}
    assert document["properties"]["width"] == {"type": "integer", "default": 2}
    assert document["required"] == ["handle"], "an opaque field is still an input"


def test_a_self_referential_config_class_stops_rather_than_exhausting_the_stack():
    """Inlining is the only nesting the deriver emits, so a cycle has no fixed
    point. Unrecognised, the walk runs out of stack at decoration — which is
    import time, where the traceback names typing internals and not the class."""
    document = schema_of(TreeConfig)

    assert document["properties"]["child"]["anyOf"] == [
        {"type": "object"},
        {"type": "null"},
    ]
    assert document["properties"]["depth"] == {"type": "integer", "default": 0}


def test_two_config_classes_that_reach_each_other_stop_at_the_second_pass():
    left = schema_of(LeftConfig)["properties"]["right"]["anyOf"][0]

    assert left["properties"]["left"]["anyOf"] == [{"type": "object"}, {"type": "null"}]


def test_the_same_class_nested_twice_without_a_cycle_is_inlined_both_times():
    """The guard is on an ancestry, not on a visited set: a diamond is not a
    cycle and must not be truncated."""

    @dataclasses.dataclass
    class LeafConfig:
        width: int = 1

    @dataclasses.dataclass
    class BranchConfig:
        first: LeafConfig = dataclasses.field(default_factory=LeafConfig)
        second: LeafConfig = dataclasses.field(default_factory=LeafConfig)

    document = schema_of(BranchConfig)

    assert document["properties"]["first"]["properties"]["width"]["type"] == "integer"
    assert document["properties"]["second"]["properties"]["width"]["type"] == "integer"


def test_a_frozen_slotted_dataclass_derives_like_any_other():
    @dataclasses.dataclass(frozen=True)
    class FrozenConfig:
        width: int = 1

    assert schema_of(FrozenConfig)["properties"]["width"] == {
        "type": "integer",
        "default": 1,
    }


def test_a_typed_dict_inheriting_another_carries_both_key_sets():
    class BaseConfig(TypedDict):
        width: int

    class DerivedConfig(BaseConfig, total=False):
        label: str

    document = schema_of(DerivedConfig)

    assert set(document["properties"]) == {"width", "label"}
    assert document["required"] == ["width"]


def test_a_node_declaring_no_config_publishes_what_rust_publishes():
    """One catalog reads one way whichever language declared the node."""
    assert schema_of(None) == {
        "type": "object",
        "description": "This node declares no configuration.",
        "additionalProperties": False,
    }


def test_the_document_is_2020_12_with_no_meta_schema_key():
    for config_class in (BlurConfigTypedDict, BlurConfigDataclass, BlurConfigModel):
        assert "$schema" not in schema_of(config_class), config_class


def test_a_typing_extensions_typed_dict_is_recognised_as_one():
    """`typing.is_typeddict` sees `typing.TypedDict` alone.

    On the 3.10 floor `Required` / `NotRequired` come from `typing_extensions`,
    so its spelling is the one an author reaches for — and unrecognised it
    would fall through to an open object with no keys and no refusal to say so.
    """
    typing_extensions = pytest.importorskip("typing_extensions")

    class ExtensionSpelledConfig(typing_extensions.TypedDict):  # pyright: ignore[reportGeneralTypeIssues]
        width: int

    document = schema_of(ExtensionSpelledConfig)

    assert document["properties"]["width"] == {"type": "integer"}
    assert document["required"] == ["width"]


def test_a_config_annotated_as_any_is_refused_the_same_on_every_version():
    """`isinstance(typing.Any, type)` is False on 3.10 and True on 3.11+, so
    without naming `Any` the rule would differ across the package's own range."""
    with pytest.raises(TypeError, match="`Any`"):

        @node(execution="manual")
        class Blur:
            def __init__(self, config: Any) -> None:
                self.config = config


# ---------------------------------------------------------------------------
# Shapes the deriver has to describe rather than drop
# ---------------------------------------------------------------------------


def test_a_requiredness_qualifier_does_not_hide_the_type_it_wraps():
    """`get_type_hints` keeps `Required` / `NotRequired`, and unrecognised they
    swallow the key's type — the one thing the catalog exists to publish."""

    class QualifiedConfig(TypedDict, total=False):
        width: Required[Annotated[int, "How wide."]]
        label: NotRequired[str]

    document = schema_of(QualifiedConfig)

    assert document["properties"]["width"] == {"type": "integer", "description": "How wide."}
    assert document["properties"]["label"] == {"type": "string"}
    assert document["required"] == ["width"]


def test_an_init_var_is_documented_because_it_is_a_constructor_input():
    """`dataclasses.fields()` omits an InitVar. Left out, it is absent from
    `properties` while `additionalProperties: false` forbids it — a catalog
    telling an agent that a required key is illegal."""

    @dataclasses.dataclass
    class SeededConfig:
        width: int
        seed: dataclasses.InitVar[int] = 3

        def __post_init__(self, seed: int) -> None:
            self.scaled_width = self.width * seed

    assert SeededConfig(width=2, seed=5).scaled_width == 10, "an InitVar is an input"
    document = schema_of(SeededConfig)

    assert document["properties"]["seed"] == {"type": "integer", "default": 3}
    assert "seed" not in document["required"]
    assert document["additionalProperties"] is False


def test_a_nested_models_pointers_are_followed_rather_than_left_dangling():
    """A model writes `#/$defs/...` pointers as the root. Inlined under a
    property they name a `$defs` the enclosing document does not have, so a
    reader resolves them against nothing."""

    class InnerModel(pydantic.BaseModel):
        depth: int = 1

    class OuterModel(pydantic.BaseModel):
        inner: InnerModel = InnerModel()

    @dataclasses.dataclass
    class HoldingAModel:
        model: OuterModel

    document = schema_of(HoldingAModel)
    nested = document["properties"]["model"]

    assert "$ref" not in repr(nested), f"a pointer survived into a nested document: {nested}"
    assert "$defs" not in nested
    assert nested["properties"]["inner"]["properties"]["depth"]["type"] == "integer"


def test_a_root_model_keeps_the_defs_it_wrote_for_itself():
    """As the root its pointers resolve, so its document is taken verbatim."""

    class InnerModel(pydantic.BaseModel):
        depth: int = 1

    class RootModel(pydantic.BaseModel):
        inner: InnerModel = InnerModel()

    document = schema_of(RootModel)

    assert document["properties"]["inner"]["$ref"] == "#/$defs/InnerModel"
    assert document["$defs"]["InnerModel"]["properties"]["depth"]["type"] == "integer"


@pytest.mark.parametrize(
    ("annotation", "default", "why"),
    [
        (float, float("inf"), "JSON has no infinity"),
        (float, float("nan"), "JSON has no NaN"),
        (int, 2**64, "msgpack carries no integer wider than 64 bits"),
    ],
)
def test_a_default_the_wire_cannot_carry_is_dropped_not_rewritten(
    annotation, default, why
):
    """The document crosses into Rust through the msgpack value tree, which
    turns a non-finite float into a null and refuses a wider integer outright —
    losing the whole declaration over one default the author can live without."""
    OddlyDefaultedConfig = dataclasses.make_dataclass(
        "OddlyDefaultedConfig",
        [("setting", annotation, dataclasses.field(default=default))],
    )

    document = schema_of(OddlyDefaultedConfig)

    assert "default" not in document["properties"]["setting"], why
    assert "setting" not in document.get("required", []), "it still has a default"


def test_a_variadic_positional_config_is_refused_by_name():
    with pytest.raises(TypeError, match=r"\*config"):

        @node(execution="manual")
        class Blur:
            def __init__(self, *config: Any) -> None:
                self.config = config


def test_a_dataclass_whose_constructor_takes_less_than_its_fields_documents_the_constructor():
    """`config_class(**configuration)` is what a configuration meets, so the
    constructor has the final say. Documenting a key it refuses is the same lie
    as omitting one it requires."""

    @dataclasses.dataclass(init=False)
    class NarrowerThanItsFieldsConfig:
        width: int = 1
        label: str = "x"

        def __init__(self, width: int = 1) -> None:
            self.width = width
            self.label = "derived"

    document = schema_of(NarrowerThanItsFieldsConfig)

    assert set(document["properties"]) == {"width"}
    with pytest.raises(TypeError, match="label"):
        NarrowerThanItsFieldsConfig(width=2, label="refused")  # pyright: ignore[reportCallIssue]


def test_a_dataclass_with_no_generated_constructor_documents_no_keys_at_all():
    @dataclasses.dataclass(init=False)
    class TakesNothingConfig:
        width: int = 1

    assert schema_of(TakesNothingConfig) == {
        "type": "object",
        "properties": {},
        "additionalProperties": False,
    }


def test_a_fixed_length_tuple_states_its_length_not_only_its_positions():
    """2020-12 reads `prefixItems` as what each position holds and nothing about
    how many there are, so on its own it validates a shorter or longer array.
    The Rust seam bounds its tuples the same way."""

    @dataclasses.dataclass
    class CroppedConfig:
        crop: "tuple[int, int, int, int]" = (0, 0, 0, 0)
        tail: "tuple[int, ...]" = ()

    document = schema_of(CroppedConfig)

    assert document["properties"]["crop"]["minItems"] == 4
    assert document["properties"]["crop"]["maxItems"] == 4
    assert len(document["properties"]["crop"]["prefixItems"]) == 4
    # A homogeneous tuple has no length to state.
    assert document["properties"]["tail"] == {
        "type": "array",
        "items": {"type": "integer"},
        "default": [],
    }
