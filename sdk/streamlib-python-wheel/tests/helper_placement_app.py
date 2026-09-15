# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that prove where a Python processor actually runs.

Run as a real `python app.py` because the claim is about processes: only a
parent can see that the app's interpreter never loaded a second copy of the
processor's module, and that the pid a bag was produced in is not the app's.
"""

import os
import shutil
import sys
import tempfile
import threading
from pathlib import Path

import streamlib
from helper_placement_processors import (
    DiesAbruptlyProbe,
    ReportsItsOwnProcessesProcessorCatalog,
    ReportsItsOwnProcessSource,
    ReportsItsOwnProcessVideoSink,
    ReportsUpstreamProcessSink,
)
from streamlib._engine import (
    engine_build_id_compiled_into_this_extension,
    processor_class_import_paths_in_this_processes_catalog,
)

MARKER_PREFIX = "MARKER:"

# Long enough for both children to boot and pass bags before the app re-reads
# its own `sys.modules`. The check is about what the *parent* loaded, so it only
# needs the graph to have been running, not to have run long.
SECONDS_OF_RUNNING_BEFORE_RECHECK = 4.0


def marker(name: str) -> None:
    print(f"{MARKER_PREFIX}{name}", flush=True)


def scenario_the_app_never_hosts_the_processor() -> None:
    """`rt.add` loads nothing into the app's own `sys.modules`.

    The app's own `from helper_placement_processors import …` is the
    registration import and the only parent-side load there is. What must not
    happen is the engine loading anything more to host an instance —
    `streamlib._helper` constructs the class, and it lives in another process.
    """
    modules_before_add = set(sys.modules)
    runtime = streamlib.Runtime()
    source = runtime.add(ReportsItsOwnProcessSource, config={"label": "first"})
    sink = runtime.add(ReportsUpstreamProcessSink)
    runtime.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )
    marker(f"MODULES_ADDED_BY_ADD={sorted(set(sys.modules) - modules_before_add)}")

    # Checked again after bags have actually crossed, not only after `rt.add`:
    # a host that constructed the class lazily — on the first frame rather than
    # at graph build — would pass the first check and fail this one.
    def report_modules_once_bags_are_flowing() -> None:
        marker(
            f"MODULES_ADDED_WHILE_RUNNING="
            f"{sorted(set(sys.modules) - modules_before_add)}"
        )
        marker(f"HELPER_MODULE_IN_APP={'streamlib._helper' in sys.modules}")
        runtime.shutdown()

    threading.Timer(SECONDS_OF_RUNNING_BEFORE_RECHECK, report_modules_once_bags_are_flowing).start()
    runtime.run()
    marker("CLEAN_EXIT")


def scenario_a_bag_is_produced_in_another_process() -> None:
    """A source and a sink are two children, and the app is neither."""
    runtime = streamlib.Runtime()
    source = runtime.add(ReportsItsOwnProcessSource, config={"label": "only"})
    sink = runtime.add(ReportsUpstreamProcessSink)
    runtime.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )
    marker(f"APP_PID={os.getpid()}")
    # Runs until the test has seen what it came for and interrupts — the
    # children report in milliseconds, so a timer here would only be a guess
    # about how slow the machine is.
    runtime.run()
    marker("CLEAN_EXIT")


def scenario_two_instances_of_one_class_get_two_processes() -> None:
    """Two `rt.add` calls on one class are two children, not two objects."""
    runtime = streamlib.Runtime()
    for label in ("first", "second"):
        source = runtime.add(ReportsItsOwnProcessSource, config={"label": label})
        sink = runtime.add(ReportsUpstreamProcessSink, display_name=f"{label}Sink")
        runtime.connect(
            source.output("frames_to_downstream"), sink.input("frames_from_upstream")
        )
    marker(f"APP_PID={os.getpid()}")
    # Runs until the test has seen what it came for and interrupts — the
    # children report in milliseconds, so a timer here would only be a guess
    # about how slow the machine is.
    runtime.run()
    marker("CLEAN_EXIT")


def scenario_a_native_builtin_stays_in_the_app_process() -> None:
    """A native built-in and a Python processor, so the boundary has two sides.

    The native source is statically linked and runs on an engine thread; the
    Python sink runs in a child. Nothing spawns for the source, which is the
    observable form of "it runs in the app's process".
    """
    runtime = streamlib.Runtime()
    pattern = runtime.add(
        streamlib.TestPatternSource, config={"width": 64, "height": 32}
    )
    sink = runtime.add(ReportsItsOwnProcessVideoSink)
    runtime.connect(pattern.output("video"), sink.input("video_from_upstream"))
    marker(f"APP_PID={os.getpid()}")
    runtime.run()
    marker("CLEAN_EXIT")


def scenario_every_child_is_reaped() -> None:
    """`rt.run()` returning means no helper outlived it.

    The spawn host reports each child's pid as it starts one; the test is what
    checks those pids are gone once the app has exited.
    """
    runtime = streamlib.Runtime()
    source = runtime.add(ReportsItsOwnProcessSource, config={"label": "reaped"})
    sink = runtime.add(ReportsUpstreamProcessSink)
    runtime.connect(
        source.output("frames_to_downstream"), sink.input("frames_from_upstream")
    )
    runtime.run()
    marker("CLEAN_EXIT")


def scenario_a_crashed_helper_leaves_the_pipeline_running() -> None:
    """One processor takes its own process down; the others keep going.

    The owner's crash policy is surface-and-keep-running: the graph shows the
    dead one in error, the rest of the pipeline is unaffected, and the frame it
    had in flight is lost rather than silently replayed.
    """
    runtime = streamlib.Runtime()
    runtime.add(DiesAbruptlyProbe)
    survivor_source = runtime.add(ReportsItsOwnProcessSource, config={"label": "survivor"})
    survivor_sink = runtime.add(ReportsUpstreamProcessSink)
    runtime.connect(
        survivor_source.output("frames_to_downstream"),
        survivor_sink.input("frames_from_upstream"),
    )
    marker(f"APP_PID={os.getpid()}")
    runtime.run()
    marker("CLEAN_EXIT")


def scenario_a_helper_registers_nothing_it_imports() -> None:
    """The class is in the app's catalog from its import, and in no child's.

    The app side is the decorator's whole point — the class is discoverable
    without ever being added. The child side is the other half: it imports the
    same module to host the class and must register nothing, because a helper
    hosts no graph.
    """
    marker(
        f"APP_CATALOG_HAS_THE_CLASS="
        f"{'helper_placement_processors:ReportsItsOwnProcessesProcessorCatalog' in processor_class_import_paths_in_this_processes_catalog()}"
    )
    runtime = streamlib.Runtime()
    runtime.add(ReportsItsOwnProcessesProcessorCatalog)
    marker(f"APP_PID={os.getpid()}")
    runtime.run()
    marker("CLEAN_EXIT")


#: A build id no build of this checkout mints: its nonce is all zeros.
ENGINE_BUILD_ID_OF_ANOTHER_BUILD = (
    "0.0.1+0123456789abcdef0123456789abcdef01234567.00000000000000000000000000000000"
)


def scenario_a_helper_that_imported_another_engine_build_is_refused() -> None:
    """A child whose engine is not the app's is refused, and the refusal says why.

    The child is made to see another build the one way a test can reach it
    before `main()` runs: a `sitecustomize` on the child's `PYTHONPATH` rewrites
    the id the parent handed it, which is what a stale `streamlib` earlier on
    the child's `sys.path` amounts to from the check's side.
    """
    child_startup_directory = Path(tempfile.mkdtemp(prefix="streamlib-stale-build-"))
    (child_startup_directory / "sitecustomize.py").write_text(
        "import os\n"
        "if 'STREAMLIB_ENTRYPOINT' in os.environ:\n"
        f"    os.environ['STREAMLIB_ENGINE_BUILD_ID'] = {ENGINE_BUILD_ID_OF_ANOTHER_BUILD!r}\n"
    )
    inherited_python_path = os.environ.get("PYTHONPATH")
    os.environ["PYTHONPATH"] = os.pathsep.join(
        entry for entry in (str(child_startup_directory), inherited_python_path) if entry
    )

    runtime = streamlib.Runtime()
    runtime.add(ReportsItsOwnProcessSource, config={"label": "stale"})
    marker(f"APP_ENGINE_BUILD_ID={engine_build_id_compiled_into_this_extension()}")

    def report_whether_the_processor_ever_started() -> None:
        try:
            runtime.wait_until_every_processor_is_running(timeout=30.0)
        except RuntimeError as never_started:
            marker(f"PROCESSOR_REFUSED={never_started}")
        else:
            marker("PROCESSOR_REFUSED=it started anyway")
        runtime.shutdown()

    threading.Thread(target=report_whether_the_processor_ever_started, daemon=True).start()
    try:
        runtime.run()
    finally:
        shutil.rmtree(child_startup_directory, ignore_errors=True)
    marker("CLEAN_EXIT")


if __name__ == "__main__":
    globals()[f"scenario_{sys.argv[1]}"]()
