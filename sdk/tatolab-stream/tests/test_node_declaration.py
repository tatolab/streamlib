# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""How a class node runs, the retired declaration forms and imports, and the audio window contract,
exercised without an engine."""

from __future__ import annotations

import dataclasses
import importlib
import pathlib
from typing import Annotated

import pytest

import tatolab.stream
from tatolab.stream import (
    AudioWindowContract,
    Input,
    InputPin,
    NodeDeclarationRefusal,
    OutputPin,
    StreamDelivery,
    Texture,
    node,
)
from tatolab.stream import _node_declaration
from tatolab.stream._retired_declarations import (
    RETIRED_DECLARATION_FINDING_CODE,
    RETIRED_IMPORTS,
    RetiredImportRefusal,
)


def test_a_filter_declares_its_pins_and_nothing_else():
    """The zero-ceremony bar: a node with an input reacts to it, with no keyword written."""

    @node(execution="reactive")
    def BrightnessFilter(
        *,
        frames_from_upstream: InputPin[Annotated[Texture, StreamDelivery("newest")]],
        frames_to_downstream: OutputPin[Texture],
    ) -> None:
        return None

    assert getattr(BrightnessFilter, "__tatolab_node_declared__") is True
    assert getattr(BrightnessFilter, "__tatolab_node_execution__") == {"mode": "reactive", "interval_ms": 0}


@pytest.mark.parametrize("retired_port_decorator_name", ["input", "output"])
def test_node_carries_no_port_decorator(retired_port_decorator_name: str):
    """A port is a pin, so `@node.input` fails where it is written."""
    with pytest.raises(AttributeError, match=retired_port_decorator_name):
        getattr(node, retired_port_decorator_name)
    assert retired_port_decorator_name not in tatolab.stream.__all__


def test_the_package_exports_node():
    assert "node" in tatolab.stream.__all__


@pytest.mark.parametrize("written_as", ["bare", "with keyword arguments"])
def test_node_process_is_refused_naming_the_node_written_as_one_function(written_as: str):
    """`@node.process` stays only as a name that answers with its replacement.

    Mental-revert: restoring the old reader declares the class and raises nothing.
    """
    with pytest.raises(NodeDeclarationRefusal) as refusal:
        if written_as == "bare":

            @node.process
            class BrightnessFilter:
                pass

        else:

            @node.process(execution="manual")
            class Camera:
                pass

    assert refusal.value.finding_code == RETIRED_DECLARATION_FINDING_CODE
    words = str(refusal.value)
    assert "`@node.process`, which is retired" in words
    assert "on a `def` whose keyword-only parameters are its typed pins" in words
    assert "`video: InputPin[Texture]`" in words
    assert "`execution`, `interval_ms`, `scheduling` and `paced_by_real_time` go on" in words
    assert words.startswith("`BrightnessFilter`" if written_as == "bare" else "A node")


@pytest.mark.parametrize("written_as", ["bare", "called"])
def test_node_function_is_refused_naming_bare_node(written_as: str):
    with pytest.raises(NodeDeclarationRefusal) as refusal:
        if written_as == "bare":

            @node.function
            def half(value: float = 0.0) -> float:
                return value / 2

        else:

            @node.function()
            def half(value: float = 0.0) -> float:
                return value / 2

    assert refusal.value.finding_code == RETIRED_DECLARATION_FINDING_CODE
    assert "`@node.function`, which is retired. To fix: bare `@node`" in str(refusal.value)


@pytest.mark.parametrize("retired", RETIRED_IMPORTS, ids=lambda retired: retired.name)
def test_each_retired_name_is_refused_on_import_with_its_replacement(retired):
    """`from tatolab.stream import Output` fails where it is written, saying what to write.

    Mental-revert: restoring the export imports the name and raises nothing.
    """
    with pytest.raises(RetiredImportRefusal) as refusal:
        exec(f"from tatolab.stream import {retired.name}", {})
    assert str(refusal.value) == (
        f"`tatolab.stream.{retired.name}` is retired. To fix: {retired.replaced_by}."
    )
    assert retired.name not in tatolab.stream.__all__


def test_a_name_the_package_never_had_is_an_attribute_error():
    with pytest.raises(AttributeError, match="no attribute 'NeverDeclared'"):
        importlib.import_module("tatolab.stream").NeverDeclared  # noqa: B018


def test_a_source_must_declare_its_execution_mode():
    """Reactive defaults only where reacting is possible.

    A node with no input has nothing to react to, so a silent reactive default would hand the
    author a node that never runs once.
    """
    with pytest.raises(ValueError, match="declares no input ports"):

        @node()
        def TestPatternSource(
            *,
            frames_to_downstream: OutputPin[Texture],
        ) -> None:
            return None


def test_a_source_that_declares_a_mode_is_accepted():
    @node(execution="continuous", interval_ms=33)
    def TestPatternSource(
        *,
        frames_to_downstream: OutputPin[Texture],
    ) -> None:
        return None

    assert getattr(TestPatternSource, "__tatolab_node_execution__") == {"mode": "continuous", "interval_ms": 33}


def test_a_scheduling_priority_is_stamped():
    @node(execution="manual", scheduling="realtime")
    def Camera(
        *,
        frames_to_downstream: OutputPin[Texture],
    ) -> None:
        return None

    assert getattr(Camera, "__tatolab_node_scheduling_priority__") == "realtime"


@pytest.mark.parametrize(
    ("keyword", "value", "expected_message"),
    [
        ("execution", "whenever", "invalid execution"),
        ("scheduling", "urgent", "invalid scheduling"),
    ],
)
def test_an_unknown_mode_or_priority_is_refused(keyword, value, expected_message):
    with pytest.raises(ValueError, match=expected_message):

        @node(**{keyword: value})
        def Filter(
            *,
            frames_from_upstream: InputPin[Texture],
        ) -> None:
            return None


def test_a_negative_interval_is_refused():
    with pytest.raises(ValueError, match="non-negative int"):

        @node(execution="continuous", interval_ms=-1)
        def TestPatternSource(
            *,
            frames_to_downstream: OutputPin[Texture],
        ) -> None:
            return None


@pytest.mark.parametrize("paced_by_real_time", [True, False])
def test_a_node_is_stamped_with_whether_it_is_paced_by_real_time(paced_by_real_time: bool):
    @node(execution="manual", paced_by_real_time=paced_by_real_time)
    def CameraSource(
        *,
        frames_to_downstream: OutputPin[Texture],
    ) -> None:
        return None

    stamp = getattr(CameraSource, _node_declaration.NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP)
    assert stamp is paced_by_real_time


@pytest.mark.parametrize("not_a_bool", [1, "yes", None])
def test_paced_by_real_time_other_than_a_bool_is_refused(not_a_bool: object):
    with pytest.raises(TypeError, match="invalid paced_by_real_time"):

        @node(execution="manual", paced_by_real_time=not_a_bool)  # type: ignore[arg-type]
        def CameraSource(
            *,
            frames_to_downstream: OutputPin[Texture],
        ) -> None:
            return None


def test_an_omitted_hop_defaults_to_the_window_size():
    """Contiguous, non-overlapping windows — the default an author gets."""
    contract = AudioWindowContract(
        sample_rate=16_000, channels=1, dtype="f32", window_size=400
    )

    assert contract.hop == 400


def test_a_hop_below_the_window_is_a_rolling_window_and_is_accepted():
    contract = AudioWindowContract(
        sample_rate=16_000, channels=1, dtype="f32", window_size=512, hop=160
    )

    assert contract.hop == 160


def test_a_hop_above_the_window_size_is_refused_naming_both_numbers():
    with pytest.raises(ValueError) as refusal:
        AudioWindowContract(
            sample_rate=16_000, channels=1, dtype="f32", window_size=512, hop=1024
        )

    assert "1024" in str(refusal.value) and "512" in str(refusal.value)


@pytest.mark.parametrize(
    "field_name", ["sample_rate", "channels", "window_size", "hop"]
)
@pytest.mark.parametrize("value", [0, -1])
def test_every_numeric_field_is_refused_at_zero_or_below_naming_the_field_and_the_value(
    field_name: str, value: int
):
    """Python's declaration path would otherwise carry either straight to the engine."""
    fields = {
        "sample_rate": 16_000,
        "channels": 1,
        "dtype": "f32",
        "window_size": 512,
        "hop": 512,
    }
    fields[field_name] = value

    with pytest.raises(ValueError) as refusal:
        AudioWindowContract(**fields)  # type: ignore[arg-type]

    assert field_name in str(refusal.value)
    assert str(value) in str(refusal.value)


