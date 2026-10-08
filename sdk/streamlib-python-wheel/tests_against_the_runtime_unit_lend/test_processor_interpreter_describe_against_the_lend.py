# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A load describes a node type against the lend `cargo xtask build-runtime` lays out.

Run from a venv holding `tatolab-stream` and pytest, with the lend first on
`PYTHONPATH`: the `Runtime` built here is the lend's own, so the processor
interpreter it describes in borrows that same lend. The graph is never run, so
no device is needed.
"""

import os
import sys
import textwrap
from pathlib import Path

import tatolab.runtime
from tatolab.runtime._engine import processor_class_import_paths_in_this_processes_catalog
from test_wheel_portability import runtime_unit_lend_directory

DESCRIBED_NODE_MODULE = "runtime_unit_lend_described_nodes"
DESCRIBED_NODE_TYPE = f"{DESCRIBED_NODE_MODULE}:DescribedAgainstTheLendSource"


def test_a_type_described_against_the_lend_registers_without_its_module_entering_this_process(
    tmp_path: Path,
):
    lend_directory = runtime_unit_lend_directory()
    first_python_path_entry = os.environ.get("PYTHONPATH", "").split(os.pathsep)[0]
    assert first_python_path_entry, "the lend must lead PYTHONPATH"
    assert lend_directory == Path(first_python_path_entry).resolve(), (
        f"tatolab.runtime came from {lend_directory}, not the lend leading PYTHONPATH"
    )
    assert tatolab.runtime.__file__ is not None
    assert Path(tatolab.runtime.__file__).resolve().is_relative_to(lend_directory)
    (tmp_path / f"{DESCRIBED_NODE_MODULE}.py").write_text(
        textwrap.dedent(
            """
            from tatolab.stream import node


            @node(execution="continuous", interval_ms=50)
            class DescribedAgainstTheLendSource:
                @node.output()
                def bags_to_downstream(self) -> None: ...

                def process(self, ctx) -> None: ...
            """
        )
    )
    assert DESCRIBED_NODE_TYPE not in processor_class_import_paths_in_this_processes_catalog()

    runtime = tatolab.runtime.Runtime()
    try:
        runtime.load(
            {"nodes": [{"name": "nodeundertest", "type": DESCRIBED_NODE_TYPE, "config": {}}]},
            project_directory=tmp_path,
            interpreter=sys.executable,
        )
    finally:
        runtime.shutdown()

    assert DESCRIBED_NODE_TYPE in processor_class_import_paths_in_this_processes_catalog()
    assert DESCRIBED_NODE_MODULE not in sys.modules
