# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that construct a `Runtime()` with test extensions installed.

Driven as its own `python <script>.py` process because the loop runs once per
process: a second `Runtime()` re-runs nothing, so one process can prove one
outcome. The extensions are put on `sys.path` here rather than installed into
the venv — the raising variant would otherwise fail every other test's
`Runtime()`.

A processor interpreter's `PYTHONPATH` is the lend directory and then the
project directory, so a scenario that spawns one loads its stream from a
project directory holding the fixture distributions beside the processor's
own module.
"""

import atexit
import importlib
import json
import shutil
import sys
import tempfile
import threading
import time
from pathlib import Path

FIXTURES = Path(__file__).parent / "extension_fixtures"

MARKER_PREFIX = "MARKER:"

#: Long enough for the helper's first `process()` to report, short enough that
#: a rig run of this suite stays quick.
SECONDS_OF_RUNNING_BEFORE_SHUTDOWN = 2.0


def marker(name: str) -> None:
    print(f"{MARKER_PREFIX}{name}", flush=True)


def install_fixture_distributions(*variants: str) -> None:
    """Put `variants` where this process's `importlib.metadata` sees them."""
    sys.path.extend(str(FIXTURES / variant) for variant in variants)
    importlib.invalidate_caches()


def project_directory_carrying_fixture_distributions(*variants: str) -> Path:
    """A project directory holding `variants` beside the processor's own module,
    where a processor interpreter's `importlib.metadata` and import both see them."""
    project_directory = Path(tempfile.mkdtemp(prefix="streamlib-extension-project-"))
    atexit.register(shutil.rmtree, project_directory, ignore_errors=True)
    for variant in variants:
        shutil.copytree(FIXTURES / variant, project_directory, dirs_exist_ok=True)
    shutil.copy2(Path(__file__).with_name("capability_extension_processor.py"), project_directory)
    return project_directory


def scenario_a_hook_runs_and_registers() -> None:
    install_fixture_distributions("registering")
    import tatolab.runtime

    runtime = tatolab.runtime.Runtime()
    host = sys.modules["streamlib_test_extension"].hosts_the_hook_was_handed[-1]
    marker(f"HOST_ROLE={host.role}")
    runtime.shutdown()
    marker("CLEAN_EXIT")


def scenario_the_hook_runs_once_however_many_runtimes() -> None:
    install_fixture_distributions("registering")
    import tatolab.runtime

    first = tatolab.runtime.Runtime()
    first.shutdown()
    second = tatolab.runtime.Runtime()
    second.shutdown()

    hooks = sys.modules["streamlib_test_extension"].hosts_the_hook_was_handed
    marker(f"HOOK_CALL_COUNT={len(hooks)}")
    marker("CLEAN_EXIT")


def scenario_a_raising_hook_fails_the_runtime() -> None:
    install_fixture_distributions("raising")
    import tatolab.runtime

    try:
        tatolab.runtime.Runtime()
    except Exception as construction_failure:
        marker(f"RUNTIME_REFUSED={construction_failure}")
    else:
        marker("RUNTIME_REFUSED=nothing was raised")
    marker("CLEAN_EXIT")


def scenario_a_raising_hook_keeps_failing_every_later_runtime() -> None:
    install_fixture_distributions("raising")
    import tatolab.runtime

    refusals = 0
    for _ in range(2):
        try:
            tatolab.runtime.Runtime()
        except Exception:
            refusals += 1
    marker(f"REFUSAL_COUNT={refusals}")
    marker(f"HOOK_CALL_COUNT={sys.modules['streamlib_raising_extension'].hook_call_count}")
    marker("CLEAN_EXIT")


def scenario_two_distributions_on_one_capability_name() -> None:
    install_fixture_distributions("registering", "duplicate")
    import tatolab.runtime

    try:
        tatolab.runtime.Runtime()
    except Exception as construction_failure:
        marker(f"RUNTIME_REFUSED={construction_failure}")
    else:
        marker("RUNTIME_REFUSED=nothing was raised")
    marker("CLEAN_EXIT")


