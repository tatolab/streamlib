# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""`@node.function` refuses a body that computes from more than its inputs, naming each way and its fix."""

from __future__ import annotations

import datetime
import os
import random
import socket
import threading
import time
from datetime import datetime as imported_datetime
from time import perf_counter
from typing import Any

import numpy as np
import pytest

from tatolab.stream import State, Stateful, Time, node
from tatolab.stream._function_node_side_effect_check import find_side_effects_in_a_function_node_body

import random as aliased_random

calls_seen: "list[float]" = []
gain_table = np.array([0.5, 1.0, 2.0])
frozen_gain_table = np.array([0.5, 1.0, 2.0])
frozen_gain_table.setflags(write=False)
TABLE_OF_STEPS = (0.0, 0.25, 0.5)
counter = 0


def reads_the_wall_clock(x: float) -> float:
    return x + time.time()


def reads_perf_counter_imported_by_name(x: float) -> float:
    return x + perf_counter()


def reads_datetime_now(x: float) -> float:
    return x + datetime.datetime.now().second


def reads_datetime_now_through_the_imported_class(x: float) -> float:
    return x + imported_datetime.now().second


def draws_a_random_number(x: float) -> float:
    return x * random.random()


def draws_through_an_aliased_random(x: float) -> float:
    return x * aliased_random.uniform(0.0, 1.0)


def draws_from_numpy_random(x: float) -> float:
    return x * float(np.random.rand())


def reads_urandom(x: float) -> float:
    return x + os.urandom(1)[0]


def opens_a_file(x: float) -> float:
    with open("/dev/null") as file_it_opened:
        return x + len(file_it_opened.read())


def opens_a_socket(x: float) -> float:
    socket.socket()
    return x


def starts_a_thread(x: float) -> float:
    threading.Thread(target=print).start()
    return x


def writes_a_global(x: float) -> float:
    global counter
    counter += 1
    return x + counter


def appends_to_a_module_list(x: float) -> float:
    calls_seen.append(x)
    return x


def reads_a_writeable_module_array(x: float) -> float:
    return x * float(gain_table[0])


@pytest.mark.parametrize(
    ("body", "the_refusal_names", "the_fix_names"),
    [
        (reads_the_wall_clock, "calls `time.time`, which reads the clock", "`now: Time`"),
        (
            reads_perf_counter_imported_by_name,
            "calls `perf_counter` (`time.perf_counter`), which reads the clock",
            "`now: Time`",
        ),
        (reads_datetime_now, "calls `datetime.datetime.now`, which reads the clock", "`now: Time`"),
        (
            reads_datetime_now_through_the_imported_class,
            "calls `imported_datetime.now` (`datetime.datetime.now`)",
            "`now: Time`",
        ),
        (draws_a_random_number, "calls `random.random`, which draws from a random source", "`State[...]`"),
        (draws_through_an_aliased_random, "(`random.uniform`)", "seed"),
        (draws_from_numpy_random, "(`numpy.random.rand`)", "seed"),
        (reads_urandom, "calls `os.urandom`", "seed"),
        (opens_a_file, "calls `open` (`io.open`), which opens a file", "`@node.process`"),
        (opens_a_socket, "calls `socket.socket`, which opens a socket", "`@node.process`"),
        (starts_a_thread, "calls `threading.Thread`, which starts or coordinates a thread", "`@node.process`"),
        (writes_a_global, "declares `global counter`", "`State[...]`"),
        (appends_to_a_module_list, "reads the module-level `calls_seen`, a mutable `list`", "`config`"),
        (reads_a_writeable_module_array, "reads the module-level `gain_table`, a mutable `ndarray`", "setflags"),
    ],
)
def test_a_side_effect_in_the_body_is_refused_at_decoration_by_name_with_its_fix(
    body: Any, the_refusal_names: str, the_fix_names: str
):
    with pytest.raises(TypeError) as refusal:
        node.function(body)
    message = str(refusal.value)
    assert body.__name__ in message
    assert the_refusal_names in message
    assert the_fix_names in message


def test_the_refusal_names_the_line_of_each_side_effect():
    (finding,) = find_side_effects_in_a_function_node_body(reads_the_wall_clock)
    assert finding.line == reads_the_wall_clock.__code__.co_firstlineno + 1


def test_every_side_effect_in_one_body_is_named_in_one_refusal():
    with pytest.raises(TypeError) as refusal:
        node.function(draws_and_reads_the_clock)
    message = str(refusal.value)
    assert "reads the clock" in message
    assert "draws from a random source" in message


def draws_and_reads_the_clock(x: float) -> float:
    return x * random.random() + time.monotonic()


def scales_and_wraps(x: float, gain: float = 2.0) -> float:
    total = 0.0
    for step in TABLE_OF_STEPS:
        total += step
    return (x * gain + total + float(frozen_gain_table[1])) % 1.0


def counts_with_its_own_state(
    x: float, *, now: Time, previous: State[float]
) -> Stateful[float, float]:
    time = float(now) * 1e-9
    random = previous + x
    return random + time, random


def test_a_numeric_body_reading_only_its_inputs_and_constants_passes():
    assert find_side_effects_in_a_function_node_body(scales_and_wraps) == []
    node.function(scales_and_wraps)


def test_a_local_named_like_a_refused_module_is_not_mistaken_for_it():
    assert find_side_effects_in_a_function_node_body(counts_with_its_own_state) == []


