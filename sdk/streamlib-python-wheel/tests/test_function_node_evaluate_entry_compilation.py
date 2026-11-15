# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A `@node.function` body compiled to its evaluate entry, or run as Python where it cannot be.

Each node is called through its compiled entry the way the native evaluator
calls it.
"""

from __future__ import annotations

import ctypes
import math
import struct
import itertools
from dataclasses import dataclass, field
from typing import Any

import pytest

from tatolab.runtime import _engine
from tatolab.runtime._function_node_compilation import (
    FunctionNodeBodyRunAsPython,
    FunctionNodeCompiledToItsEvaluateEntry,
    FunctionNodeDoesNotCompile,
    UNREAD_MESSAGE_VALUE_TYPE,
    compile_function_node_to_its_evaluate_entry,
    feed_each_input_port_from_its_function_node_island,
    prepare_function_node_to_evaluate,
)
from tatolab.stream import State, Stateful, UnreadMessage, node
from tatolab.stream import _function_node_declaration

from function_node_test_nodes import (
    GainConfig,
    Phase,
    RustGate,
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


def compiled(function_node: Any) -> FunctionNodeCompiledToItsEvaluateEntry:
    return compile_function_node_to_its_evaluate_entry(
        function_node, f"{__name__}:{function_node.__qualname__}"
    )


@dataclass(frozen=True)
class Halves:
    lower: float
    upper: float


@node.function
def halve(x: float) -> Halves:
    return Halves(lower=x / 2.0, upper=x - x / 2.0)


@node.function
def is_positive(x: float) -> bool:
    return x > 0.0


@node.function
def build_an_object(x: float) -> float:
    return float(len(repr(object()))) + x


@node.function
def square_root(x: float) -> float:
    if x < 0.0:
        raise ValueError("a negative number has no real square root")
    return math.sqrt(x)


@dataclass(frozen=True)
class Spread:
    centre: float
    width: float = 1.0


@node.function
def spread_around(x: float) -> Spread:
    return Spread(centre=x)


@dataclass(frozen=True)
class SpreadWithAFactoryWidth:
    centre: float = 0.0
    width: float = field(default_factory=lambda: 1.0)


@node.function
def spread_with_a_factory_width(x: float) -> SpreadWithAFactoryWidth:
    return SpreadWithAFactoryWidth(centre=x)


_SEEDS_HANDED_OUT = itertools.count(7)


@dataclass(frozen=True)
class SeededConfig:
    seed: int = field(default_factory=lambda: next(_SEEDS_HANDED_OUT))


@node.function
def offset_by_seed(x: float = 0.0, *, config: SeededConfig) -> float:
    return x + config.seed


@node.function
def count_from(start: int = 2**53 + 1) -> int:
    return start


def call_the_evaluate_entry(
    compiled: FunctionNodeCompiledToItsEvaluateEntry,
    inputs: "list[float]",
    config: "list[float]",
    state: "list[float]",
    now_ns: int = 0,
) -> "tuple[list[float], list[float]]":
    input_values = (ctypes.c_double * max(len(inputs), 1))(*inputs)
    config_values = (ctypes.c_double * max(len(config), 1))(*config)
    state_values = (ctypes.c_double * max(len(state), 1))(*state)
    output_values = (ctypes.c_double * len(compiled.output_ports))()
    status = _EVALUATE_ENTRY_TYPE(compiled.evaluate_entry_address)(
        input_values, config_values, state_values, output_values, now_ns
    )
    assert status == 0
    return list(output_values), list(state_values)[: len(state)]


def the_evaluate_entry_status(
    compiled: FunctionNodeCompiledToItsEvaluateEntry, inputs: "list[float]"
) -> int:
    input_values = (ctypes.c_double * max(len(inputs), 1))(*inputs)
    unused = (ctypes.c_double * 1)()
    output_values = (ctypes.c_double * len(compiled.output_ports))()
    return _EVALUATE_ENTRY_TYPE(compiled.evaluate_entry_address)(
        input_values, unused, unused, output_values, 0
    )


def test_a_stateless_body_runs_compiled_with_its_pins_in_signature_order():
    remap_compiled = compiled(remap)
    assert remap_compiled.input_ports == ("x", "low", "high")
    assert remap_compiled.pin_defaults_by_input_port == {"low": 0.0, "high": 1.0}
    assert call_the_evaluate_entry(remap_compiled, [0.5, 2.0, 4.0], [], [])[0] == [3.0]


def test_a_stateful_body_reads_time_and_its_state_and_writes_its_next_state():
    lfo_compiled = compiled(lfo)
    assert lfo_compiled.initial_state_values == (0.0, 0.0)
    outputs, state = call_the_evaluate_entry(lfo_compiled, [1.0], [], [0.0, 0.0], 1_000_000_000)
    assert outputs == [0.0] and state == [0.0, 1_000_000_000.0]
    outputs, state = call_the_evaluate_entry(lfo_compiled, [1.0], [], state, 1_250_000_000)
    assert outputs[0] == pytest.approx(1.0) and state == [0.25, 1_250_000_000.0]


def test_a_returned_dataclass_writes_one_output_per_field():
    halve_compiled = compiled(halve)
    assert halve_compiled.output_ports == ("lower", "upper")
    assert call_the_evaluate_entry(halve_compiled, [3.0], [], [])[0] == [1.5, 1.5]


def test_config_is_read_as_its_dataclass_with_a_bool_as_zero_or_one():
    amplify_compiled = compiled(amplify)
    assert amplify_compiled.config_fields == ("gain", "inverted")
    assert amplify_compiled.config_field_defaults == {"gain": 2.0, "inverted": 0.0}
    assert call_the_evaluate_entry(amplify_compiled, [3.0], [2.0, 0.0], [])[0] == [6.0]
    assert call_the_evaluate_entry(amplify_compiled, [3.0], [2.0, 1.0], [])[0] == [-6.0]


def test_an_int_input_and_output_travel_as_f64():
    assert call_the_evaluate_entry(compiled(tally), [2.0, 5.0], [], [])[0] == [7.0]


@pytest.mark.parametrize(
    ("function_node", "inputs", "config", "state", "now_ns"),
    [
        (remap, [0.25, -2.0, 6.0], [], [], 0),
        (lfo, [3.0], [], [0.1, 1_000_000_000.0], 1_100_000_000),
        (amplify, [1.5], [3.0, 1.0], [], 0),
        (tally, [4.0, 2.0], [], [], 0),
    ],
)
def test_a_compiled_body_matches_the_python_it_was_compiled_from(
    function_node: Any, inputs: "list[float]", config: "list[float]", state: "list[float]", now_ns: int
):
    outputs, new_state = call_the_evaluate_entry(compiled(function_node), inputs, config, state, now_ns)
    if function_node is remap:
        expected: Any = remap(*inputs)
    elif function_node is lfo:
        expected, expected_state = lfo(inputs[0], now=now_ns, previous=Phase(state[0], int(state[1])))
        assert new_state == pytest.approx([expected_state.cycles, expected_state.last_now_ns])
    elif function_node is amplify:
        expected = amplify(inputs[0], GainConfig(gain=config[0], inverted=bool(config[1])))
    else:
        expected = tally(inputs[0], int(inputs[1]))
    assert outputs == pytest.approx([float(expected)])


def test_a_second_compile_of_a_body_loads_it_from_numbas_cache():
    compiled(remap)
    assert compiled(remap).body_loaded_from_numbas_cache


def test_a_bool_output_travels_as_zero_or_one():
    is_positive_compiled = compiled(is_positive)
    assert call_the_evaluate_entry(is_positive_compiled, [2.0], [], [])[0] == [1.0]
    assert call_the_evaluate_entry(is_positive_compiled, [-2.0], [], [])[0] == [0.0]


def test_a_body_that_does_not_compile_is_refused_by_name():
    with pytest.raises(FunctionNodeDoesNotCompile, match="build_an_object's body does not compile"):
        compiled(build_an_object)


def test_a_body_that_raises_answers_the_refused_status():
    square_root_compiled = compiled(square_root)
    assert the_evaluate_entry_status(square_root_compiled, [4.0]) == 0
    assert the_evaluate_entry_status(square_root_compiled, [-1.0]) == -1


def test_a_body_may_omit_a_field_its_dataclass_defaults():
    assert call_the_evaluate_entry(compiled(spread_around), [3.0], [], [])[0] == [3.0, 1.0]


def test_a_factory_default_in_a_dataclass_the_body_builds_is_refused_by_name():
    with pytest.raises(FunctionNodeDoesNotCompile, match="`width` defaults through `default_factory`"):
        compiled(spread_with_a_factory_width)


def test_an_int_default_an_f64_cannot_hold_exactly_is_refused():
    with pytest.raises(FunctionNodeDoesNotCompile, match="only up to 2\\*\\*53"):
        compiled(count_from)


def test_what_is_not_a_function_node_is_refused():
    with pytest.raises(FunctionNodeDoesNotCompile, match="not a `@node.function` function"):
        compile_function_node_to_its_evaluate_entry(math.sin, "math:sin")


def test_a_config_field_its_class_does_not_declare_is_refused_by_name():
    import_path = f"{__name__}:offset_by_seed"
    with pytest.raises(FunctionNodeDoesNotCompile, match="config sets sede, which its config class"):
        feed_each_input_port_from_its_function_node_island(
            _engine.open_node_link_data_access_for_helper_process(),
            [an_island_of_one_node("level", import_path, {"sede": 3})],
            {import_path: offset_by_seed}.__getitem__,
        )


def test_a_config_field_default_factory_runs_for_each_node():
    data_plane = _engine.open_node_link_data_access_for_helper_process()
    data_plane.declare_ports(["first", "second"], [])
    import_path = f"{__name__}:offset_by_seed"
    feed_each_input_port_from_its_function_node_island(
        data_plane,
        [
            an_island_of_one_node("first", import_path, None, "seeded-1"),
            an_island_of_one_node("second", import_path, None, "seeded-2"),
        ],
        {import_path: offset_by_seed}.__getitem__,
    )
    seeds_read = []
    for port in ("first", "second"):
        payload = data_plane.read_from_input_port(port)
        assert payload is not None
        seeds_read.append(struct.unpack("<d", bytes(payload))[0])
    assert seeds_read[0] != seeds_read[1]


def test_a_rust_function_node_is_read_from_its_declaration_and_never_compiled():
    rust_compiled = compiled(WeightedSumCountingItsCalls)
    assert rust_compiled.evaluate_entry_address == (
        WeightedSumCountingItsCalls.rust_function_node_declaration.evaluate_entry_address
    )
    assert rust_compiled.input_ports == ("x", "bias")
    assert rust_compiled.pin_defaults_by_input_port == {"bias": 0.25}
    assert rust_compiled.output_ports == ("result",)
    assert rust_compiled.config_fields == ("weight",)
    assert rust_compiled.config_field_defaults == {"weight": 2.0}
    assert rust_compiled.state_fields == ("calls",)
    assert rust_compiled.initial_state_values == (10.0,)
    assert rust_compiled.is_stateful
    assert not rust_compiled.body_loaded_from_numbas_cache
    assert call_the_evaluate_entry(rust_compiled, [3.0, 1.0], [2.0], [10.0]) == ([7.0], [11.0])


@pytest.mark.parametrize(
    ("major_offset", "minor_offset", "refusal"),
    [(1, 0, "another major"), (0, 1, "newer than")],
)
def test_a_rust_function_node_built_for_a_table_this_runtime_does_not_serve_is_refused(
    major_offset: int, minor_offset: int, refusal: str
):
    built_for_another_table = node.rust_function(
        a_rust_function_node_declaration(
            module_host_table_abi_major=_engine.MODULE_HOST_TABLE_ABI_MAJOR + major_offset,
            module_host_table_abi_minor=_engine.MODULE_HOST_TABLE_ABI_MINOR + minor_offset,
        )
    )
    with pytest.raises(FunctionNodeDoesNotCompile, match=refusal):
        compile_function_node_to_its_evaluate_entry(
            built_for_another_table, f"{__name__}:WeightedSumCountingItsCalls"
        )


@node.function
def python_gate(value: UnreadMessage, open: bool = True) -> "UnreadMessage | None":
    return value if open else None


def test_a_rust_gate_carries_its_message_and_its_handed_out_flag_as_a_compiled_python_gate_does():
    rust_compiled = compiled(RustGate)
    python_compiled = compiled(python_gate)
    assert rust_compiled.input_value_types == python_compiled.input_value_types == (
        "unread_message",
        "bool",
    )
    assert rust_compiled.how_its_values_are_carried == python_compiled.how_its_values_are_carried
    assert rust_compiled.how_its_values_are_carried == {
        "inputs_that_take_none": [],
        "inputs_carrying_an_unread_message": [0],
        "outputs_carrying_an_unread_message": [0],
        "outputs_that_may_hand_out_nothing": [0],
    }


@dataclass(frozen=True)
class CallsSoFar:
    calls: int = 0


@node.function
def count_calls_by_spelling_them(step: float = 1.0, *, previous: State[CallsSoFar]) -> Stateful[int, CallsSoFar]:
    calls = previous.calls + int(step) + len(repr(object())) * 0
    return calls, CallsSoFar(calls)


@node.function
def refuse_a_negative_spelling(x: float) -> float:
    if x < 0.0:
        raise ValueError(f"{x!r} is negative")
    return float(len(repr(x)))


@node.function
def take_an_optional_level(level: "float | None" = None) -> float:
    return 0.0 if level is None else level


def test_a_body_that_does_not_compile_runs_as_python_saying_why():
    prepared = run_as_python(build_an_object)
    assert "body does not compile" in prepared.why_it_does_not_compile
    assert prepared.input_ports == ("x",)
    assert prepared.ports_carrying_records == frozenset()


def test_a_body_taking_or_giving_a_dict_runs_as_python():
    assert run_as_python(label_the_level).ports_carrying_records == frozenset({"result"})
    assert run_as_python(count_the_label_letters).ports_carrying_records == frozenset({"record"})


def test_a_signature_neither_compiled_nor_python_carries_is_refused_by_name():
    with pytest.raises(FunctionNodeDoesNotCompile, match="`level` is optional"):
        prepare_function_node_to_evaluate(take_an_optional_level, f"{__name__}:take_an_optional_level")


def test_a_body_run_as_python_answers_each_output_as_a_float_or_a_dict():
    assert FunctionNodeBodyRunAsPython(run_as_python(measure_its_own_spelling), "m", None).evaluate(
        [12.0], 0
    ) == [4.0]
    assert FunctionNodeBodyRunAsPython(run_as_python(label_the_level), "l", None).evaluate([0.25], 0) == [
        {"label": "low"}
    ]
    letters = FunctionNodeBodyRunAsPython(run_as_python(count_the_label_letters), "c", None).evaluate(
        [{"label": "high"}], 0
    )
    assert letters == [4.0] and isinstance(letters[0], float)


def test_the_runtime_spells_the_unread_message_value_type_as_tatolab_stream_does():
    assert UNREAD_MESSAGE_VALUE_TYPE == _function_node_declaration.UNREAD_MESSAGE_VALUE_TYPE


@dataclass(frozen=True)
class FirstMessageHeld:
    message: UnreadMessage | None = None


@node.function
def hold_the_first_message_handed(
    value: UnreadMessage | None = None, *, previous: State[FirstMessageHeld]
) -> Stateful[UnreadMessage | None, FirstMessageHeld]:
    if previous.message is None and value is not None:
        return value, FirstMessageHeld(value)
    return previous.message, previous


def test_a_body_run_as_python_hands_on_a_number_or_a_dict_its_state_held_from_an_earlier_call():
    holding = FunctionNodeBodyRunAsPython(run_as_python(hold_the_first_message_handed), "h", None)
    record = {"label": "low"}
    assert holding.evaluate([record], 0)[0] is record
    assert holding.evaluate([None], 1)[0] == record
    assert holding.evaluate([{"label": "high"}], 2)[0] == record


def test_a_body_run_as_python_keeps_a_state_of_any_record_between_calls():
    remembering = FunctionNodeBodyRunAsPython(run_as_python(remember_each_call_by_a_letter), "r", None)
    assert remembering.evaluate([1.0], 0) == [1.0]
    assert remembering.evaluate([2.0], 1) == [3.0]
    assert run_as_python(remember_each_call_by_a_letter).initial_state_values == (), (
        "a state record holding a string is not carried as f64s on the board"
    )


def test_a_body_run_as_python_with_a_state_of_numbers_carries_it_as_f64s_and_resumes_from_them():
    prepared = run_as_python(count_calls_by_spelling_them)
    assert prepared.initial_state_values == (0.0,)
    counting = FunctionNodeBodyRunAsPython(prepared, "counter", None)
    counting.resume_from_state_values([41.0])
    assert counting.evaluate([1.0], 0) == [42.0]
    assert counting.state_values() == [42.0]


def test_a_body_run_as_python_that_raises_keeps_its_state():
    refusing = FunctionNodeBodyRunAsPython(run_as_python(refuse_a_negative_spelling), "refuses", None)
    with pytest.raises(ValueError, match="is negative"):
        refusing.evaluate([-1.0], 0)
    assert refusing.evaluate([1.5], 0) == [3.0]


@node.function
def spell_a_positional_level(level: float = 0.75, /) -> float:
    return level + len(repr(object())) * 0


@node.function
def take_the_label_out(record: dict) -> int:
    return len(record.pop("label"))


@dataclass
class LabelsKept:
    labels: list = field(default_factory=list)


@node.function
def keep_a_label_unless_negative(x: float = 1.0, *, previous: State[LabelsKept]) -> Stateful[int, LabelsKept]:
    previous.labels.append(repr(x))
    if x < 0.0:
        raise ValueError(f"{x!r} is negative")
    return len(previous.labels), previous


@node.function
def jump_the_count_past_an_f64(step: float = 1.0, *, previous: State[CallsSoFar]) -> Stateful[float, CallsSoFar]:
    return step + len(repr(object())) * 0, CallsSoFar(previous.calls + 2**53 + 1)


@dataclass(frozen=True)
class LabelPolicy:
    prefix: str = ""


@dataclass(frozen=True)
class LabelConfig:
    policy: LabelPolicy = field(default_factory=LabelPolicy)


@node.function
def label_with_a_prefix(level: float = 0.5, *, config: LabelConfig) -> dict:
    return {"label": f"{config.policy.prefix}{level}"}


def test_a_body_run_as_python_takes_a_positional_only_input_positionally():
    assert FunctionNodeBodyRunAsPython(run_as_python(spell_a_positional_level), "p", None).evaluate(
        [0.25], 0
    ) == [0.25]


def test_a_body_run_as_python_that_changes_its_dict_input_leaves_the_one_it_was_handed():
    record = {"label": "high"}
    taking = FunctionNodeBodyRunAsPython(run_as_python(take_the_label_out), "t", None)
    assert taking.evaluate([record], 0) == [4.0]
    assert record == {"label": "high"}, "a sibling reading the same dict still finds its label"


def test_a_body_run_as_python_that_changes_its_state_and_raises_keeps_the_state_it_had():
    keeping = FunctionNodeBodyRunAsPython(run_as_python(keep_a_label_unless_negative), "k", None)
    assert keeping.evaluate([1.0], 0) == [1.0]
    with pytest.raises(ValueError, match="is negative"):
        keeping.evaluate([-1.0], 1)
    assert keeping.evaluate([2.0], 2) == [2.0], "the call that raised appended nothing it kept"


def test_a_body_run_as_python_keeps_no_state_its_stream_cannot_carry():
    jumping = FunctionNodeBodyRunAsPython(run_as_python(jump_the_count_past_an_f64), "counter", None)
    with pytest.raises(FunctionNodeDoesNotCompile, match="`counter`'s state `calls`"):
        jumping.evaluate([1.0], 0)
    assert jumping.state_values() == [0.0]


def test_a_body_run_as_python_that_takes_no_config_refuses_one_naming_its_fields():
    with pytest.raises(FunctionNodeDoesNotCompile, match="`m`'s config sets rate, and it takes no config"):
        FunctionNodeBodyRunAsPython(run_as_python(measure_its_own_spelling), "m", {"rate": 1})


def test_a_body_run_as_python_reads_a_nested_dataclass_of_its_config():
    labelling = FunctionNodeBodyRunAsPython(
        run_as_python(label_with_a_prefix), "l", {"policy": {"prefix": "at "}}
    )
    assert labelling.evaluate([0.5], 0) == [{"label": "at 0.5"}]
