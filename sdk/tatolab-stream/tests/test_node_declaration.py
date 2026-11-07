# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@node` grammar and its `Input` / `Output` port attributes, exercised without an engine.

Everything here is a pure declaration check — no runtime boots — so this is the
half of the authoring surface that stays honest on a machine with no GPU.
"""

import dataclasses
import inspect

import pytest

import tatolab.stream
from tatolab.stream import AudioWindowContract, Channels, Image, Input, Output, node

# Not `from tatolab.stream import ...`: the sentinel is on no public surface, so
# reaching the private module for it is what an author would have to do to
# reach the refusal below at all.
from tatolab.stream import _node_declaration
from tatolab.stream._node_declaration import AUDIO_WINDOW_MATCH_DEVICE


def test_a_bare_decorator_needs_no_arguments_at_all():
    """The zero-ceremony bar: a filter declares its ports and nothing else.

    An input's delivery profile is part of declaring the port, not ceremony
    on top of it — there is no identity, no manifest, no schema to wrangle.
    """

    @node
    class BrightnessFilter:
        frames_from_upstream = Input(Image, delivery="newest")

        frames_to_downstream = Output(Image)

    assert BrightnessFilter.__tatolab_node_declared__ is True
    assert BrightnessFilter.__tatolab_node_execution__ == {
        "mode": "reactive",
        "interval_ms": 0,
    }


@pytest.mark.parametrize("retired_port_decorator_name", ["input", "output"])
def test_node_carries_no_port_decorator(retired_port_decorator_name: str):
    """A port is an `Input` or `Output` attribute, so `@node.input` fails where it is written."""
    with pytest.raises(AttributeError, match=retired_port_decorator_name):
        getattr(node, retired_port_decorator_name)
    assert retired_port_decorator_name not in tatolab.stream.__all__


def test_a_port_declared_without_a_family_is_refused_at_import():
    with pytest.raises(TypeError):
        Input(delivery="newest")  # type: ignore[call-arg]
    with pytest.raises(TypeError):
        Output()  # type: ignore[call-arg]


def test_the_attribute_name_is_the_port_name():
    """A port is named once — no string repeated between declaration and use."""

    @node
    class Passthrough:
        frames_from_upstream = Input(Image, delivery="ordered")

        frames_to_downstream = Output(Image, description="the filtered frames")

    assert Passthrough.__tatolab_node_input_ports__ == [
        {
            "name": "frames_from_upstream",
            "description": "",
            "delivery_profile": "ordered",
            "family": {"family": "image"},
        }
    ]
    assert Passthrough.__tatolab_node_output_ports__ == [
        {
            "name": "frames_to_downstream",
            "description": "the filtered frames",
            "family": {"family": "image"},
        }
    ]


def test_a_port_declaration_takes_no_schema():
    """A port carries its family and nothing that names a type."""
    with pytest.raises(TypeError, match="unexpected keyword argument 'schema'"):
        Input(Image, schema="ImageFrame", delivery="newest")  # type: ignore[call-arg]
    with pytest.raises(TypeError, match="unexpected keyword argument 'schema'"):
        Output(Image, schema="ImageFrame")  # type: ignore[call-arg]


def test_a_declared_port_carries_no_type_key_under_any_spelling():
    @node
    class Untyped:
        frames_from_upstream = Input(Image, delivery="newest")

        frames_to_downstream = Output(Image)

    declared = (
        Untyped.__tatolab_node_input_ports__
        + Untyped.__tatolab_node_output_ports__
    )
    for port in declared:
        for key in ("schema", "data_type", "type", "schema_ident"):
            assert key not in port, f"port {port['name']!r} carries a type key {key!r}"


def test_a_declared_borrow_quota_rides_the_port_and_an_undeclared_one_is_absent():
    @node
    class HoldsFrames:
        frames = Input(Image, delivery="ordered", borrow_quota=4)

        audio = Input(Image, delivery="newest")

    assert HoldsFrames.__tatolab_node_input_ports__ == [
        {
            "name": "frames",
            "description": "",
            "delivery_profile": "ordered",
            "family": {"family": "image"},
            "borrow_quota": 4,
        },
        {
            "name": "audio",
            "description": "",
            "delivery_profile": "newest",
            "family": {"family": "image"},
        },
    ]


@pytest.mark.parametrize("borrow_quota", [-1, 1.5, True])
def test_a_borrow_quota_that_is_not_a_whole_count_is_refused_at_declaration(borrow_quota: object):
    with pytest.raises(ValueError, match="invalid borrow_quota"):
        Input(Image, delivery="ordered", borrow_quota=borrow_quota)  # type: ignore[arg-type]


def test_an_unknown_delivery_is_refused_at_declaration():
    with pytest.raises(ValueError, match="must declare `delivery=`"):
        Input(Image, delivery="eventually")


def test_an_input_port_without_a_delivery_is_refused():
    """There is no default, so the omission is refused where it is written."""
    with pytest.raises(ValueError, match="must declare `delivery=`"):
        Input(Image)


def test_an_output_port_needs_no_delivery_profile():
    """Delivery is the consuming port's policy — an output declaring none is correct."""

    @node(execution="manual")
    class Source:
        frames_to_downstream = Output(Image)

    assert Source.__tatolab_node_output_ports__ == [
        {"name": "frames_to_downstream", "description": "", "family": {"family": "image"}}
    ]


