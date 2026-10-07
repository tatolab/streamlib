# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`tatolab.stream` imports anywhere, and its runtime-backed calls fail loudly where nothing is lent.

This suite's venv holds no runtime, which is exactly the place a stream module
is imported to be compiled, type-checked and tested.
"""

from __future__ import annotations

import ast
import importlib
import importlib.util
import pkgutil
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

import tatolab.stream
from tatolab.stream import VideoFrame, log
from tatolab.stream._runtime_lend import (
    RUNTIME_ENGINE_MODULE_NAME,
    RuntimeIsNotLentToThisInterpreterError,
    runtime_backed_function_registry,
)

STREAM_PACKAGE_DIRECTORY = Path(tatolab.stream.__file__).resolve().parent

RUNTIME_PACKAGE_NAME = "tatolab.runtime"

# The one module allowed to name the runtime, and it names it only as the
# string `importlib` resolves on first use.
RUNTIME_LEND_MODULE_FILE_NAME = "_runtime_lend.py"

ARGUMENTS_FOR_EACH_RUNTIME_BACKED_FUNCTION: "dict[str, tuple[Any, ...]]" = {
    "monotonic_now_ns": (),
    "start_monotonic_timer": (1_000_000,),
    "encode_bag_to_msgpack_bytes": ({"width": 4},),
    "decode_msgpack_bytes_to_python_object": (b"\x80",),
    "gpu_limited_access_of_the_typed_read_in_progress": (),
    "_emit_record_on_the_engine_log_pipeline": ("info", "a record", None),
}

LOG_FUNCTIONS_BY_NAME: "dict[str, Callable[..., None]]" = {
    "log.trace": log.trace,
    "log.debug": log.debug,
    "log.info": log.info,
    "log.warn": log.warn,
    "log.warning": log.warning,
    "log.error": log.error,
}


def every_stream_module_name() -> "list[str]":
    return [
        module_info.name
        for module_info in pkgutil.walk_packages(
            tatolab.stream.__path__, prefix=f"{tatolab.stream.__name__}."
        )
    ]


def test_no_runtime_is_installed_beside_this_suite() -> None:
    """The division is structural: were the runtime here, every check below
    would pass for the wrong reason."""
    assert importlib.util.find_spec(RUNTIME_PACKAGE_NAME) is None


def test_every_stream_module_imports_and_loads_nothing_of_the_runtime() -> None:
    for module_name in every_stream_module_name():
        importlib.import_module(module_name)
    assert [
        module_name
        for module_name in sys.modules
        if module_name == RUNTIME_PACKAGE_NAME
        or module_name.startswith(f"{RUNTIME_PACKAGE_NAME}.")
    ] == []


def test_no_stream_module_imports_the_runtime_by_name() -> None:
    """Not even under `TYPE_CHECKING`: a type checker would then need the
    runtime installed to read a stream."""
    importing_the_runtime: "list[str]" = []
    for module_file in sorted(STREAM_PACKAGE_DIRECTORY.rglob("*.py")):
        for statement in ast.walk(ast.parse(module_file.read_text(encoding="utf-8"))):
            if isinstance(statement, ast.Import):
                imported_names = [alias.name for alias in statement.names]
            elif isinstance(statement, ast.ImportFrom) and statement.level == 0:
                imported_names = [statement.module or ""]
            else:
                continue
            importing_the_runtime.extend(
                f"{module_file.name}: {imported_name}"
                for imported_name in imported_names
                if imported_name == RUNTIME_PACKAGE_NAME
                or imported_name.startswith(f"{RUNTIME_PACKAGE_NAME}.")
            )
    assert importing_the_runtime == []


def test_only_the_lend_module_names_the_runtime_engine() -> None:
    naming_the_engine = [
        module_file.name
        for module_file in sorted(STREAM_PACKAGE_DIRECTORY.rglob("*.py"))
        if RUNTIME_ENGINE_MODULE_NAME in module_file.read_text(encoding="utf-8")
    ]
    assert naming_the_engine == [RUNTIME_LEND_MODULE_FILE_NAME]


def test_every_runtime_backed_function_is_exercised_below() -> None:
    assert {
        forward_to_native_callable.__name__
        for forward_to_native_callable in runtime_backed_function_registry
    } == set(ARGUMENTS_FOR_EACH_RUNTIME_BACKED_FUNCTION)


@pytest.mark.parametrize(
    "runtime_backed_function",
    list(runtime_backed_function_registry),
    ids=lambda runtime_backed_function: runtime_backed_function.__name__,
)
def test_a_runtime_backed_function_raises_naming_itself_where_nothing_is_lent(
    runtime_backed_function: "Callable[..., Any]",
) -> None:
    function_name = runtime_backed_function.__name__
    with pytest.raises(RuntimeIsNotLentToThisInterpreterError) as refusal:
        runtime_backed_function(*ARGUMENTS_FOR_EACH_RUNTIME_BACKED_FUNCTION[function_name])
    assert str(refusal.value).startswith(
        f"{function_name}() runs only in the interpreter the runtime starts for a node"
    )


@pytest.mark.parametrize("log_function_name", sorted(LOG_FUNCTIONS_BY_NAME))
def test_a_log_function_raises_naming_itself_where_nothing_is_lent(
    log_function_name: str,
) -> None:
    with pytest.raises(RuntimeIsNotLentToThisInterpreterError) as refusal:
        LOG_FUNCTIONS_BY_NAME[log_function_name]("a record", detail="fine")
    assert str(refusal.value).startswith(
        f"{log_function_name}() runs only in the interpreter the runtime starts for a node"
    )


def test_a_frame_builds_where_nothing_is_lent_and_refuses_its_pixels_by_name() -> None:
    """No typed read can be in progress without a runtime, so a test builds a
    frame the way a read outside one would: unclaimed."""
    frame = VideoFrame(
        surface_id="camera#7",
        width=20,
        height=12,
        timestamp_ns=123_456,
        color_info={"primaries": "bt709", "range": "full"},
    )
    assert frame.surface_id == "camera#7"
    with pytest.raises(RuntimeError, match="was not built by a typed read"):
        with frame.cpu():
            pass
