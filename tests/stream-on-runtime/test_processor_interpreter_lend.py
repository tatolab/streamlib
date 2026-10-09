# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stream's Python nodes run from a venv holding only `tatolab-stream`.

The suite venv has no `tatolab.runtime` of its own: a processor interpreter
started from it borrows the runtime through the runtime unit's lend, and
`tatolabd` learns each node's ports by describing it there. What a load refuses
— a module that raises, an unstamped class, an interpreter that cannot load
the lent runtime — ends at the load, so only the running stream needs a GPU.
`tatolabd` itself is native: every processor interpreter, describing or
running, is a process beneath it, and no Python is mapped into its own.
"""

from __future__ import annotations

import os
import platform
import re
import subprocess
import sys
import textwrap
from collections.abc import Callable
from pathlib import Path

import pytest

from conftest import StreamGraphLoadOutcome, environment_reaching_no_vulkan_driver
from helper_process_observation import assert_runs_in_a_process_of_its_own_beneath
from node_module_whose_describe_holds_the_load import NodeModuleWhoseDescribeHoldsTheLoad
from processor_interpreter_lend_probes import (
    CountsBagsFromUpstreamSink,
    ReportsItsInterpreterSource,
)
from runtime_process_under_test import RuntimeProcessUnderTest
from runtime_unit_under_test import SUITE_VENV_INTERPRETER, RuntimeUnitUnderTest
from tatolab.stream import StreamBuilder, stream

LoadStreamGraphOnTatolabd = Callable[..., StreamGraphLoadOutcome]

#: A mapped image that is CPython or a CPython extension module, on Linux or macOS.
PYTHON_LIBRARY_IMAGE = re.compile(r"libpython|Python\.framework|\.abi3\.so$|\.cpython-[^/]*\.so$")


@stream
def interpreter_reporting_source_into_counting_sink(stream_builder: StreamBuilder) -> None:
    """A source announcing its interpreter into a sink counting what it received."""
    source = stream_builder.add(ReportsItsInterpreterSource)
    sink = stream_builder.add(CountsBagsFromUpstreamSink)
    stream_builder.connect(source.output("bags_to_downstream"), sink.input("bags_from_upstream"))


def write_project_module(project_directory: Path, module_name: str, source: str) -> None:
    (project_directory / f"{module_name}.py").write_text(textwrap.dedent(source))


def graph_naming_one_node_of_type(node_type: str) -> "dict[str, object]":
    return {"stream": "nodeundertest", "nodes": [{"name": "nodeundertest", "type": node_type, "config": {}}]}


def refused_naming(load_outcome: StreamGraphLoadOutcome) -> str:
    """A refused load's reason; fails if the graph loaded."""
    assert not load_outcome.loaded, (
        f"the graph loaded; it should have been refused:\n{load_outcome.stderr_text[-4000:]}"
    )
    assert load_outcome.refusal is not None, load_outcome.stderr_text[-4000:]
    return load_outcome.refusal


def loaded_with(load_outcome: StreamGraphLoadOutcome) -> int:
    """How many of the stream's nodes a load that succeeded loaded; fails if it was refused."""
    assert load_outcome.loaded, (
        f"the graph was refused: {load_outcome.refusal}\n{load_outcome.stderr_text[-4000:]}"
    )
    assert load_outcome.loaded_node_count is not None
    return load_outcome.loaded_node_count


def images_mapped_into(process_id: int) -> "list[str]":
    """The path of every file image mapped into `process_id`: `/proc/<pid>/maps` on
    Linux, `lsof` on macOS."""
    if sys.platform == "linux":
        return [
            columns[5]
            for columns in (
                row.split(maxsplit=5)
                for row in Path(f"/proc/{process_id}/maps").read_text().splitlines()
            )
            if len(columns) == 6
        ]
    listed = subprocess.run(
        ["lsof", "-n", "-P", "-F", "n", "-p", str(process_id)],
        capture_output=True,
        text=True,
        check=True,
    )
    return [line[1:] for line in listed.stdout.splitlines() if line.startswith("n")]


def assert_no_python_is_mapped_into(process_id: int) -> None:
    mapped_images = images_mapped_into(process_id)
    assert mapped_images, f"read no mapped image of {process_id}, so this proves nothing"
    python_images = sorted({image for image in mapped_images if PYTHON_LIBRARY_IMAGE.search(image)})
    assert python_images == [], f"tatolabd maps a Python library: {python_images}"


# ---- describe, from the suite venv's interpreter ------------------------------


