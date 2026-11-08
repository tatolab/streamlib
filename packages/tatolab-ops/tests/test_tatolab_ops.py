# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Each node of `tatolab.ops` called as the plain function it is, then compiled and held to it."""

from __future__ import annotations

import ctypes
import dataclasses
import math
from typing import Any

import pytest

from tatolab.ops import (
    Absolute,
    Add,
    Clamp,
    Compare,
    Comparison,
    Constant,
    Counter,
    CounterCount,
    Divide,
    Feedback,
    Lag,
    LagValue,
    Lerp,
    Lfo,
    LfoPhase,
    Maximum,
    Minimum,
    Modulo,
    Multiply,
    PerlinNoise,
    Power,
    Random,
    Remap,
    SampleAndHold,
    SampledValue,
    Select,
    Smoothstep,
    Step,
    Subtract,
    Switch,
    Timer,
    TimerStart,
    Trigger,
    TriggerLevel,
)


def test_lfo_starts_at_zero_and_advances_by_the_stream_time_between_reads():
    first, phase = Lfo(1.0, now=5_000_000_000, previous=LfoPhase())
    assert first == 0.0 and phase == LfoPhase(0.0, 5_000_000_000, True)
    quarter, phase = Lfo(1.0, now=5_250_000_000, previous=phase)
    assert quarter == pytest.approx(1.0)
    assert phase.cycles == pytest.approx(0.25)


def test_remap_maps_one_range_onto_another():
    assert Remap(0.0) == 0.5
    assert Remap(5.0, from_low=0.0, from_high=10.0, to_low=100.0, to_high=200.0) == 150.0
    assert Remap(3.0, from_low=1.0, from_high=1.0, to_low=7.0) == 7.0


def test_clamp_holds_a_value_between_its_bounds():
    assert Clamp(-1.0) == 0.0
    assert Clamp(0.25) == 0.25
    assert Clamp(4.0, low=1.0, high=2.0) == 2.0


def test_feedback_called_as_a_function_passes_its_value_on():
    assert Feedback() == 0.0
    assert Feedback(0.75) == 0.75


def test_the_math_nodes_answer_what_their_names_say():
    assert Add(2.0, 3.0) == 5.0
    assert Subtract(2.0, 3.0) == -1.0
    assert Multiply(2.0, 3.0) == 6.0
    assert Divide(3.0, 2.0) == 1.5
    assert Divide(3.0, 0.0) == 0.0, "a division by zero answers 0"
    assert Power(2.0, 3.0) == 8.0
    assert Power(-8.0, 0.5) == 0.0, "a power that is no real number answers 0"
    assert Power(0.0, -1.0) == 0.0
    assert Minimum(2.0, 3.0) == 2.0
    assert Maximum(2.0, 3.0) == 3.0
    assert Absolute(-2.5) == 2.5
    assert Modulo(7.0, 3.0) == 1.0
    assert Modulo(-1.0, 3.0) == 2.0, "the remainder takes the divisor's sign"
    assert Modulo(7.0, 0.0) == 0.0


def test_the_shaping_nodes_answer_what_their_names_say():
    assert Lerp(10.0, 20.0, 0.25) == 12.5
    assert Smoothstep(-1.0) == 0.0
    assert Smoothstep(0.5) == 0.5
    assert Smoothstep(2.0) == 1.0
    assert Smoothstep(0.25, edge_low=0.5, edge_high=0.5) == 0.0
    assert Step(-0.5) == 0.0
    assert Step(0.0) == 1.0


def test_the_logic_nodes_answer_what_their_names_say():
    assert Compare(1.0, 2.0) == Comparison(less=True, equal=False, greater=False)
    assert Compare(2.0, 2.05, tolerance=0.1) == Comparison(less=False, equal=True, greater=False)
    assert Compare(3.0, 2.0) == Comparison(less=False, equal=False, greater=True)
    assert Switch(True, 4.0, 5.0) == 4.0
    assert Switch(False, 4.0, 5.0) == 5.0
    assert [Select(index, 10.0, 11.0, 12.0, 13.0) for index in (-1, 0, 1, 2, 3, 9)] == [
        10.0,
        10.0,
        11.0,
        12.0,
        13.0,
        13.0,
    ]


def test_constant_random_and_perlin_noise_hand_out_what_they_say():
    assert Constant(0.3) == 0.3
    draws = [Random(now=now) for now in range(0, 2_000_000, 1_000)]
    assert all(0.0 <= draw < 1.0 for draw in draws)
    assert len(set(draws)) > 1_900, "each stream time draws afresh"
    assert Random(now=12_345) == Random(now=12_345), "a time draws the same each time"
    assert Random(now=12_345) != Random(seed=1, now=12_345), "a seed draws its own values"
    assert 5.0 <= Random(5.0, 6.0, now=99) < 6.0
    assert PerlinNoise(3.0) == 0.0, "noise is 0 at each whole number"
    samples = [PerlinNoise(x / 100.0) for x in range(-500, 500)]
    assert all(-1.0 <= sample <= 1.0 for sample in samples)
    assert max(abs(after - before) for before, after in zip(samples, samples[1:])) < 0.1, (
        "noise is smooth along x"
    )