def test_a_port_is_declared_under_its_cast_name():
    """A port name is an exposed name, so it is cast like one — the author's
    attribute name is never refused for its spelling."""

    @node
    class CastPorts:
        Video = Input(Image, delivery="newest")

        Frames_Out = Output(Image)

    assert [port["name"] for port in CastPorts.__tatolab_node_input_ports__] == [
        "video"
    ]
    assert [port["name"] for port in CastPorts.__tatolab_node_output_ports__] == [
        "frames_out"
    ]


def test_two_ports_casting_alike_are_refused_naming_both():
    with pytest.raises(ValueError) as refusal:

        @node
        class CastClash:
            Video = Input(Image, delivery="newest")

            video = Output(Image)

    message = str(refusal.value)
    assert "'Video'" in message and "'video'" in message
    assert "both cast to 'video'" in message


def test_the_package_exports_node():
    # That no public name says the retired word is the runtime suite's
    # `test_tatolab_namespace.py`, which sweeps both packages.
    assert "node" in tatolab.stream.__all__


def test_the_node_decorator_names_its_class_parameter_node_class():
    assert next(iter(inspect.signature(node).parameters)) == "node_class"


def test_a_source_must_declare_its_execution_mode():
    """Reactive defaults only where reacting is possible.

    A node with no input port has nothing to react to, so a silent
    reactive default would hand the author a node that never runs once —
    the failure this refuses to produce.
    """
    with pytest.raises(ValueError, match="declares no input ports"):

        @node
        class TestPatternSource:
            frames_to_downstream = Output(Image)


def test_a_source_that_declares_a_mode_is_accepted():
    @node(execution="continuous", interval_ms=33)
    class TestPatternSource:
        frames_to_downstream = Output(Image)

    assert TestPatternSource.__tatolab_node_execution__ == {
        "mode": "continuous",
        "interval_ms": 33,
    }


def test_keyword_arguments_are_the_whole_grammar():
    @node(execution="manual", scheduling="realtime")
    class Camera:
        frames_to_downstream = Output(Image)

    assert Camera.__tatolab_node_declared__ is True
    assert Camera.__tatolab_node_scheduling_priority__ == "realtime"


@pytest.mark.parametrize(
    "identity",
    [
        "@tatolab/camera/Camera",
        "@tatolab/camera/Camera@1.0.0",
        "tatolab/camera/Camera",
        "@tatolab/camera",
    ],
)
def test_a_positional_identity_is_refused_naming_the_class_path_rule(identity: str):
    """Every spelling the deleted grammar accepted lands on one refusal.

    Mental-revert guard: restore the positional identity parameter and these
    declare cleanly instead of raising. The argument is deliberately the wrong
    type — the decorator's signature takes `type | None` — because the runtime
    refusal is what a caller without a type checker actually meets.
    """
    with pytest.raises(TypeError, match="takes no positional argument"):

        @node(identity, execution="manual")  # pyright: ignore[reportArgumentType]
        class Camera:
            frames_to_downstream = Output(Image)


