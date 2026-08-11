# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@processor` / `@input` / `@output` grammar, exercised without an engine.

Everything here is a pure declaration check — no runtime boots — so this is the
half of the authoring surface that stays honest on a machine with no GPU.
"""

import pytest

from streamlib import input, output, processor


def test_a_bare_decorator_needs_no_arguments_at_all():
    """The zero-ceremony bar: a filter declares its ports and nothing else.

    An input's delivery profile is part of declaring the port, not ceremony
    on top of it — there is no identity, no manifest, no schema to wrangle.
    """

    @processor
    class BrightnessFilter:
        @input(delivery_profile="latest")
        def frames_from_upstream(self) -> None: ...

        @output()
        def frames_to_downstream(self) -> None: ...

    assert BrightnessFilter.__streamlib_processor_type_reference__ == {
        "org": "app",
        "package": "local",
        "type": "BrightnessFilter",
    }
    assert BrightnessFilter.__streamlib_processor_execution__ == {
        "mode": "reactive",
        "interval_ms": 0,
    }


def test_the_method_name_is_the_port_name():
    """A port is named once — no string repeated between declaration and use."""

    @processor
    class Passthrough:
        @input(delivery_profile="every_sample")
        def frames_from_upstream(self) -> None: ...

        @output(description="the filtered frames")
        def frames_to_downstream(self) -> None: ...

    assert Passthrough.__streamlib_processor_input_ports__ == [
        {
            "name": "frames_from_upstream",
            "description": "",
            "delivery_profile": "every_sample",
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
        @input(name="video_in", delivery_profile="latest")
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
        input(schema="VideoFrame", delivery_profile="latest")  # type: ignore[call-arg]
    with pytest.raises(TypeError, match="unexpected keyword argument 'schema'"):
        output(schema="VideoFrame")  # type: ignore[call-arg]


def test_a_declared_port_carries_no_type_key_under_any_spelling():
    @processor
    class Untyped:
        @input(delivery_profile="latest")
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
            @input(name="frames", delivery_profile="latest")
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


def test_an_explicit_identity_is_parsed_into_its_three_segments():
    @processor("@tatolab/camera/Camera", execution="manual", scheduling="realtime")
    class Camera:
        @output()
        def frames_to_downstream(self) -> None: ...

    assert Camera.__streamlib_processor_type_reference__ == {
        "org": "tatolab",
        "package": "camera",
        "type": "Camera",
    }
    assert Camera.__streamlib_processor_scheduling_priority__ == "realtime"


@pytest.mark.parametrize(
    ("identity", "expected_message"),
    [
        ("@tatolab/camera/Camera@1.0.0", "version-free"),
        ("tatolab/camera/Camera", "three `/`-separated segments"),
        ("@tatolab/camera", "three `/`-separated segments"),
        ("@tatolab/camera/lowercase", "invalid type segment"),
        ("@Tatolab/camera/Camera", "invalid org segment"),
    ],
)
def test_a_malformed_identity_is_refused_with_its_reason(identity, expected_message):
    with pytest.raises(ValueError, match=expected_message):

        @processor(identity, execution="manual")
        class Camera:
            @output()
            def frames_to_downstream(self) -> None: ...


def test_a_class_name_that_cannot_be_an_identity_says_so():
    with pytest.raises(ValueError, match="must be PascalCase"):

        @processor
        class lowercase_name:
            @input(delivery_profile="latest")
            def frames_from_upstream(self) -> None: ...


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
            @input(delivery_profile="latest")
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
        @input(delivery_profile="latest")
        def frames_from_upstream(self) -> None: ...

        @output()
        def frames_to_downstream(self) -> None: ...

    @processor
    class AudioFilter(BaseFilter):
        @input(delivery_profile="every_sample")
        def frames_from_upstream(self) -> None: ...

    assert AudioFilter.__streamlib_processor_input_ports__ == [
        {
            "name": "frames_from_upstream",
            "description": "",
            "delivery_profile": "every_sample",
        }
    ]
    # The inherited output survives the subclass's redeclaration of the input.
    assert [port["name"] for port in AudioFilter.__streamlib_processor_output_ports__] == [
        "frames_to_downstream"
    ]
