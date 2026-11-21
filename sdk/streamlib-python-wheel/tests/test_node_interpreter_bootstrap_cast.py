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
        json.dumps(
            {
                node_interpreter_bootstrap.SAVED_VALUE_CAST_REQUEST_INPUT_KEY: "level",
                node_interpreter_bootstrap.SAVED_VALUE_CAST_REQUEST_VALUE_KEY: 0.1,
            }
        ),
    )

    assert cast_document(cast) == {
        node_interpreter_bootstrap.SAVED_VALUE_CAST_DOCUMENT_CAST_KEY: "10%"
    }, cast.stderr


def test_a_cast_by_a_module_that_will_not_import_names_why(tmp_path: Path):
    cast = run_the_bootstrap(
        tmp_path,
        node_interpreter_bootstrap.CAST_A_SAVED_VALUE_ARGUMENT,
        "no_such_module:LevelAsPercent",
        json.dumps(
            {
                node_interpreter_bootstrap.SAVED_VALUE_CAST_REQUEST_INPUT_KEY: "level",
                node_interpreter_bootstrap.SAVED_VALUE_CAST_REQUEST_VALUE_KEY: 0.1,
            }
        ),
    )

    document = cast_document(cast)
    not_run_because = node_interpreter_bootstrap.SAVED_VALUE_CAST_DOCUMENT_NOT_RUN_BECAUSE_KEY
    assert set(document) == {not_run_because}, document
    assert "`no_such_module:LevelAsPercent` did not load" in document[not_run_because]
    assert "No module named 'no_such_module'" in document[not_run_because]
