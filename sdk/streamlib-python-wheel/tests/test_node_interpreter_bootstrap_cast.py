# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The node interpreter bootstrap's `--cast-a-saved-value`, run by path the way the engine
runs it: a function node conversion's body run once out of stream on a saved value."""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from typing import Any

import pytest

from tatolab.runtime import _node_interpreter_bootstrap as node_interpreter_bootstrap
from test_node_interpreter_bootstrap import run_the_bootstrap

#: The `every-pin-kind` sample's own nodes, among them its conversion `LevelAsPercent`.
EVERY_PIN_KIND_SAMPLES_NODES = (
    Path(__file__).resolve().parents[3] / "examples" / "every-pin-kind" / "nodes"
)


def cast_document(cast: subprocess.CompletedProcess[str]) -> dict[str, Any]:
    """The one JSON document a cast prints on stdout, failing with its stderr if there is not
    one."""
    assert cast.returncode == 0, cast.stderr
    try:
        return json.loads(cast.stdout)
    except json.JSONDecodeError as not_one_document:
        pytest.fail(
            f"stdout is not one JSON document ({not_one_document}):\n{cast.stdout}\n"
            f"stderr:\n{cast.stderr}"
        )


def a_request_file(directory: Path, input_port: str, saved_value: Any) -> str:
    """The path of a request file in `directory` asking a cast of `saved_value` on the
    conversion's wired input `input_port`, as the engine writes one."""
    request_file = directory / "saved-value-cast-request.json"
    request_file.write_text(
        json.dumps(
            {
                node_interpreter_bootstrap.SAVED_VALUE_CAST_REQUEST_INPUT_KEY: input_port,
                node_interpreter_bootstrap.SAVED_VALUE_CAST_REQUEST_VALUE_KEY: saved_value,
            }
        ),
        encoding="utf-8",
    )
    return str(request_file)


def test_the_samples_own_conversion_casts_a_saved_value_out_of_stream(tmp_path: Path):
    """The `every-pin-kind` sample's `LevelAsPercent` casts a saved 0.1 into the String `10%`,
    its body run once with no stream started.

    Mental-revert: answering the saved value as it is prints `0.1`, which no String takes.
    """
    shutil.copytree(EVERY_PIN_KIND_SAMPLES_NODES, tmp_path / "nodes")

    cast = run_the_bootstrap(
        tmp_path,
        node_interpreter_bootstrap.CAST_A_SAVED_VALUE_ARGUMENT,
        "nodes.level_as_percent:LevelAsPercent",
        a_request_file(tmp_path, "level", 0.1),
    )

    assert cast_document(cast) == {
        node_interpreter_bootstrap.SAVED_VALUE_CAST_DOCUMENT_CAST_KEY: "10%"
    }, cast.stderr


def test_a_cast_by_a_module_that_will_not_import_names_why(tmp_path: Path):
    cast = run_the_bootstrap(
        tmp_path,
        node_interpreter_bootstrap.CAST_A_SAVED_VALUE_ARGUMENT,
        "no_such_module:LevelAsPercent",
        a_request_file(tmp_path, "level", 0.1),
    )

    document = cast_document(cast)
    not_run_because = node_interpreter_bootstrap.SAVED_VALUE_CAST_DOCUMENT_NOT_RUN_BECAUSE_KEY
    assert set(document) == {not_run_because}, document
    assert "`no_such_module:LevelAsPercent` did not load" in document[not_run_because]
    assert "No module named 'no_such_module'" in document[not_run_because]


def test_a_body_giving_a_number_no_saved_value_holds_is_a_refused_value(tmp_path: Path):
    """A body giving NaN is answered `not_cast_because`, the document still one JSON value.

    Mental-revert: writing NaN into the document fails after the cast, and the engine reads
    the missing document as a body that did not run."""
    (tmp_path / "levels.py").write_text(
        "from tatolab.stream import node\n\n\n"
        "@node.meta(conversion=True)\n"
        "@node\n"
        "def TextAsALevel(text: str = '') -> float:\n"
        "    return float(text)\n"
    )

    cast = run_the_bootstrap(
        tmp_path,
        node_interpreter_bootstrap.CAST_A_SAVED_VALUE_ARGUMENT,
        "levels:TextAsALevel",
        a_request_file(tmp_path, "text", "nan"),
    )

    document = cast_document(cast)
    not_cast_because = node_interpreter_bootstrap.SAVED_VALUE_CAST_DOCUMENT_NOT_CAST_BECAUSE_KEY
    assert set(document) == {not_cast_because}, document
    assert "which no saved value holds" in document[not_cast_because], document


def test_a_saved_value_too_long_for_one_argument_reaches_the_body_whole(tmp_path: Path):
    """A String saved value of 200 KiB, longer than Linux lets one command-line argument be,
    reaches the body whole through the request file.

    Mental-revert: reading the request from the argument itself reads a path, not JSON, and
    the cast prints no document."""
    (tmp_path / "lengths.py").write_text(
        "from tatolab.stream import node\n\n\n"
        "@node.meta(conversion=True)\n"
        "@node\n"
        "def TextAsItsLength(text: str = '') -> int:\n"
        "    return len(text)\n"
    )
    longer_than_one_argument = 200 * 1024

    cast = run_the_bootstrap(
        tmp_path,
        node_interpreter_bootstrap.CAST_A_SAVED_VALUE_ARGUMENT,
        "lengths:TextAsItsLength",
        a_request_file(tmp_path, "text", "x" * longer_than_one_argument),
    )

    assert cast_document(cast) == {
        node_interpreter_bootstrap.SAVED_VALUE_CAST_DOCUMENT_CAST_KEY: longer_than_one_argument
    }, cast.stderr
