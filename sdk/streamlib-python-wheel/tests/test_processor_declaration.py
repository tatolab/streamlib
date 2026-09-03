# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@processor` / `@input` / `@output` grammar, exercised without an engine.

Everything here is a pure declaration check — no runtime boots — so this is the
half of the authoring surface that stays honest on a machine with no GPU.
"""

import dataclasses

import pytest

import streamlib
from streamlib import AudioWindowContract, input, output, processor

# Not `from streamlib import ...`: the sentinel is on no public surface, so
# reaching the private module for it is what an author would have to do to
# reach the refusal below at all.
from streamlib import _processor_declaration
from streamlib._processor_declaration import AUDIO_WINDOW_MATCH_DEVICE


def test_a_bare_decorator_needs_no_arguments_at_all():
    """The zero-ceremony bar: a filter declares its ports and nothing else.

    An input's delivery profile is part of declaring the port, not ceremony
    on top of it — there is no identity, no manifest, no schema to wrangle.
    """

    @processor
    class BrightnessFilter:
        @input(delivery_profile="newest")
        def frames_from_upstream(self) -> None: ...

        @output()
        def frames_to_downstream(self) -> None: ...

    assert BrightnessFilter.__streamlib_processor_declared__ is True
    assert BrightnessFilter.__streamlib_processor_execution__ == {
        "mode": "reactive",
        "interval_ms": 0,
    }


def test_the_method_name_is_the_port_name():
    """A port is named once — no string repeated between declaration and use."""

    @processor
    class Passthrough:
        @input(delivery_profile="ordered")
        def frames_from_upstream(self) -> None: ...

        @output(description="the filtered frames")
        def frames_to_downstream(self) -> None: ...

    assert Passthrough.__streamlib_processor_input_ports__ == [
        {
            "name": "frames_from_upstream",
            "description": "",
            "delivery_profile": "ordered",
        }
    ]
    assert Passthrough.__streamlib_processor_output_ports__ == [
        {
            "name": "frames_to_downstream",
            "description": "the filtered frames",
        }
    ]


def test_an_explicit_name_overrides_the_method_name():
    @processor
    class Renamed:
        @input(name="video_in", delivery_profile="newest")
        def handle_incoming_video(self) -> None: ...

        @output(name="video_out")
        def handle_outgoing_video(self) -> None: ...

    assert [port["name"] for port in Renamed.__streamlib_processor_input_ports__] == [
        "video_in"
    ]
    assert [port["name"] for port in Renamed.__streamlib_processor_output_ports__] == [
        "video_out"
    ]


def test_a_port_declaration_takes_no_schema():
    """A port carries no type: `schema=` is gone, not tolerated-and-ignored.

    Type information belongs to the authoring language — the port method's
    return annotation — and never reaches the engine.
    """
    with pytest.raises(TypeError, match="unexpected keyword argument 'schema'"):
        input(schema="VideoFrame", delivery_profile="newest")  # type: ignore[call-arg]
    with pytest.raises(TypeError, match="unexpected keyword argument 'schema'"):
        output(schema="VideoFrame")  # type: ignore[call-arg]


def test_a_declared_port_carries_no_type_key_under_any_spelling():
    @processor
    class Untyped:
        @input(delivery_profile="newest")
        def frames_from_upstream(self) -> None: ...

        @output()
        def frames_to_downstream(self) -> None: ...

    declared = (
        Untyped.__streamlib_processor_input_ports__
        + Untyped.__streamlib_processor_output_ports__
    )
    for port in declared:
        for key in ("schema", "data_type", "type", "schema_ident"):
            assert key not in port, f"port {port['name']!r} carries a type key {key!r}"


def test_an_unknown_delivery_profile_is_refused_at_decoration():
    with pytest.raises(ValueError, match="invalid delivery_profile"):
        input(delivery_profile="eventually")


def test_an_input_port_without_a_delivery_profile_is_refused():
    """There is no default, so the omission is a wiring error naming the port."""
    with pytest.raises(ValueError, match="'frames_from_upstream' must declare a delivery_profile"):

        @processor
        class Unprofiled:
            @input()
            def frames_from_upstream(self) -> None: ...


def test_the_refusal_names_the_overriding_port_name():
    """`name=` renames the port, so the error must name that, not the method."""
    with pytest.raises(ValueError, match="'video_in' must declare a delivery_profile"):

        @processor
        class Unprofiled:
            @input(name="video_in")
            def frames_from_upstream(self) -> None: ...


def test_an_output_port_needs_no_delivery_profile():
    """Delivery is the consuming port's policy — an output declaring none is correct."""

    @processor(execution="manual")
    class Source:
        @output()
        def frames_to_downstream(self) -> None: ...

    assert Source.__streamlib_processor_output_ports__ == [
        {"name": "frames_to_downstream", "description": ""}
    ]


