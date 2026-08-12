# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A processor's identity is its class's import path — and `add` refuses one no
interpreter could import.

Every Python processor runs in its own child process, which reaches the class by
importing it. A class the child cannot import has no host anywhere, so the
refusal belongs at `add` — where the author is naming the class — rather than at
spawn, where it would surface as a failed child.

The other half is that an accepted name does not move. Identity is what the
registry, the control plane and the helper spawn all agree on, so one that
varied with how the user happened to start their app would be three processors
wearing one name.
"""

import re
import socket
from pathlib import Path

import pytest

from app_under_test import (
    start_app,
    start_app_as_module,
    start_app_under_the_streamlib_cli,
)
from identity_stability_app import DIRECT_LAUNCH_ARGUMENT

ENTRY_FILE_PROCESSOR_APP = Path(__file__).parent / "entry_file_processor_app.py"
IDENTITY_STABILITY_APP = Path(__file__).parent / "identity_stability_app.py"
TWO_PROCESSOR_IDENTITY_APP = Path(__file__).parent / "two_processor_identity_app.py"

# The engine's own registration record. Asserting on `__module__` from the app
# would agree with a derivation that never ran.
DERIVED_IDENTITY_PATTERN = re.compile(r'processor_class_import_path="([^"]+)"')


@pytest.fixture
def entry_file_processor_app(start_app_under_test):
    """Starts this suite's app; the shared fixture owns the cleanup."""
    return lambda scenario: start_app_under_test(ENTRY_FILE_PROCESSOR_APP, scenario)


def _refusal_marker(app) -> str:
    refusals = [marker for marker in app.markers() if marker.startswith("REFUSED=")]
    assert refusals, f"expected a refusal; markers: {app.markers()}\n{app.output}"
    return refusals[0]


def test_a_processor_declared_in_the_entry_file_is_refused(entry_file_processor_app):
    """`__main__:Type` names the child's own entry file, not the user's class."""
    app = entry_file_processor_app("entry_file_class_is_refused")
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    refusal = _refusal_marker(app)
    assert "__main__:EntryFileProcessor" in refusal, (
        f"the refusal must show the unimportable identity: {refusal}"
    )
    assert "importable module" in refusal, (
        f"the refusal must name the fix, not just the problem: {refusal}"
    )
    assert "ACCEPTED" not in app.markers()


def test_a_processor_declared_inside_a_function_is_refused(entry_file_processor_app):
    """`<locals>` marks a class that exists only for the duration of a call."""
    app = entry_file_processor_app("function_local_class_is_refused")
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    refusal = _refusal_marker(app)
    assert "<locals>" in refusal, (
        f"the refusal must name what makes the class unimportable: {refusal}"
    )
    assert "config=" in refusal, (
        f"the refusal must name how to pass what the closure captured: {refusal}"
    )


def test_the_same_class_in_an_importable_module_is_accepted(entry_file_processor_app):
    """The fix the refusal names is the whole difference — one import line."""
    app = entry_file_processor_app("importable_class_is_accepted")
    app.await_marker("ACCEPTED")
    app.await_clean_exit()

    assert not [marker for marker in app.markers() if marker.startswith("REFUSED=")], (
        f"an importable class must not be refused; output:\n{app.output}"
    )


def _free_port() -> int:
    """A port the OS reports free. The control plane increments on collision,
    so a caller that loses the race still binds nearby."""
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def _derived_identities(app) -> list[str]:
    """Every identity the engine derived, read off its own log records."""
    app.await_marker("ADDED")
    return [
        found.group(1)
        for line in app.output_lines
        if (found := DERIVED_IDENTITY_PATTERN.search(line))
    ]


def _derived_identity(app) -> str:
    """The identity the engine derived, read off its own log record."""
    derived = _derived_identities(app)
    if derived:
        return derived[0]
    raise AssertionError(
        f"the engine logged no derived identity; output:\n{app.output}"
    )


def _identity_under(launcher, start_app_under_test, *arguments: str) -> str:
    app = start_app_under_test(
        IDENTITY_STABILITY_APP, *arguments, launcher=launcher
    )
    return _derived_identity(app)


# Every arm is observed at `add`, which is where identity is derived — well
# before `run()` would initialize a GPU context. That is what keeps the launcher
# arm, which does go on to boot a node, off the rig: the fixture reaps its
# process group, so nothing here waits for a device or a clean exit.
def test_a_class_run_as_a_script_identifies_by_its_module(start_app_under_test):
    assert (
        _identity_under(start_app, start_app_under_test, DIRECT_LAUNCH_ARGUMENT)
        == "identity_stable_processor:IdentityStableProcessor"
    )


def test_the_launch_arrangement_never_changes_the_identity(start_app_under_test):
    """`python app.py`, `python -m app`, and `streamlib dev` — one name.

    Three arrangements that put a different thing on `sys.path` and give the
    entry file a different provenance. What must not move is the *processor's*
    module, because that is what the helper imports and what the registry keys
    on — and the class lives in an importable module in all three, which is the
    property the entry-file refusal above exists to guarantee.
    """
    as_a_script = _identity_under(
        start_app, start_app_under_test, DIRECT_LAUNCH_ARGUMENT
    )
    as_a_module = _identity_under(
        start_app_as_module, start_app_under_test, DIRECT_LAUNCH_ARGUMENT
    )
    under_the_launcher = _identity_under(
        start_app_under_the_streamlib_cli,
        start_app_under_test,
        "--port",
        str(_free_port()),
    )

    assert as_a_script == as_a_module == under_the_launcher, (
        f"one class, three launch arrangements, three names: "
        f"script={as_a_script!r} module={as_a_module!r} "
        f"launcher={under_the_launcher!r}"
    )


def test_two_classes_in_one_graph_register_under_two_distinct_paths(
    start_app_under_test,
):
    """A graph is keyed per class, not per app.

    The registry key used to be a synthesized org/package/type triple, which
    two classes could share; it is now each class's own module path, which they
    cannot. Asserted as an ordered pair of literals: comparing the two to each
    other would pass on any pair of distinct strings, including two the engine
    derived the same wrong way.
    """
    app = start_app_under_test(TWO_PROCESSOR_IDENTITY_APP, launcher=start_app)
    assert _derived_identities(app) == [
        "identity_stable_processor:IdentityStableProcessor",
        "second_identity_stable_processor:SecondIdentityStableProcessor",
    ]