def test_timer_counts_seconds_of_stream_time_from_its_start_or_its_reset():
    started, start = Timer(now=1_000_000_000, previous=TimerStart())
    assert started == 0.0
    later, start = Timer(now=3_500_000_000, previous=start)
    assert later == pytest.approx(2.5)
    reset, start = Timer(True, now=4_000_000_000, previous=start)
    assert reset == 0.0
    after_the_reset, _ = Timer(now=4_250_000_000, previous=start)
    assert after_the_reset == pytest.approx(0.25)


def test_lag_starts_at_its_input_and_closes_on_it_over_its_seconds():
    first, lagged = Lag(1.0, 0.1, now=0, previous=LagValue())
    assert first == 1.0
    after_one_time_constant, _ = Lag(0.0, 0.1, now=100_000_000, previous=lagged)
    assert after_one_time_constant == pytest.approx(math.exp(-1.0))


def test_trigger_is_on_for_the_one_evaluation_its_input_turns_on():
    level = TriggerLevel()
    fired = []
    for x in (False, True, True, False, True):
        on, level = Trigger(x, previous=level)
        fired.append(on)
    assert fired == [False, True, False, False, True]


def test_counter_counts_each_turn_on_of_its_increment_until_a_reset():
    count = CounterCount()
    counts = []
    for increment, reset in (
        (True, False),
        (True, False),
        (False, False),
        (True, False),
        (False, True),
    ):
        counted, count = Counter(increment, reset, previous=count)
        counts.append(counted)
    assert counts == [1, 1, 1, 2, 0]


def test_sample_and_hold_takes_its_input_as_its_sample_turns_on():
    sampled = SampledValue()
    held = []
    for x, sample in ((0.5, False), (0.5, True), (0.9, True), (0.9, False), (0.2, True)):
        output, sampled = SampleAndHold(x, sample, previous=sampled)
        held.append(output)
    assert held == [0.0, 0.5, 0.5, 0.5, 0.2]


# Each node with the calls it is held to compiled: its inputs by parameter, the
# stream time, and the state it is called with.
_CALLS_EACH_NODE_IS_HELD_TO_COMPILED: "list[tuple[Any, list[dict[str, Any]]]]" = [
    (Absolute, [{"x": -2.5}, {"x": 3.0}]),
    (Add, [{"a": 2.0, "b": 3.0}]),
    (Subtract, [{"a": 2.0, "b": 3.5}]),
    (Multiply, [{"a": 2.0, "b": -3.0}]),
    (Divide, [{"a": 3.0, "b": 2.0}, {"a": 3.0, "b": 0.0}]),
    (
        Power,
        [
            {"base": 2.0, "exponent": 0.5},
            {"base": -8.0, "exponent": 0.5},
            {"base": 0.0, "exponent": -1.0},
        ],
    ),
    (Minimum, [{"a": 2.0, "b": 3.0}]),
    (Maximum, [{"a": 2.0, "b": 3.0}]),
    (Modulo, [{"a": 7.5, "b": 2.0}, {"a": -1.0, "b": 3.0}, {"a": 1.0, "b": 0.0}]),
    (Lerp, [{"a": 10.0, "b": 20.0, "t": 0.25}]),
    (Clamp, [{"x": -1.0, "low": 0.0, "high": 1.0}, {"x": 0.4, "low": 0.0, "high": 1.0}]),
    (Remap, [{"x": 0.0, "from_low": -1.0, "from_high": 1.0, "to_low": 0.0, "to_high": 1.0}]),
    (
        Smoothstep,
        [
            {"x": 0.3, "edge_low": 0.0, "edge_high": 1.0},
            {"x": 0.3, "edge_low": 0.5, "edge_high": 0.5},
        ],
    ),
    (Step, [{"x": -0.5, "edge": 0.0}, {"x": 0.5, "edge": 0.0}]),
    (Compare, [{"a": 1.0, "b": 2.0, "tolerance": 0.0}, {"a": 2.0, "b": 2.05, "tolerance": 0.1}]),
    (
        Switch,
        [
            {"on": True, "when_on": 4.0, "when_off": 5.0},
            {"on": False, "when_on": 4.0, "when_off": 5.0},
        ],
    ),
    (
        Select,
        [
            {"index": index, "option_0": 10.0, "option_1": 11.0, "option_2": 12.0, "option_3": 13.0}
            for index in (-1, 0, 1, 2, 3, 9)
        ],
    ),
    (Constant, [{"value": 0.3}]),
    (Feedback, [{"value": 0.75}]),
    (
        Random,
        [
            {"low": 0.0, "high": 1.0, "seed": seed, "now": now}
            for seed in (0, 7)
            for now in (0, 123_456_789, 98_765_432_101_234)
        ],
    ),
    (
        PerlinNoise,
        [{"x": x, "seed": seed} for seed in (0, 3) for x in (-2.75, -0.5, 0.0, 0.3, 1.9, 41.25)],
    ),
    (Lfo, [{"rate_hz": 1.0, "now": 5_250_000_000, "previous": LfoPhase(0.0, 5_000_000_000, True)}]),
    (
        Timer,
        [
            {"reset": False, "now": 3_500_000_000, "previous": TimerStart(1_000_000_000, True)},
            {"reset": False, "now": 7, "previous": TimerStart()},
        ],
    ),
    (Lag, [{"x": 0.0, "seconds": 0.1, "now": 100_000_000, "previous": LagValue(1.0, 0, True)}]),
    (
        Trigger,
        [{"x": True, "previous": TriggerLevel(False)}, {"x": True, "previous": TriggerLevel(True)}],
    ),
    (
        Counter,
        [
            {"increment": True, "reset": False, "previous": CounterCount(4, False)},
            {"increment": True, "reset": True, "previous": CounterCount(4, False)},
        ],
    ),
    (
        SampleAndHold,
        [
            {"x": 0.9, "sample": True, "previous": SampledValue(0.5, False)},
            {"x": 0.9, "sample": True, "previous": SampledValue(0.5, True)},
        ],
    ),
]

