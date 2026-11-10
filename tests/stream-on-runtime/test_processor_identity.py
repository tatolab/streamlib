# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A processor's identity is its class's import path, and it does not move.

Every Python processor runs in its own child process, which reaches the class by
importing it. Identity is what the registry, the control plane and the helper
spawn all agree on, so one that varied with how the user named their stream to
`tatolab start` would be three processors wearing one name.

Every arm is observed at the load, which is where identity is derived — before
the engine would initialize a GPU context. Each runs on a `tatolabd` that
reaches no Vulkan driver, so a stream that loads fails at the GPU, before
any device opens, and the identities are read off `tatolabd`'s own log.
"""

from __future__ import annotations

import importlib.metadata
import re
from collections.abc import Callable
from pathlib import Path

from conftest import NO_VULKAN_DRIVER_REFUSAL, StreamRunWithNoVulkanDriverOutcome
from processor_identity_streams import two_identity_stable_processors
from stream_runs_on_tatolabd import TatolabRunOfAProject

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
"""One stream, adding one class."""

from identity_stable_processor import IdentityStableProcessor
from tatolab.stream import StreamBuilder, graph_generator


@graph_generator
def identity_stability(stream_builder: StreamBuilder) -> None:
    """The stream the project loads."""
    stream_builder.add(IdentityStableProcessor)
'''

IMPORTABLE_PROCESSOR_STREAM_SOURCE = '''\
from tatolab.stream import StreamBuilder, graph_generator


@graph_generator
def an_importable_processor(stream_builder: StreamBuilder) -> None:
    """The same stream, one import line different — the fix the refusal names."""
    from zero_argument_process_processor import ZeroArgumentProcess

    stream_builder.add(ZeroArgumentProcess)
'''

ZERO_ARGUMENT_PROCESS_PROCESSOR_SOURCE = '''\
from tatolab.stream import node


@node.process(execution="continuous", interval_ms=1)
class ZeroArgumentProcess:
    def process(self, ctx) -> None: ...
'''

MakeTatolabProject = Callable[..., Path]
RunStreamOnTatolabdWithNoVulkanDriver = Callable[..., StreamRunWithNoVulkanDriverOutcome]


#: What this suite's venv lists under `tatolab.nodes`. Every stream started from the
#: venv describes these into the stream beside its own classes.
NODE_TYPES_INSTALLED_MODULES_DECLARE = frozenset(
    entry_point.value for entry_point in importlib.metadata.entry_points(group="tatolab.nodes")
)


def identities_the_engine_logged(stderr_text: str) -> "list[str]":
    """Every identity the engine derived for the stream's own Python node types, off
    its own registration records, in order; installed modules' types are left out."""
    return [
        found.group(1)
        for line in stderr_text.splitlines()
        if PROCESSOR_TYPE_REGISTERED_LOG_LINE_FRAGMENT in line
        and (found := DERIVED_IDENTITY_PATTERN.search(line))
        and found.group(1) not in NODE_TYPES_INSTALLED_MODULES_DECLARE
    ]


def run_the_stream_entry(
    make_tatolab_project: MakeTatolabProject,
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
    files: "dict[str, str]",
) -> StreamRunWithNoVulkanDriverOutcome:
    """`tatolab start` a project holding `files`, on a `tatolabd` that reaches no Vulkan driver."""
    project_directory = make_tatolab_project(files)
    return run_stream_on_tatolabd_with_no_vulkan_driver(
        TatolabRunOfAProject(working_directory=project_directory)
    )


def test_the_same_class_in_an_importable_module_is_accepted(
    make_tatolab_project: MakeTatolabProject,
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    """A class the child imports from a module of the project loads."""
    run_outcome = run_the_stream_entry(
        make_tatolab_project,
        run_stream_on_tatolabd_with_no_vulkan_driver,
        {
            "stream.py": IMPORTABLE_PROCESSOR_STREAM_SOURCE,
            "zero_argument_process_processor.py": ZERO_ARGUMENT_PROCESS_PROCESSOR_SOURCE,
        },
    )

    assert run_outcome.loaded, (
        f"an importable class must not be refused:\n{run_outcome.tatolabd_stderr_text_during_the_run}"
    )
    assert NO_VULKAN_DRIVER_REFUSAL in (run_outcome.failed_or_refused_because or ""), run_outcome.tatolabd_stderr_text_during_the_run


def identity_under(
    make_tatolab_project: MakeTatolabProject,
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
) -> str:
    """The identity the engine derived for the one class of a project whose `stream.py` adds
    it."""
    project_directory = make_tatolab_project(
        {
            "stream.py": IDENTITY_STABILITY_STREAM_SOURCE,
            "identity_stable_processor.py": IDENTITY_STABLE_PROCESSOR_FILE.read_text(),
        },
    )
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(
        TatolabRunOfAProject(working_directory=project_directory)
    )
    assert run_outcome.loaded, run_outcome.tatolabd_stderr_text_during_the_run
    identities = identities_the_engine_logged(run_outcome.tatolabd_stderr_text_during_the_run)
    assert identities, (
        f"the engine logged no derived identity:\n{run_outcome.tatolabd_stderr_text_during_the_run}"
    )
    return identities[0]


def test_a_class_imported_by_the_stream_entry_identifies_by_its_module(
    make_tatolab_project: MakeTatolabProject,
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    assert (
        identity_under(make_tatolab_project, run_stream_on_tatolabd_with_no_vulkan_driver)
        == "identity_stable_processor:IdentityStableProcessor"
    )


def test_two_classes_in_one_graph_register_under_two_distinct_paths(
    run_stream_on_tatolabd_with_no_vulkan_driver: RunStreamOnTatolabdWithNoVulkanDriver,
):
    """A graph is keyed per class, not per app.

    The registry key used to be a synthesized org/package/type triple, which
    two classes could share; it is now each class's own module path, which they
    cannot. Asserted as an ordered pair of literals: comparing the two to each
    other would pass on any pair of distinct strings, including two the engine
    derived the same wrong way.
    """
    run_outcome = run_stream_on_tatolabd_with_no_vulkan_driver(two_identity_stable_processors)
    assert run_outcome.loaded, run_outcome.tatolabd_stderr_text_during_the_run
    assert identities_the_engine_logged(run_outcome.tatolabd_stderr_text_during_the_run) == [
        "identity_stable_processor:IdentityStableProcessor",
        "second_identity_stable_processor:SecondIdentityStableProcessor",
    ]