def test_a_type_whose_module_raises_at_import_is_refused_quoting_the_interpreters_stderr(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd, tmp_path: Path
):
    write_project_module(
        tmp_path,
        "lend_raising_nodes",
        """
        raise RuntimeError("lend_raising_nodes refuses to import on purpose")
        """,
    )

    refusal = refused_naming(
        load_stream_graph_on_tatolabd(
            graph_naming_one_node_of_type("lend_raising_nodes:NeverDeclared"),
            project_directory=tmp_path,
        )
    )

    assert "lend_raising_nodes:NeverDeclared" in refusal
    assert "lend_raising_nodes refuses to import on purpose" in refusal
    assert "lend_raising_nodes" not in sys.modules


def test_an_unstamped_class_is_refused_by_name(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd, tmp_path: Path
):
    write_project_module(
        tmp_path,
        "lend_unstamped_nodes",
        """
        class NotANode:
            def process(self, ctx) -> None: ...
        """,
    )

    refusal = refused_naming(
        load_stream_graph_on_tatolabd(
            graph_naming_one_node_of_type("lend_unstamped_nodes:NotANode"),
            project_directory=tmp_path,
        )
    )

    assert "lend_unstamped_nodes:NotANode" in refusal
    assert "carries no `@node` declaration" in refusal


def test_an_interpreter_that_cannot_load_the_lent_runtime_is_refused_naming_what_it_is(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd, tmp_path: Path
):
    """The wrapper drops the lend from `PYTHONPATH`, so the suite venv's
    interpreter — which has no runtime of its own — cannot import it."""
    project_directory = tmp_path / "project"
    project_directory.mkdir()
    write_project_module(
        project_directory,
        "lend_declared_nodes_for_a_refused_interpreter",
        """
        from tatolab.stream import node


        @node(execution="manual")
        class DeclaredNode:
            @node.output()
            def nothing_to_downstream(self) -> None: ...
        """,
    )
    interpreter_without_the_lend = tmp_path / "python-without-the-lend"
    interpreter_without_the_lend.write_text(
        f'#!/bin/sh\nunset PYTHONPATH\nexec "{SUITE_VENV_INTERPRETER}" "$@"\n'
    )
    interpreter_without_the_lend.chmod(0o755)

    refusal = refused_naming(
        load_stream_graph_on_tatolabd(
            graph_naming_one_node_of_type(
                "lend_declared_nodes_for_a_refused_interpreter:DeclaredNode"
            ),
            project_directory=project_directory,
            interpreter=interpreter_without_the_lend,
        )
    )

    assert "lend_declared_nodes_for_a_refused_interpreter:DeclaredNode" in refusal
    assert str(interpreter_without_the_lend) in refusal
    assert f"interpreter: {SUITE_VENV_INTERPRETER}" in refusal
    assert f"implementation: {platform.python_implementation()}" in refusal
    assert f"version: {platform.python_version()}" in refusal
    assert "free-threaded: no" in refusal
    assert f"architecture: {platform.machine()}" in refusal
    assert "No module named 'tatolab.runtime'" in refusal


def test_a_type_that_describes_registers_without_its_module_entering_this_process(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd, tmp_path: Path
):
    write_project_module(
        tmp_path,
        "lend_described_nodes",
        """
        from tatolab.stream import node


        @node(execution="continuous", interval_ms=50)
        class DescribedSource:
            @node.output()
            def bags_to_downstream(self) -> None: ...

            def process(self, ctx) -> None: ...
        """,
    )

    load_outcome = load_stream_graph_on_tatolabd(
        graph_naming_one_node_of_type("lend_described_nodes:DescribedSource"),
        project_directory=tmp_path,
    )

    assert loaded_with(load_outcome) == 1
    assert "lend_described_nodes" not in sys.modules


def test_a_type_described_against_the_lend_imports_the_runtime_the_runtime_unit_lends(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd,
    runtime_unit: RuntimeUnitUnderTest,
    tmp_path: Path,
):
    """The describing interpreter's `tatolab.runtime` is the lend's, read off the
    module it imported: the suite venv has none to fall back on."""
    lent_runtime_file_record = tmp_path / "lent-runtime-file.txt"
    write_project_module(
        tmp_path,
        "runtime_unit_lend_described_nodes",
        f"""
        from pathlib import Path

        import tatolab.runtime
        from tatolab.stream import node

        Path({str(lent_runtime_file_record)!r}).write_text(tatolab.runtime.__file__)


        @node(execution="continuous", interval_ms=50)
        class DescribedAgainstTheLendSource:
            @node.output()
            def bags_to_downstream(self) -> None: ...

            def process(self, ctx) -> None: ...
        """,
    )

    load_outcome = load_stream_graph_on_tatolabd(
        graph_naming_one_node_of_type(
            "runtime_unit_lend_described_nodes:DescribedAgainstTheLendSource"
        ),
        project_directory=tmp_path,
    )

    assert loaded_with(load_outcome) == 1
    assert "runtime_unit_lend_described_nodes" not in sys.modules
    assert Path(lent_runtime_file_record.read_text()).resolve().is_relative_to(
        runtime_unit.lend_directory.resolve()
    ), lent_runtime_file_record.read_text()


