# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The lent bootstrap describes a project's nodes with whatever `tatolab-stream` its venv holds.

A runtime at least as new as a stream's `tatolab-stream` runs it, so CI runs this file twice:
once with this commit's `tatolab-stream` and once with the newest one PyPI holds. The node
module uses only what every `tatolab-stream` the runtime runs declares.
"""

import json
import os
import subprocess
import sys
import textwrap
from pathlib import Path

from test_wheel_portability import runtime_unit_lend_directory

from tatolab.runtime._engine import engine_build_id_compiled_into_this_extension

SECONDS_A_DESCRIBE_HAS_TO_ANSWER = 60.0

NODES_EVERY_RUNNABLE_TATOLAB_STREAM_DECLARES = '''
from tatolab.stream import Input, Output, Packets, node


@node.process(description="relays each packet")
class PacketRelay:
    packets_from_upstream = Input(Packets, delivery="ordered")
    packets_to_downstream = Output(Packets)

    def process(self, ctx) -> None: ...


@node.function
def remap(x: float, low: float = 0.0, high: float = 1.0) -> float:
    """Map 0..1 onto low..high."""
    return low + x * (high - low)
'''


def test_the_lent_bootstrap_describes_a_process_node_and_a_function_node(tmp_path: Path):
    lend_directory = runtime_unit_lend_directory()
    (tmp_path / "nodes_of_the_project.py").write_text(
        textwrap.dedent(NODES_EVERY_RUNNABLE_TATOLAB_STREAM_DECLARES)
    )

    described = subprocess.run(
        [
            sys.executable,
            str(lend_directory / "tatolab" / "runtime" / "_processor_interpreter_bootstrap.py"),
            "--describe",
            "nodes_of_the_project:PacketRelay",
            "nodes_of_the_project:remap",
        ],
        cwd=tmp_path,
        env={
            "PATH": os.environ.get("PATH", ""),
            "PYTHONPATH": f"{lend_directory}{os.pathsep}{tmp_path}",
            "STREAMLIB_ENGINE_BUILD_ID": engine_build_id_compiled_into_this_extension(),
        },
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=SECONDS_A_DESCRIBE_HAS_TO_ANSWER,
        check=False,
    )

    assert described.returncode == 0, described.stderr
    document = json.loads(described.stdout)
    assert document["refused_node_types"] == [], document
    assert [
        (described_node_type["short_name"], described_node_type["kind"])
        for described_node_type in document["described_node_types"]
    ] == [("PacketRelay", "process"), ("remap", "function")]
