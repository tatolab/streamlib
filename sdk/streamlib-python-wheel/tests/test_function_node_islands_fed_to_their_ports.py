# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""An island of function nodes fed to a port.

An island is read off a helper's own data plane as the node it feeds reads
it: one f64 sample on one channel, evaluated at the read.
"""

from __future__ import annotations

import ctypes
import struct
from dataclasses import dataclass
from typing import Any, Literal

import pytest

from tatolab.runtime import _engine
from tatolab.runtime._function_node_compilation import (
    FunctionNodeBodyRunAsPython,
    FunctionNodeDoesNotCompile,
    feed_each_input_port_from_its_function_node_island,
)
from tatolab.stream import (
    Asset,
    Color,
    PreviousValue,
    Rectangle,
    State,
    Stateful,
    Transform,
    Vector2,
    node,
)

from function_node_test_nodes import (
    WeightedSumCountingItsCalls,
    _EVALUATE_ENTRY_TYPE,
    a_rust_function_node_declaration,
    amplify,
    an_island_of_one_node,
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
    for function_node in (remap, lfo, amplify, WeightedSumCountingItsCalls)
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
    """A Rust function node's `x` declares no default, which a Python one always does."""
    island = an_island_of_lfo_then_remap_feeding("brightness")
    island["nodes"][1]["import_path"] = f"{__name__}:WeightedSumCountingItsCalls"
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
    for function_node in (measure_its_own_spelling, remap)
}


@node
def hand_on_once_it_has_built_an_object[PinValue](value: PinValue) -> PinValue:
    repr(object())
    return value


def test_a_body_run_as_python_hands_on_unread_what_another_node_of_its_island_gave():
    handing_on = FunctionNodeBodyRunAsPython(run_as_python(hand_on_once_it_has_built_an_object), "h", None)
    level = 0.25
    message = object()
    assert handing_on.evaluate([level], 0)[0] is level
    assert handing_on.evaluate([message], 1)[0] is message


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


@dataclass(frozen=True)
class EvaluationsSoFar:
    evaluations: float = 0.0


@node
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


@node
def build_an_object_then_answer_the_level_raising_on_a_negative_one(level: float = 0.5) -> float:
    repr(object())
    if level < 0.0:
        raise RuntimeError("broke on purpose")
    return level


def an_island_feeding_the_level_its_pin_sets(level: float) -> "dict[str, Any]":
    island = an_island_of_one_node(
        "level",
        f"{__name__}:build_an_object_then_answer_the_level_raising_on_a_negative_one",
        None,
        "beat-1",
    )
    island["nodes"][0]["pins"] = {"level": level}
    return island


def test_a_function_node_that_raises_leaves_its_port_reading_nothing_and_is_named_until_it_runs_cleanly():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_feeding_the_level_its_pin_sets(-1.0)],
        {
            f"{__name__}:build_an_object_then_answer_the_level_raising_on_a_negative_one": (
                build_an_object_then_answer_the_level_raising_on_a_negative_one
            )
        }.__getitem__,
    )

    assert data_plane.read_from_input_port("level") is None
    raising_because = _engine.why_each_function_node_its_islands_call_is_raising(
        link_data_access=data_plane
    )
    assert list(raising_because) == ["beat-1"]
    assert raising_because["beat-1"].startswith("raised RuntimeError: broke on purpose (")
    assert "test_function_node_islands_fed_to_their_ports.py, line " in raising_because["beat-1"]

    fed.feed_an_input_port_from_its_island(an_island_feeding_the_level_its_pin_sets(0.5))
    assert the_value_read_from(data_plane, "level") == 0.5
    assert _engine.why_each_function_node_its_islands_call_is_raising(link_data_access=data_plane) == {}

    fed.feed_an_input_port_from_its_island(an_island_feeding_the_level_its_pin_sets(-1.0))
    assert data_plane.read_from_input_port("level") is None
    fed.stop_feeding_an_input_port("level")
    assert _engine.why_each_function_node_its_islands_call_is_raising(link_data_access=data_plane) == {}