def test_the_refusal_names_where_the_identity_actually_comes_from():
    with pytest.raises(TypeError) as refusal:

        @node("@tatolab/camera/Camera")  # pyright: ignore[reportArgumentType]
        class Camera:
            frames_to_downstream = Output(Image)

    message = str(refusal.value)
    assert "import path" in message
    assert "__module__" in message and "__qualname__" in message


def test_a_class_name_that_is_not_pascal_case_is_accepted():
    """Python does not enforce PascalCase, so neither does the decorator.

    The old grammar refused `lowercase_name` because it had to fit a
    `^[A-Z][A-Za-z0-9]*$` type segment. Nothing parses the class name now — it
    is read off `__name__` for the display-name default and passed through.
    """

    @node
    class lowercase_name:
        frames_from_upstream = Input(Image, delivery="newest")

    assert lowercase_name.__tatolab_node_declared__ is True


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
        class Filter:
            frames_from_upstream = Input(Image, delivery="newest")


def test_a_negative_interval_is_refused():
    with pytest.raises(ValueError, match="non-negative int"):

        @node(execution="continuous", interval_ms=-1)
        class TestPatternSource:
            frames_to_downstream = Output(Image)


@pytest.mark.parametrize("paced_by_real_time", [True, False])
def test_a_node_is_stamped_with_whether_it_is_paced_by_real_time(paced_by_real_time: bool):
    @node(execution="manual", paced_by_real_time=paced_by_real_time)
    class CameraSource:
        frames_to_downstream = Output(Image)

    stamp = getattr(CameraSource, _node_declaration.NODE_DECLARATION_PACED_BY_REAL_TIME_STAMP)
    assert stamp is paced_by_real_time


@pytest.mark.parametrize("not_a_bool", [1, "yes", None])
def test_paced_by_real_time_other_than_a_bool_is_refused(not_a_bool: object):
    with pytest.raises(TypeError, match="invalid paced_by_real_time"):

        @node(execution="manual", paced_by_real_time=not_a_bool)  # type: ignore[arg-type]
        class CameraSource:
            frames_to_downstream = Output(Image)


def test_ports_are_inherited_and_a_subclass_can_redeclare_one():
    @node
    class BaseFilter:
        frames_from_upstream = Input(Image, delivery="newest")

        frames_to_downstream = Output(Image)

    @node
    class AudioFilter(BaseFilter):
        frames_from_upstream = Input(Image, delivery="ordered")

    assert AudioFilter.__tatolab_node_input_ports__ == [
        {
            "name": "frames_from_upstream",
            "description": "",
            "delivery_profile": "ordered",
            "family": {"family": "image"},
        }
    ]
    # The inherited output survives the subclass's redeclaration of the input.
    assert [port["name"] for port in AudioFilter.__tatolab_node_output_ports__] == [
        "frames_to_downstream"
    ]


# ---- `audio_window`: the declaration, and every way it is refused ----


def test_an_audio_input_declares_its_window_contract():
    @node
    class WakeWordDetector:
        audio_from_microphone = Input(
            Channels,
            delivery="ordered",
            window=AudioWindowContract(
                sample_rate=16_000, channels=1, dtype="f32", window_size=512, hop=512
            ),
        )

    assert WakeWordDetector.__tatolab_node_input_ports__ == [
        {
            "name": "audio_from_microphone",
            "description": "",
            "delivery_profile": "ordered",
            "family": {"family": "channels"},
            "audio_window": {
                "resolved_from": "declaration",
                "sample_rate": 16_000,
                "channels": 1,
                "dtype": "f32",
                "window_size": 512,
                "hop": 512,
            },
        }
    ]


@pytest.mark.parametrize("delivery_profile", ["ordered", "newest"])
def test_the_device_matching_sentinel_is_refused_at_import(delivery_profile: str):
    """No Python node can resolve it, so the line that writes it never takes.

    Under either delivery profile: the sentinel is refused for what it is, not
    for the company it keeps, so the profile refusal never gets to speak first
    and send an author to fix the wrong knob.
    """
    with pytest.raises(TypeError) as refusal:

        @node(execution="manual")
        class Speaker:
            audio_from_upstream = Input(
                Channels,
                delivery=delivery_profile,
                window=AUDIO_WINDOW_MATCH_DEVICE,  # type: ignore[arg-type]
            )

    message = str(refusal.value)
    assert "audio" in message, message
    assert "AUDIO_WINDOW_MATCH_DEVICE" in message, message
    assert "helper" in message, message
    assert "setup()" in message, message
    assert "AudioWindowContract" in message, message


