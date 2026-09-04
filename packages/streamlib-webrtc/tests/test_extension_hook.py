# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The support hook pip records and the engine runs.

What this proves is this wheel's half of the contract — that the entry point is
installed, resolves, brings the stack up and registers this wheel's own name and
version, and that constructing a `Runtime()` is what runs it. That the engine
finds every installed hook, fails hard on one that raises, and renders the
result in `graph` is the mechanism's own proof, which lives with the mechanism
in the engine wheel's tests.

Nothing here calls `extension.load` against the real registry. The registry is
process-wide and refuses a second registration of one name, so a hand-made call
would make the engine's own run of the same hook fail in whatever test happened
to build a runtime next.
"""

import importlib.metadata
from typing import cast

import pytest

import streamlib
from streamlib import CapabilityExtensionHost
from streamlib._engine import capability_extension_host_for_the_app_process
from streamlib_webrtc import _native, extension

THIS_WHEEL = "streamlib-webrtc"


class HostRecordingWhatItWasHanded:
    """A stand-in for the door the engine passes in, so the hook's own contract
    can be checked without touching the process-wide registry."""

    def __init__(self) -> None:
        self.registered: "list[tuple[str, str]]" = []

    @property
    def role(self) -> str:
        return "app"

    def register_capability(self, name: str, version: str) -> None:
        self.registered.append((name, version))


def installed_hooks() -> "list[importlib.metadata.EntryPoint]":
    return [
        entry_point
        for entry_point in importlib.metadata.entry_points(group="streamlib.extensions")
        if entry_point.dist is not None and entry_point.dist.name == THIS_WHEEL
    ]


def test_pip_recorded_this_wheels_hook_under_the_engines_entry_point_group():
    hooks = installed_hooks()

    assert len(hooks) == 1, "one wheel declares one hook"
    assert hooks[0].name == extension.CAPABILITY_NAME
    assert hooks[0].value == "streamlib_webrtc.extension:load"


def test_the_recorded_hook_resolves_to_the_function_the_engine_will_call():
    assert installed_hooks()[0].load() is extension.load


def test_the_hook_registers_this_wheels_own_name_and_version():
    host = HostRecordingWhatItWasHanded()

    extension.load(cast(CapabilityExtensionHost, host))

    assert host.registered == [
        (extension.CAPABILITY_NAME, importlib.metadata.version(THIS_WHEEL))
    ]


def test_constructing_a_runtime_is_what_runs_this_wheels_hook():
    """The registration lands because the engine ran the hook, not because
    anything here called it — which is the whole of the mechanism's promise to
    an installed wheel."""
    runtime = streamlib.Runtime()
    try:
        # A second distribution claiming the name is refused, naming both. That
        # refusal is only reachable if the first registration happened.
        with pytest.raises(Exception, match=extension.CAPABILITY_NAME):
            capability_extension_host_for_the_app_process(
                "a-distribution-that-is-not-installed"
            ).register_capability(extension.CAPABILITY_NAME, "0.0.0")
    finally:
        runtime.shutdown()


def test_bringing_the_transport_stack_up_twice_is_not_an_error():
    """The hook runs once per process, but a runtime built after another must
    not be able to turn a second call into a failure."""
    _native.bring_up_the_transport_stack()
    _native.bring_up_the_transport_stack()