_EvaluateEntry = ctypes.CFUNCTYPE(
    ctypes.c_int32,
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.POINTER(ctypes.c_double),
    ctypes.c_int64,
)


def _as_f64(value: Any) -> float:
    return 1.0 if value is True else 0.0 if value is False else float(value)


def _the_values_a_call_returns(
    function_node: Any, call: "dict[str, Any]"
) -> "tuple[list[float], list[float]]":
    signature = function_node.__tatolab_node_function_signature__
    returned = function_node(**call)
    output, new_state = returned if signature["state_parameter"] is not None else (returned, None)
    outputs = [
        _as_f64(output if each_output["field"] is None else getattr(output, each_output["field"]))
        for each_output in signature["outputs"]
    ]
    state = (
        []
        if new_state is None
        else [_as_f64(getattr(new_state, field.name)) for field in dataclasses.fields(new_state)]
    )
    return outputs, state


def _the_values_its_evaluate_entry_returns(
    function_node: Any, compiled: Any, call: "dict[str, Any]"
) -> "tuple[list[float], list[float]]":
    signature = function_node.__tatolab_node_function_signature__
    input_values = (ctypes.c_double * max(1, len(signature["inputs"])))(
        *[_as_f64(call[each_input["parameter"]]) for each_input in signature["inputs"]]
    )
    previous = call.get(signature["state_parameter"] or "")
    state_values = (ctypes.c_double * max(1, len(compiled.state_fields)))(
        *[_as_f64(getattr(previous, field)) for field in compiled.state_fields]
    )
    output_values = (ctypes.c_double * len(signature["outputs"]))()
    config_values = (ctypes.c_double * 1)()
    status = _EvaluateEntry(compiled.evaluate_entry_address)(
        input_values,
        config_values,
        state_values,
        output_values,
        call.get(signature["time_parameter"] or "", 0),
    )
    assert status == 0, f"{function_node.__name__}'s evaluate entry refused {call}"
    return list(output_values), list(state_values)[: len(compiled.state_fields)]


@pytest.mark.parametrize(
    ("function_node", "calls"),
    _CALLS_EACH_NODE_IS_HELD_TO_COMPILED,
    ids=[function_node.__name__ for function_node, _ in _CALLS_EACH_NODE_IS_HELD_TO_COMPILED],
)
def test_each_node_compiles_and_its_evaluate_entry_returns_what_its_python_does(
    function_node: Any, calls: "list[dict[str, Any]]"
):
    compilation = pytest.importorskip("tatolab.runtime._function_node_compilation")
    compiled = compilation.prepare_function_node_to_evaluate(
        function_node, f"tatolab.ops:{function_node.__name__}"
    )
    assert isinstance(compiled, compilation.FunctionNodeCompiledToItsEvaluateEntry), getattr(
        compiled, "why_it_does_not_compile", compiled
    )
    for call in calls:
        python_outputs, python_state = _the_values_a_call_returns(function_node, call)
        compiled_outputs, compiled_state = _the_values_its_evaluate_entry_returns(
            function_node, compiled, call
        )
        assert compiled_outputs == pytest.approx(python_outputs, rel=1e-12, abs=1e-12), call
        assert compiled_state == pytest.approx(python_state, rel=1e-12, abs=1e-12), call


def test_every_node_the_module_exports_is_held_to_compiled():
    import tatolab.ops

    function_nodes = {
        name
        for name in tatolab.ops.__all__
        if hasattr(getattr(tatolab.ops, name), "__tatolab_node_function_signature__")
    }
    held = {function_node.__name__ for function_node, _ in _CALLS_EACH_NODE_IS_HELD_TO_COMPILED}
    assert function_nodes == held