def test_the_device_matching_sentinel_is_on_no_public_surface():
    """The refusal above is the second guard; not being reachable is the first.

    The declaring module's own list counts: it is what a `import *` would take,
    so a name restored there is back on the surface whatever the package root
    re-exports.
    """
    for name in ("AUDIO_WINDOW_MATCH_DEVICE", "AudioWindowMatchDeviceSentinel"):
        assert name not in tatolab.stream.__all__, name
        assert not hasattr(tatolab.stream, name), name
        assert name not in _node_declaration.__all__, name


def test_a_port_declaring_no_contract_carries_no_audio_window_key():
    """The contract is opt-in: nothing about a contract-less port moves."""

    @node
    class Passthrough:
        frames_from_upstream = Input(Image, delivery="newest")

        frames_to_downstream = Output(Image)

    for port in (
        Passthrough.__tatolab_node_input_ports__
        + Passthrough.__tatolab_node_output_ports__
    ):
        assert "audio_window" not in port


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
    """Keyword-only, so a positional call fails loudly rather than binding a
    value to the wrong keyword."""
    with pytest.raises(TypeError):
        AudioWindowContract(16_000, "f32", 512)  # type: ignore[misc]


def test_a_contract_beside_a_skipping_delivery_profile_is_refused_naming_both_knobs():
    with pytest.raises(ValueError) as refusal:

        @node
        class Skipping:
            audio_from_microphone = Input(
                Channels,
                delivery="newest",
                window=AudioWindowContract(
                    sample_rate=16_000, channels=1, dtype="f32", window_size=512
                ),
            )

    message = str(refusal.value)
    assert "audio_window" in message and "newest" in message and "ordered" in message


def test_an_audio_window_that_is_not_a_contract_is_refused():
    with pytest.raises(TypeError, match="AudioWindowContract"):

        @node
        class Wrong:
            audio_from_microphone = Input(
                Channels, delivery="ordered", window={"window_size": 512}  # type: ignore[arg-type]
            )


def test_an_output_port_takes_no_window_contract():
    """A producer publishes what it has; only a consumer states what it needs.

    The contract handed over is a valid one, so what the refusal rejects is
    unambiguously the keyword and not the value behind it.
    """
    contract = AudioWindowContract(
        sample_rate=16_000, channels=1, dtype="f32", window_size=512
    )

    with pytest.raises(TypeError):
        Output(Channels, window=contract)  # type: ignore[call-arg]


def test_a_contract_is_frozen_after_declaration():
    """A declaration a node could edit later is not a declaration."""
    contract = AudioWindowContract(
        sample_rate=16_000, channels=1, dtype="f32", window_size=512
    )

    with pytest.raises(dataclasses.FrozenInstanceError):
        contract.window_size = 1024  # type: ignore[misc]


def test_a_node_with_no_description_is_described_by_its_docstring():
    """The text an author already wrote, rather than a second place to write it."""

    @node(execution="manual")
    class DescribedByItsDocstringAlone:
        """What a node with no description= keyword falls back to."""

    assert (
        DescribedByItsDocstringAlone.__tatolab_node_description__
        == "What a node with no description= keyword falls back to."
    )


def test_an_explicit_description_outranks_the_docstring():
    """The keyword is the deliberate one; the docstring is the fallback."""

    @node(execution="manual", description="The keyword wins")
    class DescribedByBothKeywordAndDocstring:
        """The docstring the keyword outranks."""

    assert (
        DescribedByBothKeywordAndDocstring.__tatolab_node_description__
        == "The keyword wins"
    )


def test_a_node_with_neither_is_described_by_the_empty_string():
    """Never `None`: the descriptor's description is a string."""

    @node(execution="manual")
    class DescribedByNothingAtAll:
        pass

    assert DescribedByNothingAtAll.__tatolab_node_description__ == ""