def test_a_port_fed_again_by_an_island_of_another_node_forgets_the_raise_of_the_node_it_no_longer_calls():
    raising_path = f"{__name__}:build_an_object_then_answer_the_level_raising_on_a_negative_one"
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_feeding_the_level_its_pin_sets(-1.0)],
        {
            raising_path: build_an_object_then_answer_the_level_raising_on_a_negative_one,
            **_FUNCTION_NODES_FED_AGAIN_BY_IMPORT_PATH,
        }.__getitem__,
    )
    assert data_plane.read_from_input_port("level") is None
    assert list(
        _engine.why_each_function_node_its_islands_call_is_raising(link_data_access=data_plane)
    ) == ["beat-1"]

    fed.feed_an_input_port_from_its_island(
        an_island_of_the_counter_feeding("level", then_remap_it=False)
    )

    assert _engine.why_each_function_node_its_islands_call_is_raising(link_data_access=data_plane) == {}


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
    import_path = f"{__name__}:WeightedSumCountingItsCalls"
    island = an_island_of_one_node("level", import_path, None, "weighted-1")
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["level"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane, [island], {import_path: WeightedSumCountingItsCalls}.__getitem__
    )
    assert data_plane.read_from_input_port("level") is None
    island["nodes"][0]["pins"] = {"x": 0.5}
    fed.feed_an_input_port_from_its_island(island)
    assert the_value_read_from(data_plane, "level") == 0.5 * 2.0 + 0.25


def test_a_pin_value_that_is_not_a_number_is_refused_by_name():
    island = an_island_of_one_node("level", f"{__name__}:remap", None, "remap-1")
    island["nodes"][0]["pins"] = {"x": "half"}
    with pytest.raises(FunctionNodeDoesNotCompile, match="pin on input `x` is 'half'"):
        feed_each_input_port_from_its_function_node_island(
            _engine.open_node_link_data_access_for_helper_process(),
            [island],
            _FUNCTION_NODES_BY_IMPORT_PATH.__getitem__,
        )


@node
def add_a_step(x: float = 0.0, step: float = 1.0) -> float:
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


@node
def spell_the_scaled_value(x: float = 4.0, gain: float = 10.0) -> float:
    return float(len(f"{x * gain:.1f}"))


def an_island_whose_gain_a_tally_drives(
    input_port: str, scaled_import_path: str, gain_port: str
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
                "config": None,
                "pins": {"x": 2.0},
                "links_into": [{"port": gain_port, "from": {"node": "tally-1", "port": "result"}}],
            },
        ],
    }


def test_a_linked_input_drives_a_node_run_as_python_in_place_of_its_pin():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["spelled"], [])
    scaled_import_path = f"{__name__}:spell_the_scaled_value"
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_whose_gain_a_tally_drives("spelled", scaled_import_path, "gain")],
        {f"{__name__}:tally": tally, scaled_import_path: spell_the_scaled_value}.__getitem__,
    )
    assert fed.ran_compiled_by_node_id == {"tally-1": True, "scaled-1": False}
    assert the_value_read_from(data_plane, "spelled") == float(len("6.0")), (
        "2 scaled by the tally's 3 spells 6.0, not 20.0"
    )


@node
def one_without_a_frame_and_two_with_one[PinValue](frame: PinValue | None = None) -> float:  # pyright: ignore[reportInvalidTypeVarUse]
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


@node
def place_a_tint(
    tint: Color = Color(1.0, 0.5, 0.25),
    spot: Vector2 = Vector2(0.5, 0.5),
    area: Rectangle = Rectangle(0.0, 0.0, 1.0, 1.0),
    move: Transform = Transform.identity(),
) -> Color:
    x = move.numbers[0] * spot.x + move.numbers[3]
    inside = area.x <= x <= area.x + area.width
    return tint if inside else Color(0.0, 0.0, 0.0, tint.alpha)


@node
def dim_a_tint(tint: Color = Color(0.0, 0.0, 0.0)) -> Color:
    return Color(tint.red / 2, tint.green / 2, tint.blue / 2, tint.alpha)


_FUNCTION_NODES_OF_PARTS_BY_IMPORT_PATH = {
    f"{__name__}:{function_node.__qualname__}": function_node
    for function_node in (place_a_tint, dim_a_tint)
}


def the_parts_read_from(data_plane: Any, input_port: str, part_count: int) -> "tuple[float, ...]":
    payload = data_plane.read_from_input_port(input_port)
    assert payload is not None
    return struct.unpack(f"<{part_count}d", bytes(payload))


