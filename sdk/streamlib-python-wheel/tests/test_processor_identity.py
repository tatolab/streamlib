# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A processor's identity is its class's import path — and `add` refuses one no
interpreter could import.

Every Python processor runs in its own child process, which reaches the class by
importing it. A class the child cannot import has no host anywhere, so the
refusal belongs at `add` — where the author is naming the class — rather than at
spawn, where it would surface as a failed child.
"""

from pathlib import Path

import pytest

ENTRY_FILE_PROCESSOR_APP = Path(__file__).parent / "entry_file_processor_app.py"


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
