# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""An island of `@node.function` nodes fed to a port.

An island is read off a helper's own data plane as the node it feeds reads
it: one f64 sample on one channel, evaluated at the read.
"""

from __future__ import annotations

import ctypes
import struct
from dataclasses import dataclass
from typing import Any

import pytest

from tatolab.runtime import _engine
from tatolab.runtime._function_node_compilation import (
    FunctionNodeBodyRunAsPython,
    FunctionNodeDoesNotCompile,
    feed_each_input_port_from_its_function_node_island,
)
from tatolab.stream import PreviousValue, State, Stateful, UnreadMessage, node

from function_node_test_nodes import (
    WeightedSumCountingItsCalls,
    _EVALUATE_ENTRY_TYPE,
    a_rust_function_node_declaration,
    amplify,
    an_island_of_one_node,
    count_the_label_letters,
    label_the_level,
    lfo,
    measure_its_own_spelling,
    remap,
    remember_each_call_by_a_letter,
    run_as_python,
    tally,
)

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


_FUNCTION_NODES_BY_IMPORT_PATH = {
    f"{__name__}:{function_node.__qualname__}": function_node
    for function_node in (remap, lfo, amplify)
}


def an_island_of_lfo_then_remap_feeding(input_port: str) -> "dict[str, Any]":
    return {
        "input_port": input_port,
        "link_id": "L-island",
        "output": {"node": "remap-1", "port": "result"},
        "nodes": [
            {
                "node": "lfo-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:lfo",
                "config": None,
                "links_into": [],
            },
            {
                "node": "remap-1",
                "breadcrumb_number": 1,
                "import_path": f"{__name__}:remap",
                "config": None,
                "links_into": [{"port": "x", "from": {"node": "lfo-1", "port": "result"}}],
            },
        ],
    }


def test_an_island_feeds_its_port_one_f64_sample_evaluated_at_the_read():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["brightness"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_lfo_then_remap_feeding("brightness")],
        _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__,
    )
    assert len(fed.function_node_types_kept_alive) == 2
    assert fed.ran_compiled_by_node_id == {"lfo-1": True, "remap-1": True}
    assert data_plane.input_port_has_data("brightness")

    payload = data_plane.read_from_input_port("brightness")
    assert payload is not None
    (brightness,) = struct.unpack("<d", bytes(payload))
    assert 0.0 <= brightness <= 1.0


def test_an_island_with_an_input_no_value_reaches_holds_until_a_later_feed_gives_it_one():
    island = an_island_of_lfo_then_remap_feeding("brightness")
    linked_x = island["nodes"][1]["links_into"]
    island["nodes"][1]["links_into"] = []
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["brightness"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__
    )
    assert fed.ran_compiled_by_node_id == {}, "a held island runs nothing"
    assert data_plane.read_from_input_port("brightness") is None

    island["nodes"][1]["links_into"] = linked_x
    fed.feed_an_input_port_from_its_island(island)
    assert data_plane.read_from_input_port("brightness") is not None

    island["nodes"][1]["links_into"] = []
    fed.feed_an_input_port_from_its_island(island)
    assert data_plane.read_from_input_port("brightness") is None, (
        "an island fed again with an input no value reaches stops feeding its port"
    )


def test_a_stateful_node_feeding_two_inputs_of_one_reader_is_refused_by_name():
    with pytest.raises(FunctionNodeDoesNotCompile, match="stateful function node `lfo-1` feeds both"):
        feed_each_input_port_from_its_function_node_island(
            _engine.open_node_link_data_access_for_helper_process(),
            [
                an_island_of_lfo_then_remap_feeding("brightness"),
                an_island_of_lfo_then_remap_feeding("contrast"),
            ],
            _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__,
        )


def test_an_island_mixing_a_compiled_python_and_a_rust_function_node_feeds_its_port():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["weighted"], [])
    island = {
        "input_port": "weighted",
        "link_id": "L-mixed-island",
        "output": {"node": "weighted-1", "port": "result"},
        "nodes": [
            {
                "node": "lfo-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:lfo",
                "config": None,
                "links_into": [],
            },
            {
                "node": "tally-1",
                "breadcrumb_number": 1,
                "import_path": f"{__name__}:tally",
                "config": None,
                "links_into": [{"port": "x", "from": {"node": "lfo-1", "port": "result"}}],
            },
            {
                "node": "weighted-1",
                "breadcrumb_number": 2,
                "import_path": f"{__name__}:WeightedSumCountingItsCalls",
                "config": {"weight": 4.0},
                "links_into": [{"port": "x", "from": {"node": "tally-1", "port": "result"}}],
            },
        ],
    }
    function_nodes_by_import_path = {
        f"{__name__}:lfo": lfo,
        f"{__name__}:tally": tally,
        f"{__name__}:WeightedSumCountingItsCalls": WeightedSumCountingItsCalls,
    }
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], function_nodes_by_import_path.__getitem__
    )

    payload = data_plane.read_from_input_port("weighted")
    assert payload is not None
    (weighted,) = struct.unpack("<d", bytes(payload))
    assert weighted == pytest.approx(3 * 4.0 + 0.25), (
        "`tally` hands the Rust node 3, which it weights by its config's 4 and moves by its "
        "`bias` pin's 0.25"
    )


@_EVALUATE_ENTRY_TYPE
def _rust_reciprocal_evaluate_entry(
    input_values: Any, config_values: Any, state_values: Any, output_values: Any, now_ns: int
) -> int:
    hands_one_out = input_values[0] != 0.0
    output_values[0] = 1.0 / input_values[0] if hands_one_out else 0.0
    output_values[1] = 1.0 if hands_one_out else 0.0
    return 0


RustReciprocal = node.rust_function(
    a_rust_function_node_declaration(
        short_name="RustReciprocal",
        description="The reciprocal of `x`, or nothing at zero.",
        input_pins=[{"port": "x", "value_type": "float", "pin_default": 0.0, "takes_none": False}],
        output_ports=[{"name": "result", "value_type": "float", "may_hand_out_nothing": True}],
        config_fields=[],
        state_fields=[],
        state_parameter=None,
        config_parameter=None,
        evaluate_entry_address=ctypes.cast(_rust_reciprocal_evaluate_entry, ctypes.c_void_p).value,
    )
)


@pytest.mark.parametrize(("x", "read"), [(4.0, 0.25), (0.0, None)])
def test_an_island_output_a_rust_node_hands_out_nothing_on_reads_nothing(x: float, read: Any):
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["reciprocal"], [])
    island = {
        "input_port": "reciprocal",
        "link_id": f"L-rust-reciprocal-of-{x}",
        "output": {"node": "reciprocal-1", "port": "result"},
        "nodes": [
            {
                "node": "reciprocal-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:RustReciprocal",
                "config": None,
                "links_into": [],
                "pins": {"x": x},
            },
        ],
    }
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {f"{__name__}:RustReciprocal": RustReciprocal}.__getitem__
    )

    payload = data_plane.read_from_input_port("reciprocal")
    if read is None:
        assert payload is None, "an output that hands out nothing yields no message"
    else:
        assert payload is not None
        assert struct.unpack("<d", bytes(payload)) == (pytest.approx(read),)


_FUNCTION_NODES_RUN_AS_PYTHON_BY_IMPORT_PATH = {
    f"{__name__}:{function_node.__qualname__}": function_node
    for function_node in (measure_its_own_spelling, label_the_level, count_the_label_letters, remap)
}


@node.function
def hand_on_once_it_has_built_an_object(value: UnreadMessage) -> UnreadMessage:
    repr(object())
    return value


def test_a_body_run_as_python_hands_on_unread_a_number_or_a_dict_another_node_of_its_island_gave():
    handing_on = FunctionNodeBodyRunAsPython(run_as_python(hand_on_once_it_has_built_an_object), "h", None)
    level = 0.25
    record = {"label": "low"}
    assert handing_on.evaluate([level], 0)[0] is level
    assert handing_on.evaluate([record], 1)[0] is record


def an_island_of_label_then_count_feeding(input_port: str, output_node: str) -> "dict[str, Any]":
    return {
        "input_port": input_port,
        "link_id": f"L-{input_port}",
        "output": {"node": output_node, "port": "result"},
        "nodes": [
            {
                "node": "label-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:label_the_level",
                "config": None,
                "links_into": [],
            },
            {
                "node": "count-1",
                "breadcrumb_number": 1,
                "import_path": f"{__name__}:count_the_label_letters",
                "config": None,
                "links_into": [{"port": "record", "from": {"node": "label-1", "port": "result"}}],
            },
        ],
    }


def test_an_island_runs_a_body_that_does_not_compile_as_python_inside_the_read():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["spelled"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_one_node("spelled", f"{__name__}:measure_its_own_spelling", None, "spelling-1")],
        _FUNCTION_NODES_RUN_AS_PYTHON_BY_IMPORT_PATH.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"spelling-1": False}
    payload = data_plane.read_from_input_port("spelled")
    assert payload is not None
    assert struct.unpack("<d", bytes(payload)) == (3.0,)


def test_a_dict_passes_between_nodes_run_as_python_and_reaches_its_port_as_a_records_message():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["letters", "label"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [
            an_island_of_label_then_count_feeding("letters", "count-1"),
            an_island_of_label_then_count_feeding("label", "label-1"),
        ],
        _FUNCTION_NODES_RUN_AS_PYTHON_BY_IMPORT_PATH.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"label-1": False, "count-1": False}

    letters = data_plane.read_from_input_port("letters")
    assert letters is not None
    assert struct.unpack("<d", bytes(letters)) == (4.0,)
    label = data_plane.read_from_input_port("label")
    assert label is not None
    assert bytes(label) == b"\x81\xa5label\xa4high", "one msgpack map, as a Records message carries it"


@dataclass(frozen=True)
class EvaluationsSoFar:
    evaluations: float = 0.0


@node.function
def count_each_evaluation(
    step: float = 1.0, *, previous: State[EvaluationsSoFar]
) -> Stateful[float, EvaluationsSoFar]:
    evaluations = previous.evaluations + step
    return evaluations, EvaluationsSoFar(evaluations)


_FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH = {
    f"{__name__}:{function_node.__qualname__}": function_node
    for function_node in (count_each_evaluation, remember_each_call_by_a_letter, remap)
}


def an_island_of_the_counter_feeding(input_port: str, *, then_remap_it: bool) -> "dict[str, Any]":
    counter = {
        "node": "counter-1",
        "breadcrumb_number": 0,
        "import_path": f"{__name__}:count_each_evaluation",
        "config": None,
        "links_into": [],
    }
    if not then_remap_it:
        return {
            "input_port": input_port,
            "link_id": f"L-{input_port}",
            "output": {"node": "counter-1", "port": "result"},
            "nodes": [counter],
        }
    return {
        "input_port": input_port,
        "link_id": f"L-{input_port}",
        "output": {"node": "remap-1", "port": "result"},
        "nodes": [
            counter,
            {
                "node": "remap-1",
                "breadcrumb_number": 1,
                "import_path": f"{__name__}:remap",
                "config": None,
                "links_into": [{"port": "x", "from": {"node": "counter-1", "port": "result"}}],
            },
        ],
    }


def the_value_read_from(data_plane: Any, input_port: str) -> float:
    payload = data_plane.read_from_input_port(input_port)
    assert payload is not None
    return struct.unpack("<d", bytes(payload))[0]


def test_an_island_fed_again_to_its_port_carries_each_compiled_stateful_nodes_state_on():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["count"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_the_counter_feeding("count", then_remap_it=False)],
        _FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"counter-1": True}
    assert the_value_read_from(data_plane, "count") == 1.0
    assert the_value_read_from(data_plane, "count") == 2.0

    fed.feed_an_input_port_from_its_island(an_island_of_the_counter_feeding("count", then_remap_it=True))

    assert fed.ran_compiled_by_node_id == {"counter-1": True, "remap-1": True}
    assert the_value_read_from(data_plane, "count") == 3.0, "the counter resumes, read through remap"


def test_an_island_fed_again_keeps_the_body_of_each_node_run_as_python_and_its_state():
    import_path = f"{__name__}:remember_each_call_by_a_letter"
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["letters"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_one_node("letters", import_path, None, "letters-1")],
        _FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"letters-1": False}
    assert the_value_read_from(data_plane, "letters") == 1.0

    fed.feed_an_input_port_from_its_island(an_island_of_one_node("letters", import_path, None, "letters-1"))

    assert the_value_read_from(data_plane, "letters") == 2.0, "a state of any record carries on"


def test_a_port_an_island_stops_feeding_reads_nothing_and_names_no_node():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["count"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_the_counter_feeding("count", then_remap_it=False)],
        _FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH.__getitem__,
    )

    assert fed.stop_feeding_an_input_port("count") is True

    assert fed.ran_compiled_by_node_id == {}
    assert data_plane.read_from_input_port("count") is None
    assert fed.stop_feeding_an_input_port("count") is False


def test_a_live_feed_of_a_stateful_node_another_port_reads_is_refused_and_leaves_both_ports_as_they_were():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["count", "also_count"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_the_counter_feeding("count", then_remap_it=False)],
        _FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH.__getitem__,
    )

    with pytest.raises(FunctionNodeDoesNotCompile, match="stateful function node `counter-1` feeds both"):
        fed.feed_an_input_port_from_its_island(
            an_island_of_the_counter_feeding("also_count", then_remap_it=False)
        )

    assert fed.ran_compiled_by_node_id == {"counter-1": True}
    assert the_value_read_from(data_plane, "count") == 1.0
    assert data_plane.read_from_input_port("also_count") is None


def test_an_unlinked_input_takes_its_pin_value_before_its_default():
    import_path = f"{__name__}:remap"
    island = an_island_of_one_node("level", import_path, None, "remap-1")
    island["nodes"][0]["pins"] = {"x": 0.5, "high": 4.0}
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {import_path: remap}.__getitem__
    )
    assert the_value_read_from(data_plane, "level") == 2.0, "x pinned to 0.5 over 0..4"


def test_an_island_node_input_with_no_link_pin_or_default_holds_its_port():
    import_path = f"{__name__}:remap"
    island = an_island_of_one_node("level", import_path, None, "remap-1")
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {import_path: remap}.__getitem__
    )
    assert data_plane.read_from_input_port("level") is None
    island["nodes"][0]["pins"] = {"x": 0.5, "high": 4.0}
    fed.feed_an_input_port_from_its_island(island)
    assert the_value_read_from(data_plane, "level") == 2.0


def test_a_pin_value_that_is_not_a_number_is_refused_by_name():
    island = an_island_of_one_node("level", f"{__name__}:remap", None, "remap-1")
    island["nodes"][0]["pins"] = {"x": "half"}
    with pytest.raises(FunctionNodeDoesNotCompile, match="pin on input `x` is 'half'"):
        feed_each_input_port_from_its_function_node_island(
            _engine.open_node_link_data_access_for_helper_process(),
            [island],
            _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__,
        )


@node.function
def add_a_step(x: float, step: float = 1.0) -> float:
    return x + step


def test_a_node_reading_a_later_node_takes_its_output_from_the_last_evaluation():
    """`PreviousValue -> add_a_step -> PreviousValue`, the loop a `PreviousValue` node
    closes: it comes first and reads what `add_a_step` gave last time, so each read counts
    one further."""
    previous_value_path = "tatolab.stream:PreviousValue"
    add_path = f"{__name__}:add_a_step"
    island = {
        "input_port": "count",
        "link_id": "L-count",
        "output": {"node": "add-1", "port": "result"},
        "nodes": [
            {
                "node": "previous-value-1",
                "breadcrumb_number": 0,
                "import_path": previous_value_path,
                "config": None,
                "links_into": [{"port": "value", "from": {"node": "add-1", "port": "result"}}],
            },
            {
                "node": "add-1",
                "breadcrumb_number": 1,
                "import_path": add_path,
                "config": None,
                "links_into": [{"port": "x", "from": {"node": "previous-value-1", "port": "result"}}],
            },
        ],
    }
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["count"], [])
    feed_each_input_port_from_its_function_node_island(
        data_plane,
        [island],
        {previous_value_path: PreviousValue, add_path: add_a_step}.__getitem__,
    )
    assert [the_value_read_from(data_plane, "count") for _ in range(3)] == [1.0, 2.0, 3.0]


@dataclass(frozen=True)
class SpelledGainConfig:
    gain: float = 2.0


@node.function
def spell_the_scaled_value(x: float = 4.0, *, config: SpelledGainConfig) -> float:
    return float(len(f"{x * config.gain:.1f}"))


def an_island_whose_gain_a_tally_drives(
    input_port: str, scaled_import_path: str, scaled_pins: "dict[str, float]"
) -> "dict[str, Any]":
    return {
        "input_port": input_port,
        "link_id": f"L-{input_port}",
        "output": {"node": "scaled-1", "port": "result"},
        "nodes": [
            {
                "node": "tally-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:tally",
                "config": None,
                "pins": {"x": 0.0},
                "links_into": [],
            },
            {
                "node": "scaled-1",
                "breadcrumb_number": 1,
                "import_path": scaled_import_path,
                "config": {"gain": 10.0},
                "pins": scaled_pins,
                "exposed_config_fields": ["gain"],
                "links_into": [{"port": "gain", "from": {"node": "tally-1", "port": "result"}}],
            },
        ],
    }


def test_a_linked_exposed_config_field_drives_a_compiled_node_in_place_of_its_config():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["amplified"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_whose_gain_a_tally_drives("amplified", f"{__name__}:amplify", {"x": 2.0})],
        {f"{__name__}:tally": tally, f"{__name__}:amplify": amplify}.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"tally-1": True, "scaled-1": True}
    assert the_value_read_from(data_plane, "amplified") == 2.0 * 3.0, (
        "the tally's 3 is the gain, not the config's 10"
    )


def test_a_linked_exposed_config_field_drives_a_node_run_as_python_in_place_of_its_config():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["spelled"], [])
    scaled_import_path = f"{__name__}:spell_the_scaled_value"
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_whose_gain_a_tally_drives("spelled", scaled_import_path, {})],
        {f"{__name__}:tally": tally, scaled_import_path: spell_the_scaled_value}.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"tally-1": True, "scaled-1": False}
    assert the_value_read_from(data_plane, "spelled") == float(len("12.0")), (
        "4 scaled by the tally's 3 spells 12.0, not 40.0"
    )


def test_an_exposed_config_field_no_link_drives_reads_the_nodes_config():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["amplified"], [])
    island = an_island_whose_gain_a_tally_drives("amplified", f"{__name__}:amplify", {"x": 2.0})
    island["nodes"][1]["links_into"] = []
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {f"{__name__}:tally": tally, f"{__name__}:amplify": amplify}.__getitem__
    )
    assert the_value_read_from(data_plane, "amplified") == 2.0 * 10.0


@node.function
def one_without_a_frame_and_two_with_one(frame: "UnreadMessage | None" = None) -> float:
    return 1.0 if frame is None else 2.0


def test_an_unlinked_optional_message_input_takes_none_and_its_island_feeds_its_port():
    import_path = f"{__name__}:one_without_a_frame_and_two_with_one"
    island = an_island_of_one_node("level", import_path, None, "frame-counter-1")
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {import_path: one_without_a_frame_and_two_with_one}.__getitem__
    )
    assert the_value_read_from(data_plane, "level") == 1.0
