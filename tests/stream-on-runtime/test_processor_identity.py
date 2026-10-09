# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A processor's identity is its class's import path — and `stream_builder.add` refuses one
no interpreter could import.

Every Python processor runs in its own child process, which reaches the class by
importing it. A class the child cannot import has no host anywhere, so the
refusal belongs at `stream_builder.add` — where the author is naming the class — rather
than at spawn, where it would surface as a failed child.

The other half is that an accepted name does not move. Identity is what the
registry, the control plane and the helper spawn all agree on, so one that
varied with how the user named their stream to `tatolab run` would be three
processors wearing one name.

Every arm is observed at the load, which is where identity is derived — before
the engine would initialize a GPU context. Each runs with no Vulkan driver
reachable, so a stream that loads is refused at the GPU, before any device opens.
"""

from __future__ import annotations

import re
from collections.abc import Callable
from pathlib import Path
from typing import Any

from conftest import NO_VULKAN_DRIVER_REFUSAL, StreamGraphLoadOutcome, environment_reaching_no_vulkan_driver
from identity_stable_processor import IdentityStableProcessor
from runtime_process_under_test import STREAM_LOADED_LOG_LINE_PATTERN, RuntimeProcessUnderTest
from second_identity_stable_processor import SecondIdentityStableProcessor
from tatolab.stream import StreamBuilder, stream

# The engine's own registration record. Asserting on `__module__` from the test
# would agree with a derivation that never ran.
DERIVED_IDENTITY_PATTERN = re.compile(r'processor_class_import_path="([^"]+)"')

#: The engine's message on that record, written for each node type a stream's
#: processor interpreter described into the stream; a built-in's says otherwise.
PROCESSOR_TYPE_REGISTERED_LOG_LINE_FRAGMENT = (
    "node type described in the stream's processor interpreter registered"
)

IDENTITY_STABLE_PROCESSOR_FILE = Path(__file__).with_name("identity_stable_processor.py")

IDENTITY_STABILITY_STREAM_SOURCE = '''\
"""One stream, named to `tatolab run` three ways, adding one class."""

from identity_stable_processor import IdentityStableProcessor
from tatolab.stream import StreamBuilder, stream


@stream
def identity_stability(stream_builder: StreamBuilder) -> None:
    """The stream every launch arrangement loads."""
    stream_builder.add(IdentityStableProcessor)
'''

ENTRY_FILE_PROCESSOR_STREAM_SOURCE = '''\
from tatolab.stream import StreamBuilder, node, stream


@node(execution="continuous", interval_ms=1)
class EntryFileProcessor:
    """Declared in the entry file, which is exactly what makes it unhostable."""

    def process(self, ctx) -> None: ...


@stream
def a_processor_defined_in_the_entry_file(stream_builder: StreamBuilder) -> None:
    stream_builder.add(EntryFileProcessor)
'''

FUNCTION_LOCAL_PROCESSOR_STREAM_SOURCE = '''\
from tatolab.stream import StreamBuilder, node, stream


@stream
def a_function_local_processor(stream_builder: StreamBuilder) -> None:
    def build_processor() -> type:
        @node(execution="continuous", interval_ms=1)
        class FunctionLocalProcessor:
            def process(self, ctx) -> None: ...

        return FunctionLocalProcessor

    stream_builder.add(build_processor())
'''

IMPORTABLE_PROCESSOR_STREAM_SOURCE = '''\
from tatolab.stream import StreamBuilder, stream


@stream
def an_importable_processor(stream_builder: StreamBuilder) -> None:
    """The same stream, one import line different — the fix the refusal names."""
    from zero_argument_process_processor import ZeroArgumentProcess

    stream_builder.add(ZeroArgumentProcess)
'''

ZERO_ARGUMENT_PROCESS_PROCESSOR_SOURCE = '''\
from tatolab.stream import node


@node(execution="continuous", interval_ms=1)
class ZeroArgumentProcess:
    def process(self, ctx) -> None: ...
'''

MakeTatolabProject = Callable[..., Path]
RunTatolab = Callable[..., Any]
StartTatolab = Callable[..., RuntimeProcessUnderTest]


@stream
def two_identity_stable_processors(stream_builder: StreamBuilder) -> None:
    """Both processors, in one stream."""
    stream_builder.add(IdentityStableProcessor)
    stream_builder.add(SecondIdentityStableProcessor)


def identities_the_engine_logged(stderr_text: str) -> "list[str]":
    """Every identity the engine derived for the stream's Python node types, off
    its own registration records, in order."""
    return [
        found.group(1)
        for line in stderr_text.splitlines()
        if PROCESSOR_TYPE_REGISTERED_LOG_LINE_FRAGMENT in line
        and (found := DERIVED_IDENTITY_PATTERN.search(line))
    ]


def run_the_stream_entry(
    make_tatolab_project: MakeTatolabProject,
    run_tatolab: RunTatolab,
    tmp_path: Path,
    files: "dict[str, str]",
):
    """`tatolab run` a project holding `files`, with no Vulkan driver reachable."""
    project_directory = make_tatolab_project(files)
    return run_tatolab(
        "run",
        working_directory=project_directory,
        extra_environment=environment_reaching_no_vulkan_driver(tmp_path),
    )


def test_a_processor_declared_in_the_entry_file_is_refused(
    make_tatolab_project: MakeTatolabProject, run_tatolab: RunTatolab, tmp_path: Path
):
    """`__main__:Type` names the child's own entry file, not the user's class."""
    finished = run_the_stream_entry(
        make_tatolab_project, run_tatolab, tmp_path, {"stream.py": ENTRY_FILE_PROCESSOR_STREAM_SOURCE}
    )

    assert finished.returncode != 0, finished.stderr
    assert "__main__:EntryFileProcessor" in finished.stderr, (
        f"the refusal must show the unimportable identity:\n{finished.stderr}"
    )
    assert "importable module" in finished.stderr, (
        f"the refusal must name the fix, not just the problem:\n{finished.stderr}"
    )
    assert STREAM_LOADED_LOG_LINE_PATTERN.search(finished.stderr) is None, finished.stderr


def test_a_processor_declared_inside_a_function_is_refused(
    make_tatolab_project: MakeTatolabProject, run_tatolab: RunTatolab, tmp_path: Path
):
    """`<locals>` marks a class that exists only for the duration of a call."""
    finished = run_the_stream_entry(
        make_tatolab_project, run_tatolab, tmp_path, {"stream.py": FUNCTION_LOCAL_PROCESSOR_STREAM_SOURCE}
    )

    assert finished.returncode != 0, finished.stderr
    assert "<locals>" in finished.stderr, (
        f"the refusal must name what makes the class unimportable:\n{finished.stderr}"
    )
    assert "config=" in finished.stderr, (
        f"the refusal must name how to pass what the closure captured:\n{finished.stderr}"
    )


def test_the_same_class_in_an_importable_module_is_accepted(
    make_tatolab_project: MakeTatolabProject, run_tatolab: RunTatolab, tmp_path: Path
):
    """The fix the refusal names is the whole difference — one import line."""
    finished = run_the_stream_entry(
        make_tatolab_project,
        run_tatolab,
        tmp_path,
        {
            "stream.py": IMPORTABLE_PROCESSOR_STREAM_SOURCE,
            "zero_argument_process_processor.py": ZERO_ARGUMENT_PROCESS_PROCESSOR_SOURCE,
        },
    )

    assert STREAM_LOADED_LOG_LINE_PATTERN.search(finished.stderr) is not None, (
        f"an importable class must not be refused:\n{finished.stderr}"
    )
    assert NO_VULKAN_DRIVER_REFUSAL in finished.stderr, finished.stderr


def identity_under(
    make_tatolab_project: MakeTatolabProject,
    start_tatolab: StartTatolab,
    tmp_path: Path,
    *run_arguments: str,
) -> str:
    """The identity the engine derived for the one class, the stream named to
    `tatolab run` by `run_arguments`."""
    project_directory = make_tatolab_project(
        {
            "identity_stability.py": IDENTITY_STABILITY_STREAM_SOURCE,
            "identity_stable_processor.py": IDENTITY_STABLE_PROCESSOR_FILE.read_text(),
        },
        directory_name=re.sub(r"[^A-Za-z0-9_.-]", "-", "-".join(["project", *run_arguments])),
    )
    tatolab = start_tatolab(
        "run",
        *run_arguments,
        working_directory=project_directory,
        extra_environment=environment_reaching_no_vulkan_driver(tmp_path),
    )
    tatolab.await_stream_loaded()
    tatolab.await_exit()
    identities = identities_the_engine_logged(tatolab.stderr_text)
    assert identities, f"the engine logged no derived identity:\n{tatolab.recent_stderr()}"
    return identities[0]


def test_a_class_run_as_a_script_identifies_by_its_module(
    make_tatolab_project: MakeTatolabProject, start_tatolab: StartTatolab, tmp_path: Path
):
    assert (
        identity_under(make_tatolab_project, start_tatolab, tmp_path, "identity_stability.py")
        == "identity_stable_processor:IdentityStableProcessor"
    )


def test_the_launch_arrangement_never_changes_the_identity(
    make_tatolab_project: MakeTatolabProject, start_tatolab: StartTatolab, tmp_path: Path
):
    """`tatolab run <file>.py`, `tatolab run <module>:<function>` and `tatolab run -f <file>.py` — one name.

    Three arrangements that reach the entry differently: a file executed as
    `__main__`, a module imported by name, and a file named in place of the
    `stream.py` convention. What must not move is the *processor's* module,
    because that is what the helper imports and what the registry keys on —
    and the class lives in an importable module in all three, which is the
    property the entry-file refusal above exists to guarantee.
    """
    as_a_file_target = identity_under(
        make_tatolab_project, start_tatolab, tmp_path, "identity_stability.py"
    )
    as_a_module_target = identity_under(
        make_tatolab_project, start_tatolab, tmp_path, "identity_stability:identity_stability"
    )
    as_the_named_entry_file = identity_under(
        make_tatolab_project, start_tatolab, tmp_path, "-f", "identity_stability.py"
    )

    assert as_a_file_target == as_a_module_target == as_the_named_entry_file, (
        f"one class, three launch arrangements, three names: "
        f"file={as_a_file_target!r} module={as_a_module_target!r} "
        f"entry file={as_the_named_entry_file!r}"
    )


def test_two_classes_in_one_graph_register_under_two_distinct_paths(
    load_stream_graph_on_tatolabd: "Callable[..., StreamGraphLoadOutcome]",
):
    """A graph is keyed per class, not per app.

    The registry key used to be a synthesized org/package/type triple, which
    two classes could share; it is now each class's own module path, which they
    cannot. Asserted as an ordered pair of literals: comparing the two to each
    other would pass on any pair of distinct strings, including two the engine
    derived the same wrong way.
    """
    load_outcome = load_stream_graph_on_tatolabd(two_identity_stable_processors)
    assert load_outcome.loaded, load_outcome.stderr_text[-4000:]
    assert identities_the_engine_logged(load_outcome.stderr_text) == [
        "identity_stable_processor:IdentityStableProcessor",
        "second_identity_stable_processor:SecondIdentityStableProcessor",
    ]