def test_a_colour_vector_rectangle_and_transform_set_on_inputs_reach_the_body_as_set():
    import_path = f"{__name__}:place_a_tint"
    island = an_island_of_one_node("tint", import_path, None, "place-1")
    island["nodes"][0]["pins"] = {
        "tint": {"red": 0.2, "green": 0.4, "blue": 0.6, "alpha": 0.8},
        "spot": {"x": 0.25, "y": 0.0},
        "area": {"x": 0.0, "y": 0.0, "width": 0.5, "height": 1.0},
        "move": [1, 0, 0, 0.5, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1],
    }
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["tint"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_OF_PARTS_BY_IMPORT_PATH.__getitem__
    )
    assert fed.ran_compiled_by_node_id == {"place-1": False}, "a value of parts runs as Python"
    assert the_parts_read_from(data_plane, "tint", 4) == (0.0, 0.0, 0.0, 0.8), (
        "the spot, moved by 0.5, falls outside an area half the frame wide"
    )
    island["nodes"][0]["pins"]["move"] = Transform.identity().stored_value()
    fed.feed_an_input_port_from_its_island(island)
    assert the_parts_read_from(data_plane, "tint", 4) == (0.2, 0.4, 0.6, 0.8)


def test_an_input_of_parts_with_no_pin_reads_its_declared_default():
    import_path = f"{__name__}:place_a_tint"
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["tint"], [])
    feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_one_node("tint", import_path, None, "place-1")],
        _FUNCTION_NODES_OF_PARTS_BY_IMPORT_PATH.__getitem__,
    )
    assert the_parts_read_from(data_plane, "tint", 4) == (1.0, 0.5, 0.25, 1.0)


def test_a_colour_travels_from_one_node_to_the_next_unchanged():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["dimmed"], [])
    island = {
        "input_port": "dimmed",
        "link_id": "L-island",
        "output": {"node": "dim-1", "port": "result"},
        "nodes": [
            {
                "node": "place-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:place_a_tint",
                "config": None,
                "links_into": [],
            },
            {
                "node": "dim-1",
                "breadcrumb_number": 1,
                "import_path": f"{__name__}:dim_a_tint",
                "config": None,
                "links_into": [{"port": "tint", "from": {"node": "place-1", "port": "result"}}],
            },
        ],
    }
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_OF_PARTS_BY_IMPORT_PATH.__getitem__
    )
    assert the_parts_read_from(data_plane, "dimmed", 4) == (0.5, 0.25, 0.125, 1.0)


@pytest.mark.parametrize(
    ("pin", "value", "refused_because"),
    [
        ("tint", {"red": 1.0, "green": 0.5, "blue": 0.2}, "exactly red, green, blue, alpha"),
        ("spot", {"x": 1.0, "y": 2.0, "z": 3.0}, "exactly x, y"),
        ("area", {"x": 0.0, "y": 0.0, "width": -1.0, "height": 1.0}, "width is 0 or more"),
        ("move", [1.0, 0.0], "16 numbers"),
    ],
)
def test_a_pin_value_of_parts_of_the_wrong_shape_is_refused_by_name(
    pin: str, value: Any, refused_because: str
):
    island = an_island_of_one_node("tint", f"{__name__}:place_a_tint", None, "place-1")
    island["nodes"][0]["pins"] = {pin: value}
    with pytest.raises(FunctionNodeDoesNotCompile, match=f"pin on input `{pin}`.*{refused_because}"):
        feed_each_input_port_from_its_function_node_island(
            _engine.open_node_link_data_access_for_helper_process(),
            [island],
            _FUNCTION_NODES_OF_PARTS_BY_IMPORT_PATH.__getitem__,
        )


AN_ASSET_UUID = "0eaf9ee2-0f73-4154-a3f6-a99c261b850d"


@node
def describe_a_cue(
    title: str = "",
    shape: Literal["sine", "square"] = "sine",
    picture: Asset = Asset(AN_ASSET_UUID),
    levels: list[float] = [0.5],  # noqa: B006
) -> str:
    return f"{title}/{shape}/{picture.asset[:8]}/{sum(levels)}"


@node
def double_each_level(levels: list[float] = [1.0]) -> list[float]:  # noqa: B006
    return [level * 2 for level in levels]


@node
def total_the_levels(levels: list[float] = [0.0]) -> float:  # noqa: B006
    return sum(levels)