def test_a_duplicate_port_name_is_refused():
    with pytest.raises(ValueError, match="more than once"):

        @processor
        class Clashing:
            @input(name="frames", delivery_profile="newest")
            def frames_in(self) -> None: ...

            @output(name="frames")
            def frames_out(self) -> None: ...


def test_a_source_must_declare_its_execution_mode():
    """Reactive defaults only where reacting is possible.

    A processor with no input port has nothing to react to, so a silent
    reactive default would hand the author a processor that never runs once —
    the failure this refuses to produce.
    """
    with pytest.raises(ValueError, match="declares no input ports"):

        @processor
        class TestPatternSource:
            @output()
            def frames_to_downstream(self) -> None: ...


def test_a_source_that_declares_a_mode_is_accepted():
    @processor(execution="continuous", interval_ms=33)
    class TestPatternSource:
        @output()
        def frames_to_downstream(self) -> None: ...

    assert TestPatternSource.__streamlib_processor_execution__ == {
        "mode": "continuous",
        "interval_ms": 33,
    }


def test_keyword_arguments_are_the_whole_grammar():
    @processor(execution="manual", scheduling="realtime")
    class Camera:
        @output()
        def frames_to_downstream(self) -> None: ...

    assert Camera.__streamlib_processor_declared__ is True
    assert Camera.__streamlib_processor_scheduling_priority__ == "realtime"


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

        @processor(identity, execution="manual")  # pyright: ignore[reportArgumentType]
        class Camera:
            @output()
            def frames_to_downstream(self) -> None: ...


def test_the_refusal_names_where_the_identity_actually_comes_from():
    with pytest.raises(TypeError) as refusal:

        @processor("@tatolab/camera/Camera")  # pyright: ignore[reportArgumentType]
        class Camera:
            @output()
            def frames_to_downstream(self) -> None: ...

    message = str(refusal.value)
    assert "import path" in message
    assert "__module__" in message and "__qualname__" in message


def test_a_class_name_that_is_not_pascal_case_is_accepted():
    """Python does not enforce PascalCase, so neither does the decorator.

    The old grammar refused `lowercase_name` because it had to fit a
    `^[A-Z][A-Za-z0-9]*$` type segment. Nothing parses the class name now — it
    is read off `__name__` for the display-name default and passed through.
    """

    @processor
    class lowercase_name:
        @input(delivery_profile="newest")
        def frames_from_upstream(self) -> None: ...

    assert lowercase_name.__streamlib_processor_declared__ is True


@pytest.mark.parametrize(
    ("keyword", "value", "expected_message"),
    [
        ("execution", "whenever", "invalid execution"),
        ("scheduling", "urgent", "invalid scheduling"),
    ],
)
def test_an_unknown_mode_or_priority_is_refused(keyword, value, expected_message):
    with pytest.raises(ValueError, match=expected_message):

        @processor(**{keyword: value})
        class Filter:
            @input(delivery_profile="newest")
            def frames_from_upstream(self) -> None: ...


def test_a_negative_interval_is_refused():
    with pytest.raises(ValueError, match="non-negative int"):

        @processor(execution="continuous", interval_ms=-1)
        class TestPatternSource:
            @output()
            def frames_to_downstream(self) -> None: ...


