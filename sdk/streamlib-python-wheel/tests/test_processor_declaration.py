# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@processor` grammar, exercised without an engine.

Everything here is a pure declaration check — no runtime boots — so this is the
half of the authoring surface that stays honest on a machine with no GPU.
"""

import pytest

from streamlib import LinkInputDataPort, LinkOutputDataPort, processor


def test_a_bare_decorator_needs_no_arguments_at_all():
    """The zero-ceremony bar: a filter declares nothing but its ports."""

    @processor
    class BrightnessFilter:
        frames_from_upstream = LinkInputDataPort()
        frames_to_downstream = LinkOutputDataPort()

    assert BrightnessFilter.__streamlib_processor_type_reference__ == {
        "org": "app",
        "package": "local",
        "type": "BrightnessFilter",
    }
    assert BrightnessFilter.__streamlib_processor_execution__ == {
        "mode": "reactive",
        "interval_ms": 0,
    }


def test_the_attribute_name_is_the_port_name():
    """A port is named once — no string repeated between declaration and use."""

    @processor
    class Passthrough:
        frames_from_upstream = LinkInputDataPort(delivery_profile="every_sample")
        frames_to_downstream = LinkOutputDataPort(description="the filtered frames")

    assert Passthrough.__streamlib_processor_input_ports__ == [
        {
            "name": "frames_from_upstream",
            "description": "",
            "delivery_profile": "every_sample",
        }
    ]
    assert Passthrough.__streamlib_processor_output_ports__ == [
        {"name": "frames_to_downstream", "description": "the filtered frames"}
    ]


def test_a_source_must_declare_its_execution_mode():
    """Reactive defaults only where reacting is possible.

    A processor with no input port has nothing to react to, so a silent
    reactive default would hand the author a processor that never runs once —
    the failure this refuses to produce.
    """
    with pytest.raises(ValueError, match="declares no input ports"):

        @processor
        class TestPatternSource:
            frames_to_downstream = LinkOutputDataPort()


def test_a_source_that_declares_a_mode_is_accepted():
    @processor(execution="continuous", interval_ms=33)
    class TestPatternSource:
        frames_to_downstream = LinkOutputDataPort()

    assert TestPatternSource.__streamlib_processor_execution__ == {
        "mode": "continuous",
        "interval_ms": 33,
    }


def test_an_explicit_identity_is_parsed_into_its_three_segments():
    @processor("@tatolab/camera/Camera", execution="manual", scheduling="realtime")
    class Camera:
        frames_to_downstream = LinkOutputDataPort()

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
            frames_to_downstream = LinkOutputDataPort()


def test_a_class_name_that_cannot_be_an_identity_says_so():
    with pytest.raises(ValueError, match="must be PascalCase"):

        @processor
        class lowercase_name:
            frames_from_upstream = LinkInputDataPort()


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
            frames_from_upstream = LinkInputDataPort()


def test_an_unknown_delivery_profile_is_refused_at_declaration():
    with pytest.raises(ValueError, match="invalid delivery_profile"):
        LinkInputDataPort(delivery_profile="eventually")


def test_a_negative_interval_is_refused():
    with pytest.raises(ValueError, match="non-negative int"):

        @processor(execution="continuous", interval_ms=-1)
        class TestPatternSource:
            frames_to_downstream = LinkOutputDataPort()


def test_ports_are_inherited_and_a_subclass_can_redeclare_one():
    @processor
    class BaseFilter:
        frames_from_upstream = LinkInputDataPort(delivery_profile="latest")
        frames_to_downstream = LinkOutputDataPort()

    @processor
    class AudioFilter(BaseFilter):
        frames_from_upstream = LinkInputDataPort(delivery_profile="every_sample")

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


def test_an_unbound_port_explains_itself_rather_than_failing_silently():
    """Reading a port off a hand-instantiated processor is a mistake with a name."""

    @processor
    class Passthrough:
        frames_from_upstream = LinkInputDataPort()
        frames_to_downstream = LinkOutputDataPort()

    with pytest.raises(RuntimeError, match="not bound to a running processor"):
        Passthrough().frames_from_upstream.read()
    with pytest.raises(RuntimeError, match="not bound to a running processor"):
        Passthrough().frames_to_downstream.write({"frame": 1})