def scenario_a_helper_runs_the_hook_before_the_processor() -> None:
    """A real helper spawn: the hook runs in the child, before its import."""
    install_fixture_distributions("registering")
    import tatolab.runtime
    import tatolab.stream
    from capability_extension_reporting_stream import (
        one_processor_that_reports_its_helpers_extensions,
    )

    graph = tatolab.stream.compile_stream_to_graph(
        one_processor_that_reports_its_helpers_extensions
    )
    runtime = tatolab.runtime.Runtime()
    runtime.load(
        graph,
        project_directory=project_directory_carrying_fixture_distributions("registering"),
        interpreter=sys.executable,
    )

    def stop_once_the_helper_has_reported() -> None:
        runtime.wait_until_every_node_is_running(timeout=60.0)
        time.sleep(SECONDS_OF_RUNNING_BEFORE_SHUTDOWN)
        runtime.shutdown()

    threading.Thread(target=stop_once_the_helper_has_reported, daemon=True).start()
    runtime.run()
    marker("CLEAN_EXIT")


def scenario_a_raising_hook_refuses_the_processor() -> None:
    """A hook that fails in the child takes that processor's start with it.

    The fixture raises only when `role` is `"helper"`: an extension that failed
    in the app process too would refuse `Runtime()` first, and this would never
    reach a helper at all.
    """
    install_fixture_distributions("helper_raising")
    import tatolab.runtime
    import tatolab.stream
    from capability_extension_reporting_stream import (
        one_processor_that_reports_its_helpers_extensions,
    )

    graph = tatolab.stream.compile_stream_to_graph(
        one_processor_that_reports_its_helpers_extensions
    )
    runtime = tatolab.runtime.Runtime()
    runtime.load(
        graph,
        project_directory=project_directory_carrying_fixture_distributions("helper_raising"),
        interpreter=sys.executable,
    )

    def report_whether_the_processor_ever_started() -> None:
        try:
            runtime.wait_until_every_node_is_running(timeout=30.0)
        except RuntimeError as never_started:
            marker(f"PROCESSOR_REFUSED={never_started}")
        else:
            marker("PROCESSOR_REFUSED=it started anyway")
        runtime.shutdown()

    threading.Thread(target=report_whether_the_processor_ever_started, daemon=True).start()
    runtime.run()
    marker("CLEAN_EXIT")


def scenario_graph_renders_the_registered_capability() -> None:
    """`streamlib graph` is where an operator sees what an install enabled.

    Read through this run's own control plane, which is the exact payload
    `GET /api/graph` and the MCP `graph` tool serve.
    """
    install_fixture_distributions("registering")
    import tatolab.runtime
    import tatolab.stream
    from capability_extension_reporting_stream import (
        one_processor_that_reports_its_helpers_extensions,
    )
    from tatolab.runtime._control_plane_client import call_tool
    from this_processes_node_registry_entry import this_processes_local_api_socket

    graph = tatolab.stream.compile_stream_to_graph(
        one_processor_that_reports_its_helpers_extensions
    )
    runtime = tatolab.runtime.Runtime()
    runtime.load(
        graph,
        project_directory=project_directory_carrying_fixture_distributions("registering"),
        interpreter=sys.executable,
    )
    runtime.host_control_plane()

    def report_the_extensions_the_graph_carries() -> None:
        runtime.wait_until_every_node_is_running(timeout=60.0)
        graph = json.loads(call_tool(this_processes_local_api_socket(), "graph", {}))
        marker(f"GRAPH_EXTENSIONS={json.dumps(graph['extensions'])}")
        runtime.shutdown()

    threading.Thread(target=report_the_extensions_the_graph_carries, daemon=True).start()
    runtime.run()
    marker("CLEAN_EXIT")


SCENARIOS = {
    "a_hook_runs_and_registers": scenario_a_hook_runs_and_registers,
    "the_hook_runs_once_however_many_runtimes": (
        scenario_the_hook_runs_once_however_many_runtimes
    ),
    "a_raising_hook_fails_the_runtime": scenario_a_raising_hook_fails_the_runtime,
    "a_raising_hook_keeps_failing_every_later_runtime": (
        scenario_a_raising_hook_keeps_failing_every_later_runtime
    ),
    "two_distributions_on_one_capability_name": (
        scenario_two_distributions_on_one_capability_name
    ),
    "a_helper_runs_the_hook_before_the_processor": (
        scenario_a_helper_runs_the_hook_before_the_processor
    ),
    "a_raising_hook_refuses_the_processor": scenario_a_raising_hook_refuses_the_processor,
    "graph_renders_the_registered_capability": (
        scenario_graph_renders_the_registered_capability
    ),
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]]()