def test_an_unknown_dtype_is_refused_listing_the_legal_values():
    with pytest.raises(ValueError) as refusal:
        AudioWindowContract(
            sample_rate=16_000, channels=1, dtype="i16", window_size=512
        )

    message = str(refusal.value)
    assert "i16" in message and "f32" in message and "f64" in message


def test_a_partial_contract_is_refused_naming_the_missing_fields():
    """All-or-nothing but for the count: the rest leave the engine guessing."""
    with pytest.raises(TypeError) as refusal:
        AudioWindowContract(sample_rate=16_000, window_size=512)  # type: ignore[call-arg]

    assert "dtype" in str(refusal.value)


def test_an_omitted_channel_count_follows_the_source():
    """The default a graph needs: a microphone added later must not require
    editing every consumer downstream of it."""
    contract = AudioWindowContract(sample_rate=48_000, dtype="f32", window_size=960)

    assert contract.channels is None
    assert contract._as_declaration()["channels"] == "source"


def test_a_declared_channel_count_still_renders_as_the_number_it_declared():
    contract = AudioWindowContract(
        sample_rate=48_000, dtype="f32", window_size=960, channels=1
    )

    assert contract.channels == 1
    assert contract._as_declaration()["channels"] == 1


@pytest.mark.parametrize("omitted", ["sample_rate", "dtype", "window_size"])
def test_every_value_but_the_channel_count_is_still_required(omitted: str):
    """Relaxing one field must not have relaxed the contract."""
    fields = {"sample_rate": 16_000, "dtype": "f32", "window_size": 512}
    del fields[omitted]

    with pytest.raises(TypeError) as refusal:
        AudioWindowContract(**fields)  # type: ignore[arg-type]

    assert omitted in str(refusal.value)


