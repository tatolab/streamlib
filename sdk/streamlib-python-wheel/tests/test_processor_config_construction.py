# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The hosting module constructs a node's config class, and reconfigures it.

What a helper process does on the compile thread: build the class `@node`
read off `__init__` from the config mapping `stream_builder.add` recorded on
the graph node, and hand `configure` the same kind of object later. None of it
boots an engine. How the class is declared and documented is the stream
suite's (`sdk/tatolab-stream/tests/test_node_config_class.py`).
"""

import dataclasses
from typing import Annotated, Any, Literal, Optional, TypedDict

import pydantic
import pytest

from tatolab.stream import node
from tatolab.runtime._processor_hosting import (
    apply_configuration,
    construct_processor_instance,
)

# ---------------------------------------------------------------------------
# The config classes the cases below are declared against
# ---------------------------------------------------------------------------


class BlurConfigTypedDict(TypedDict):
    """Admits anything at run time — the loosest dial an author can pick."""

    width: int
    label: Annotated[str, "What to call this blur."]


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


# ---------------------------------------------------------------------------
# Hosting: constructing the class from the mapping, and reconfiguring
# ---------------------------------------------------------------------------


@node(execution="manual")
class DataclassConfigured:
    def __init__(self, config: BlurConfigDataclass) -> None:
        self.config = config

    def configure(self, config: BlurConfigDataclass) -> None:
        self.config = config


@node(execution="manual")
class TypedDictConfigured:
    def __init__(self, config: BlurConfigTypedDict) -> None:
        self.config = config


@node(execution="manual")
class ModelConfigured:
    def __init__(self, config: BlurConfigModel) -> None:
        self.config = config


@node(execution="manual")
class Unconfigured:
    def __init__(self) -> None:
        self.seen = 0


def test_a_dataclass_config_reaches_the_processor_as_an_object():
    built = construct_processor_instance(
        DataclassConfigured, {"width": 4, "label": "left"}, None
    )

    assert built.config == BlurConfigDataclass(width=4, label="left")


def test_a_typed_dict_config_reaches_the_processor_as_the_mapping_itself():
    built = construct_processor_instance(TypedDictConfigured, {"width": 4}, None)

    assert built.config == {"width": 4}


def test_a_model_config_reaches_the_processor_validated():
    built = construct_processor_instance(ModelConfigured, {"width": "4"}, None)

    assert built.config.width == 4, "the model coerced it; the wheel added no opinion"


def test_whatever_the_config_class_raises_is_what_the_author_sees():
    """Construction is the only check the wheel performs: how strict it is is
    the author's choice of config class, the same dial `read(port, into=T)` is."""
    with pytest.raises(TypeError, match="bogus"):
        construct_processor_instance(DataclassConfigured, {"bogus": 1}, None)

    with pytest.raises(pydantic.ValidationError):
        construct_processor_instance(ModelConfigured, {"width": "wide"}, None)


def test_a_processor_declaring_no_config_refuses_a_non_empty_one_by_name():
    with pytest.raises(TypeError, match="`width` has nowhere to go"):
        construct_processor_instance(Unconfigured, {"width": 1}, None)


def test_a_processor_declaring_no_config_takes_an_empty_one():
    assert construct_processor_instance(Unconfigured, {}, None).seen == 0
    assert construct_processor_instance(Unconfigured, None, None).seen == 0


def test_reconfiguration_hands_configure_the_same_kind_of_object():
    built = construct_processor_instance(DataclassConfigured, {"width": 4}, None)

    apply_configuration(built, {"width": 9, "label": "right"})

    assert built.config == BlurConfigDataclass(width=9, label="right")


def test_a_processor_without_configure_is_refused_by_the_hook_it_needs():
    built = construct_processor_instance(TypedDictConfigured, {"width": 4}, None)

    with pytest.raises(TypeError, match=r"configure\(self, config\)"):
        apply_configuration(built, {"width": 9})


def test_a_configuration_that_is_not_a_mapping_is_refused_before_construction():
    with pytest.raises(TypeError, match="must be a dict"):
        construct_processor_instance(DataclassConfigured, ["width", 4], None)


# ---------------------------------------------------------------------------
# The migrated fixtures, guarded where CI can see them
# ---------------------------------------------------------------------------

