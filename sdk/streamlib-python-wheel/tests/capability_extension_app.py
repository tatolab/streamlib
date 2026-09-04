# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that construct a `Runtime()` with test extensions installed.

Driven as a real `python app.py` because the loop runs once per process: a
second `Runtime()` re-runs nothing, so one process can prove one outcome. The
extensions are put on `sys.path` here rather than installed into the venv — the
raising variant would otherwise fail every other test's `Runtime()`.

`sys.path.append`, never `insert`: the helper spawn host reads `sys.path[0]` to
tell a child where the app's own modules live, and PYTHONPATH carries the
fixture to that child.
"""

import importlib
import json
import os
import sys
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
    """Put `variants` where `importlib.metadata` and a helper child both see them."""
    directories = [str(FIXTURES / variant) for variant in variants]
    sys.path.extend(directories)
    already_on_the_path = os.environ.get("PYTHONPATH", "")
    os.environ["PYTHONPATH"] = os.pathsep.join(
        [*directories, already_on_the_path] if already_on_the_path else directories
    )
    importlib.invalidate_caches()


def scenario_a_hook_runs_and_registers() -> None:
    install_fixture_distributions("registering")
    import streamlib

    runtime = streamlib.Runtime()
    host = sys.modules["streamlib_test_extension"].hosts_the_hook_was_handed[-1]
    marker(f"HOST_ROLE={host.role}")
    runtime.shutdown()
    marker("CLEAN_EXIT")


def scenario_the_hook_runs_once_however_many_runtimes() -> None:
    install_fixture_distributions("registering")
    import streamlib

    first = streamlib.Runtime()
    first.shutdown()
    second = streamlib.Runtime()
    second.shutdown()

    hooks = sys.modules["streamlib_test_extension"].hosts_the_hook_was_handed
    marker(f"HOOK_CALL_COUNT={len(hooks)}")
    marker("CLEAN_EXIT")


def scenario_a_raising_hook_fails_the_runtime() -> None:
    install_fixture_distributions("raising")
    import streamlib

    try:
        streamlib.Runtime()
    except Exception as construction_failure:
        marker(f"RUNTIME_REFUSED={construction_failure}")
    else:
        marker("RUNTIME_REFUSED=nothing was raised")
    marker("CLEAN_EXIT")


def scenario_a_raising_hook_keeps_failing_every_later_runtime() -> None:
    install_fixture_distributions("raising")
    import streamlib

    refusals = 0
    for _ in range(2):
        try:
            streamlib.Runtime()
        except Exception:
            refusals += 1
    marker(f"REFUSAL_COUNT={refusals}")
    marker(f"HOOK_CALL_COUNT={sys.modules['streamlib_raising_extension'].hook_call_count}")
    marker("CLEAN_EXIT")


def scenario_two_distributions_on_one_capability_name() -> None:
    install_fixture_distributions("registering", "duplicate")
    import streamlib

    try:
        streamlib.Runtime()
    except Exception as construction_failure:
        marker(f"RUNTIME_REFUSED={construction_failure}")
    else:
        marker("RUNTIME_REFUSED=nothing was raised")
    marker("CLEAN_EXIT")


def scenario_a_helper_runs_the_hook_before_the_processor() -> None:
    """A real helper spawn: the hook runs in the child, before its import."""
    install_fixture_distributions("registering")
    import streamlib
    from capability_extension_processor import ReportsTheExtensionItsHelperLoaded

    runtime = streamlib.Runtime()
    runtime.add(ReportsTheExtensionItsHelperLoaded)

    def stop_once_the_helper_has_reported() -> None:
        runtime.wait_until_every_processor_is_running(timeout=60.0)
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
    import streamlib
    from capability_extension_processor import ReportsTheExtensionItsHelperLoaded

    runtime = streamlib.Runtime()
    runtime.add(ReportsTheExtensionItsHelperLoaded)

    def report_whether_the_processor_ever_started() -> None:
        try:
            runtime.wait_until_every_processor_is_running(timeout=30.0)
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
    import streamlib
    from capability_extension_processor import ReportsTheExtensionItsHelperLoaded
    from streamlib._control_plane_client import call_tool
    from streamlib._node_registry import live_nodes

    runtime = streamlib.Runtime()
    runtime.host_control_plane()
    runtime.add(ReportsTheExtensionItsHelperLoaded)

    def report_the_extensions_the_graph_carries() -> None:
        runtime.wait_until_every_processor_is_running(timeout=60.0)
        control_url = next(
            node.control_url for node in live_nodes() if node.pid == os.getpid()
        )
        graph = json.loads(call_tool(control_url, "graph", {}))
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