def test_a_declared_channel_count_of_zero_is_still_refused():
    """The relaxation is about a count nobody stated, never one stated wrong."""
    with pytest.raises(ValueError) as refusal:
        AudioWindowContract(
            sample_rate=16_000, dtype="f32", window_size=512, channels=0
        )

    assert "channels" in str(refusal.value) and "0" in str(refusal.value)


def test_the_contract_takes_no_positional_arguments():
    """Keyword-only, so a positional call fails loudly rather than binding a value to the wrong
    keyword."""
    with pytest.raises(TypeError):
        AudioWindowContract(16_000, "f32", 512)  # type: ignore[misc]


def test_a_contract_is_frozen_after_declaration():
    """A declaration a node could edit later is not a declaration."""
    contract = AudioWindowContract(
        sample_rate=16_000, channels=1, dtype="f32", window_size=512
    )

    with pytest.raises(dataclasses.FrozenInstanceError):
        contract.window_size = 1024  # type: ignore[misc]


def test_a_docstring_never_stands_in_for_a_description():
    """A docstring is the code's own reading; only `@node.meta`'s description reaches the catalog."""

    @node(execution="manual")
    def DescribedByItsDocstringAlone() -> None:
        """Read by whoever reads the code, never by the catalog."""
        return None

    assert getattr(DescribedByItsDocstringAlone, "__tatolab_node_description__") == ""


def test_the_description_keyword_is_served_exactly_as_written():
    markdown = "Blurs `video`.\n\n- **radius**: in pixels"

    @node.meta(description=markdown)
    @node(execution="manual")
    def DescribedInMarkdown() -> None:
        """The docstring, which the catalog never reads."""
        return None

    assert getattr(DescribedInMarkdown, "__tatolab_node_description__") == markdown


def test_a_description_that_is_not_a_string_is_refused():
    with pytest.raises(TypeError, match="never a path to read it from"):

        @node.meta(description=pathlib.Path("README.md"))  # type: ignore[arg-type]
        @node(execution="manual")
        def DescribedByAPath() -> None:
            return None


def test_a_class_node_has_the_category_it_declares_and_none_when_it_declares_none():
    @node(execution="reactive")
    def Uncategorized(
        *,
        frames_from_upstream: InputPin[Texture],
    ) -> None:
        return None

    @node.meta(category="Image effects")
    @node(execution="reactive")
    def Categorized(
        *,
        frames_from_upstream: InputPin[Texture],
    ) -> None:
        return None

    assert getattr(Uncategorized, "__tatolab_node_category__") is None
    assert getattr(Categorized, "__tatolab_node_category__") == "Image effects"