# Every other test that runs these five is `requires_gpu` and so runs on the rig
# alone. Decoration is where a bad migration raises, so importing them here is
# what puts the migration in front of CI at all.
MIGRATED_FIXTURES = [
    ("capability_context_probes", "ConfigProbe", "ConfigProbeConfig"),
    ("helper_placement_processors", "ReportsItsOwnProcessSource", "ReportsItsOwnProcessSourceConfig"),
    ("helper_process_probes", "PassThroughProbe", "PassThroughProbeConfig"),
    ("single_processor_under_test", "ConfiguredScaler", "ConfiguredScalerConfig"),
    ("texture_ring_producer_probes", "TextureRingPublishingVideoSource", "TextureRingPublishingVideoSourceConfig"),
]


@pytest.mark.parametrize(
    ("module_name", "processor_name", "config_name"),
    MIGRATED_FIXTURES,
    ids=[processor_name for _, processor_name, _ in MIGRATED_FIXTURES],
)
def test_a_migrated_fixture_declares_the_config_class_beside_it(
    module_name, processor_name, config_name
):
    module = __import__(module_name)
    processor_class = getattr(module, processor_name)

    assert processor_class.__tatolab_node_config_class__ is getattr(
        module, config_name
    )


def test_the_live_mutation_fixture_written_as_a_source_string_still_declares():
    """`LiveAddedEffect` lives as a triple-quoted literal, so no import, no
    linter and no AST sweep reaches it — running it here is the only way a bad
    migration of it fails anywhere but on the rig."""
    from test_live_graph_mutation import LIVE_ADDED_EFFECT_SOURCE

    namespace: "dict[str, Any]" = {"__name__": "processors.live_added_effect"}
    exec(compile(LIVE_ADDED_EFFECT_SOURCE, "live_added_effect.py", "exec"), namespace)

    effect = namespace["LiveAddedEffect"]
    assert effect.__tatolab_node_config_class__ is namespace["LiveAddedEffectConfig"]
    assert effect.__tatolab_node_config_schema__["properties"]["marker"] == {
        "type": "string",
        "default": "LIVE_FRAME",
    }


# ---------------------------------------------------------------------------
# The rest of the construction contract
# ---------------------------------------------------------------------------


def test_the_helper_constructs_the_processor_by_the_config_keyword():
    """Positionally would work for every class in this suite and break the
    moment an author writes a keyword-only `config`."""
    constructed_with: "dict[str, Any]" = {}

    @node(execution="manual")
    class KeywordOnlyConfigured:
        def __init__(self, *, config: BlurConfigDataclass) -> None:
            constructed_with["config"] = config

    construct_processor_instance(KeywordOnlyConfigured, {"width": 2}, None)

    assert constructed_with["config"] == BlurConfigDataclass(width=2)


def test_reconfiguring_a_processor_that_declares_no_config_refuses_the_keys():
    @node(execution="manual")
    class UnconfiguredButReconfigurable:
        def __init__(self) -> None:
            self.configured_with: Any = "never"

        def configure(self, config: None) -> None:
            self.configured_with = config

    built = construct_processor_instance(UnconfiguredButReconfigurable, {}, None)

    with pytest.raises(TypeError, match="`width` has nowhere to go"):
        apply_configuration(built, {"width": 1})

    # An empty update is not a mistake, so it reaches the hook with the nothing
    # the class declared.
    apply_configuration(built, {})
    assert built.configured_with is None


def test_a_config_class_of_a_kind_the_deriver_cannot_read_is_accepted_and_open():
    """A plain annotated class constructs fine and describes nothing.

    Pinned rather than left to drift, because it is the one shape where the
    catalog goes quiet on a class that works: an agent reading this entry learns
    that configuration is a mapping and nothing about its keys. Whether such a
    class should instead be refused at decoration, or read off its `__init__`,
    is an open question for the owner — the plan says "any class constructible
    from the config's keys with annotated fields" while the change enumerates
    three kinds.
    """

    class PlainlyAnnotatedConfig:
        def __init__(self, width: int = 3, label: str = "x") -> None:
            self.width = width
            self.label = label

    @node(execution="manual")
    class PlainlyConfigured:
        def __init__(self, config: PlainlyAnnotatedConfig) -> None:
            self.config = config

    assert PlainlyConfigured.__tatolab_node_config_schema__ == {"type": "object"}
    built = construct_processor_instance(PlainlyConfigured, {"width": 9}, None)
    assert built.config.width == 9, "it constructs; only the description is missing"