_FUNCTION_NODES_OF_STORED_VALUES_BY_IMPORT_PATH = {
    f"{__name__}:{function_node.__qualname__}": function_node
    for function_node in (describe_a_cue, double_each_level, total_the_levels)
}


def a_records_message_of_a_string(text: str) -> bytes:
    """`{"value": text}` as msgpack, written out by hand for a string of at most 255 bytes."""
    encoded = text.encode()
    assert len(encoded) <= 255
    length = bytes([0xA0 | len(encoded)]) if len(encoded) < 32 else b"\xd9" + bytes([len(encoded)])
    return b"\x81\xa5value" + length + encoded


def test_a_string_menu_asset_and_list_of_floats_set_on_inputs_reach_the_body_as_set():
    island = an_island_of_one_node("cue", f"{__name__}:describe_a_cue", None, "cue-1")
    island["nodes"][0]["pins"] = {
        "title": "intro to the show",
        "shape": "square",
        "picture": {"asset": "5f0c3b1e-27a4-4bd3-9b51-6f0d5bd0b7a2"},
        "levels": [0.25, 0.5],
    }
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["cue"], [])
    fed = feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_OF_STORED_VALUES_BY_IMPORT_PATH.__getitem__
    )
    assert fed.ran_compiled_by_node_id == {"cue-1": False}, "a String runs as Python"
    payload = data_plane.read_from_input_port("cue")
    assert payload is not None
    assert bytes(payload) == a_records_message_of_a_string("intro to the show/square/5f0c3b1e/0.75")


def test_an_input_of_a_stored_value_type_with_no_pin_reads_its_declared_default():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["cue"], [])
    feed_each_input_port_from_its_function_node_island(
        data_plane,
        [an_island_of_one_node("cue", f"{__name__}:describe_a_cue", None, "cue-1")],
        _FUNCTION_NODES_OF_STORED_VALUES_BY_IMPORT_PATH.__getitem__,
    )
    payload = data_plane.read_from_input_port("cue")
    assert payload is not None
    assert bytes(payload) == a_records_message_of_a_string(f"/sine/{AN_ASSET_UUID[:8]}/0.5")


def test_a_list_of_floats_travels_from_one_node_to_the_next_unchanged():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["total"], [])
    island = {
        "input_port": "total",
        "link_id": "L-island",
        "output": {"node": "total-1", "port": "result"},
        "nodes": [
            {
                "node": "double-1",
                "breadcrumb_number": 0,
                "import_path": f"{__name__}:double_each_level",
                "config": None,
                "links_into": [],
                "pins": {"levels": [0.25, 0.5, 4]},
            },
            {
                "node": "total-1",
                "breadcrumb_number": 1,
                "import_path": f"{__name__}:total_the_levels",
                "config": None,
                "links_into": [{"port": "levels", "from": {"node": "double-1", "port": "result"}}],
            },
        ],
    }
    feed_each_input_port_from_its_function_node_island(
        data_plane, [island], _FUNCTION_NODES_OF_STORED_VALUES_BY_IMPORT_PATH.__getitem__
    )
    payload = data_plane.read_from_input_port("total")
    assert payload is not None
    assert struct.unpack("<d", bytes(payload)) == (9.5,)


@pytest.mark.parametrize(
    ("pin", "value", "refused_because"),
    [
        ("shape", "saw", "it takes one of `sine`, `square`"),
        ("levels", ["loud"], "element 0 is 'loud', and it takes a number"),
        ("picture", AN_ASSET_UUID, 'a stored Asset is the object {"asset": "<uuid>"}'),
        ("title", 3, "it takes text"),
    ],
)
def test_a_pin_value_of_a_stored_value_type_its_input_does_not_take_is_refused_by_name(
    pin: str, value: Any, refused_because: str
):
    island = an_island_of_one_node("cue", f"{__name__}:describe_a_cue", None, "cue-1")
    island["nodes"][0]["pins"] = {pin: value}
    with pytest.raises(FunctionNodeDoesNotCompile, match=f"pin on input `{pin}`.*{refused_because}"):
        feed_each_input_port_from_its_function_node_island(
            _engine.open_node_link_data_access_for_helper_process(),
            [island],
            _FUNCTION_NODES_OF_STORED_VALUES_BY_IMPORT_PATH.__getitem__,
        )