def test_ports_are_inherited_and_a_subclass_can_redeclare_one():
    @processor
    class BaseFilter:
        @input(delivery_profile="newest")
        def frames_from_upstream(self) -> None: ...

        @output()
        def frames_to_downstream(self) -> None: ...

    @processor
    class AudioFilter(BaseFilter):
        @input(delivery_profile="ordered")
        def frames_from_upstream(self) -> None: ...

    assert AudioFilter.__streamlib_processor_input_ports__ == [
        {
            "name": "frames_from_upstream",
            "description": "",
            "delivery_profile": "ordered",
        }
    ]
    # The inherited output survives the subclass's redeclaration of the input.
    assert [port["name"] for port in AudioFilter.__streamlib_processor_output_ports__] == [
        "frames_to_downstream"
    ]


# ---- `audio_window`: the declaration, and every way it is refused ----


def test_an_audio_input_declares_its_window_contract():
    @processor
    class WakeWordDetector:
        @input(
            "audio",
            delivery_profile="ordered",
            audio_window=AudioWindowContract(
                sample_rate=16_000, channels=1, dtype="f32", window_size=512, hop=512
            ),
        )
        def audio_from_microphone(self) -> None: ...

    assert WakeWordDetector.__streamlib_processor_input_ports__ == [
        {
            "name": "audio",
            "description": "",
            "delivery_profile": "ordered",
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
def test_the_device_matching_sentinel_is_refused_at_decoration(delivery_profile: str):
    """No Python processor can resolve it, so the line that writes it never takes.

    Under either delivery profile: the sentinel is refused for what it is, not
    for the company it keeps, so the profile refusal never gets to speak first
    and send an author to fix the wrong knob.
    """
    with pytest.raises(TypeError) as refusal:

        @processor(execution="manual")
        class Speaker:
            @input(
                "audio",
                delivery_profile=delivery_profile,
                audio_window=AUDIO_WINDOW_MATCH_DEVICE,  # type: ignore[arg-type]
            )
            def audio_from_upstream(self) -> None: ...

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
        assert name not in streamlib.__all__, name
        assert not hasattr(streamlib, name), name
        assert name not in _processor_declaration.__all__, name


def test_a_port_declaring_no_contract_carries_no_audio_window_key():
    """The contract is opt-in: nothing about a contract-less port moves."""

    @processor
    class Passthrough:
        @input(delivery_profile="newest")
        def frames_from_upstream(self) -> None: ...

        @output()
        def frames_to_downstream(self) -> None: ...

    for port in (
        Passthrough.__streamlib_processor_input_ports__
        + Passthrough.__streamlib_processor_output_ports__
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
            sample_rate=16_000, channels=1, dtype="f64", window_size=512
        )

    message = str(refusal.value)
    assert "f64" in message and "f32" in message and "i16" in message


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

        @processor
        class Skipping:
            @input(
                "audio",
                delivery_profile="newest",
                audio_window=AudioWindowContract(
                    sample_rate=16_000, channels=1, dtype="f32", window_size=512
                ),
            )
            def audio_from_microphone(self) -> None: ...

    message = str(refusal.value)
    assert "audio_window" in message and "newest" in message and "ordered" in message


def test_an_audio_window_that_is_not_a_contract_is_refused():
    with pytest.raises(TypeError, match="AudioWindowContract"):

        @processor
        class Wrong:
            @input("audio", delivery_profile="ordered", audio_window={"window_size": 512})  # type: ignore[arg-type]
            def audio_from_microphone(self) -> None: ...


def test_an_output_port_takes_no_window_contract():
    """A producer publishes what it has; only a consumer states what it needs.

    The contract handed over is a valid one, so what the refusal rejects is
    unambiguously the keyword and not the value behind it.
    """
    contract = AudioWindowContract(
        sample_rate=16_000, channels=1, dtype="f32", window_size=512
    )

    with pytest.raises(TypeError):
        output("audio_out", audio_window=contract)  # type: ignore[call-arg]


def test_a_contract_is_frozen_after_declaration():
    """A declaration a processor could edit later is not a declaration."""
    contract = AudioWindowContract(
        sample_rate=16_000, channels=1, dtype="f32", window_size=512
    )

    with pytest.raises(dataclasses.FrozenInstanceError):
        contract.window_size = 1024  # type: ignore[misc]