def test_a_relative_project_directory_and_interpreter_name_what_they_name_from_the_callers_directory(
    load_stream_graph_on_tatolabd: LoadStreamGraphOnTatolabd, tmp_path: Path
):
    """The interpreter starts in the project directory, so a relative path left
    relative would be read from there rather than from the caller's directory."""
    callers_directory = tmp_path / "callers-directory"
    project_directory = callers_directory / "project"
    project_directory.mkdir(parents=True)
    write_project_module(
        project_directory,
        "relatively_loaded_nodes",
        """
        from tatolab.stream import node


        @node(execution="continuous", interval_ms=50)
        class RelativelyLoadedSource:
            @node.output()
            def bags_to_downstream(self) -> None: ...

            def process(self, ctx) -> None: ...
        """,
    )
    relative_interpreter = os.path.relpath(SUITE_VENV_INTERPRETER, callers_directory)
    assert not (project_directory / relative_interpreter).exists()

    load_outcome = load_stream_graph_on_tatolabd(
        graph_naming_one_node_of_type("relatively_loaded_nodes:RelativelyLoadedSource"),
        project_directory="project",
        interpreter=relative_interpreter,
        working_directory=callers_directory,
    )

    assert loaded_with(load_outcome) == 1


def test_tatolabd_maps_no_python_while_a_processor_interpreter_describes_a_node_type(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
    held_node_module: NodeModuleWhoseDescribeHoldsTheLoad,
    tmp_path: Path,
):
    """Describe runs in a processor interpreter of its own, beneath `tatolabd`;
    `tatolabd`, mid-load with that interpreter parked at the node module's
    import, has no Python in it."""
    tatolabd = start_tatolabd(
        graph_naming_one_node_of_type(f"{held_node_module.name}:LoadedFrameRelay"),
        project_directory=held_node_module.project_directory,
        extra_environment=environment_reaching_no_vulkan_driver(tmp_path),
    )
    assert held_node_module.wait_until_the_load_reaches_the_import(), (
        f"the load never reached the describe:\n{tatolabd.recent_stderr()}"
    )
    try:
        assert held_node_module.describing_interpreter_process_id is not None
        assert_runs_in_a_process_of_its_own_beneath(
            held_node_module.describing_interpreter_process_id, tatolabd.pid
        )
        assert_no_python_is_mapped_into(tatolabd.pid)
    finally:
        tatolabd.interrupt()
        held_node_module.release_the_load()
        tatolabd.await_exit()


# ---- a running stream --------------------------------------------------------


@pytest.mark.requires_gpu
def test_a_node_runs_in_a_processor_interpreter_started_from_a_venv_holding_only_tatolab_stream(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]", runtime_unit: RuntimeUnitUnderTest
):
    tatolabd = start_tatolabd(interpreter_reporting_source_into_counting_sink)
    reported = tatolabd.await_every_marker("PROCESSOR_INTERPRETER", "BAGS_PROCESSED")
    assert_runs_in_a_process_of_its_own_beneath(
        reported["PROCESSOR_INTERPRETER"]["processor_interpreter_process_id"], tatolabd.pid
    )
    assert_no_python_is_mapped_into(tatolabd.pid)
    tatolabd.interrupt()
    tatolabd.await_clean_exit()

    processor_interpreter = reported["PROCESSOR_INTERPRETER"]
    bags_processed = reported["BAGS_PROCESSED"]
    assert processor_interpreter["processor_interpreter"] == str(SUITE_VENV_INTERPRETER)
    assert bags_processed["sink_interpreter"] == str(SUITE_VENV_INTERPRETER)
    assert bags_processed["upstream_interpreter"] == str(SUITE_VENV_INTERPRETER)
    assert Path(processor_interpreter["lent_runtime_file"]).resolve().is_relative_to(
        runtime_unit.lend_directory.resolve()
    ), (processor_interpreter["lent_runtime_file"], runtime_unit.lend_directory)
