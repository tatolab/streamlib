# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Where a running `tatolabd` keeps its live files, and the reader that finds them agreeing.

`nodes` finds what a runtime wrote only if the lend's reader resolves the
runtime directory exactly as the engine does. `tatolabd` opens its iceoryx2
node and its surface socket as it builds the engine, before the load, so the
agreement is checked against a real `tatolabd` held mid-load, with no device.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from collections.abc import Callable
from pathlib import Path

import pytest

from conftest import PrivateRuntimeDirectories, environment_overlaid_with, environment_reaching_no_vulkan_driver
from node_module_whose_describe_holds_the_load import NodeModuleWhoseDescribeHoldsTheLoad
from runtime_process_under_test import RuntimeProcessUnderTest
from runtime_unit_under_test import SUITE_VENV_INTERPRETER, RuntimeUnitUnderTest

PER_USER_FALLBACK = Path("/tmp") / f"streamlib-{os.getuid()}"

READER_REPORTING_THE_RESOLVED_RUNTIME_DIRECTORY = (
    "from tatolab.runtime._node_registry import runtime_directory\nprint(runtime_directory())\n"
)

READER_TIMEOUT_SECONDS = 60.0


def runtime_directory_the_lends_reader_resolves(
    runtime_unit: RuntimeUnitUnderTest, environment: "dict[str, str]"
) -> Path:
    """The runtime directory the lent `tatolab.runtime` resolves in `environment`."""
    finished = subprocess.run(
        [str(SUITE_VENV_INTERPRETER), "-c", READER_REPORTING_THE_RESOLVED_RUNTIME_DIRECTORY],
        env={**environment, "PYTHONPATH": str(runtime_unit.lend_directory)},
        capture_output=True,
        text=True,
        timeout=READER_TIMEOUT_SECONDS,
        check=False,
    )
    assert finished.returncode == 0, finished.stderr
    return Path(finished.stdout.strip().splitlines()[-1])


def iceoryx2_node_details_in(runtime_directory: Path) -> "set[Path]":
    return set((runtime_directory / "iox2" / "nodes").glob("*/*node.details"))


def surface_sockets_in(runtime_directory: Path) -> "set[Path]":
    return set(runtime_directory.glob("surface-share-*.sock"))


@pytest.mark.parametrize("xdg_runtime_dir_arm", ["set", "empty", "unset"])
def test_the_reader_resolves_the_directory_a_runtime_opened_its_domain_in(
    start_tatolabd: "Callable[..., RuntimeProcessUnderTest]",
    held_node_module: NodeModuleWhoseDescribeHoldsTheLoad,
    private_runtime_directories: PrivateRuntimeDirectories,
    runtime_unit: RuntimeUnitUnderTest,
    tmp_path: Path,
    xdg_runtime_dir_arm: str,
):
    """The engine's half of the agreement is what `tatolabd` actually created:
    its iceoryx2 domain and, on Linux, its surface socket. The reader must name
    the directory holding both."""
    xdg_runtime_directory_override: "dict[str, str | None]" = {
        "set": {},
        "empty": {"XDG_RUNTIME_DIR": ""},
        "unset": {"XDG_RUNTIME_DIR": None},
    }[xdg_runtime_dir_arm]
    resolved = runtime_directory_the_lends_reader_resolves(
        runtime_unit,
        environment_overlaid_with(private_runtime_directories.environment, xdg_runtime_directory_override),
    )
    node_details_before, sockets_before = iceoryx2_node_details_in(resolved), surface_sockets_in(resolved)

    tatolabd = start_tatolabd(
        {"nodes": [{"name": "held", "type": f"{held_node_module.name}:LoadedFrameRelay", "config": {}}]},
        project_directory=held_node_module.project_directory,
        extra_environment={
            **environment_reaching_no_vulkan_driver(tmp_path),
            **xdg_runtime_directory_override,
        },
    )
    assert held_node_module.wait_until_the_load_reaches_the_import(), (
        f"the load never reached the describe:\n{tatolabd.recent_stderr()}"
    )
    new_node_details = sorted(iceoryx2_node_details_in(resolved) - node_details_before)
    new_sockets = sorted(surface_sockets_in(resolved) - sockets_before)
    tatolabd.interrupt()
    tatolabd.await_exit()
    report = json.dumps(
        {
            "resolved_by_the_reader": str(resolved),
            "new_node_details": [str(details) for details in new_node_details],
            "new_sockets": [str(socket) for socket in new_sockets],
        }
    )

    if xdg_runtime_dir_arm == "set" and sys.platform == "linux":
        assert resolved == private_runtime_directories.xdg_runtime_directory / "streamlib"
    else:
        assert resolved == PER_USER_FALLBACK
    assert [details.name for details in new_node_details] == [f"sl{os.getuid()}_node.details"], (
        f"tatolabd's iceoryx2 node must be in {resolved / 'iox2'}: {report}"
    )
    if sys.platform == "linux":
        assert len(new_sockets) == 1, f"tatolabd's surface socket must be in {resolved}: {report}"
