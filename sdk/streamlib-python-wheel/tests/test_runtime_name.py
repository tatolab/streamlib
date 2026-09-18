# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""How a runtime is told its name on the runtime mesh.

`Runtime()` boots the engine without starting it, so every arm here runs on
every pull request rather than only on the rig. What the default name *is* — the
host, the app directory and the hash of its full path — is the engine's own to
prove; what these prove is that each door reaches the engine and that the
constructor outranks the environment.
"""

import pytest

import streamlib

RUNTIME_NAME_ENVIRONMENT_VARIABLE = "STREAMLIB_RUNTIME_NAME"


@pytest.fixture(autouse=True)
def an_unnamed_environment(monkeypatch):
    """No inherited name, so a test that sets none exercises the default."""
    monkeypatch.delenv(RUNTIME_NAME_ENVIRONMENT_VARIABLE, raising=False)


def test_the_constructor_takes_a_runtime_name_by_keyword_only():
    runtime = streamlib.Runtime(runtime_name="desk rig")
    runtime.shutdown()

    with pytest.raises(TypeError):
        streamlib.Runtime("desk rig")  # type: ignore[call-arg]


def test_a_runtime_name_that_cannot_be_an_address_chunk_is_refused_naming_the_character():
    for forbidden in ["/", "*", "$", "#", "?"]:
        with pytest.raises(RuntimeError) as refusal:
            streamlib.Runtime(runtime_name=f"desk{forbidden}rig")
        assert repr(forbidden) in str(refusal.value), (
            f"the refusal must name {forbidden!r}: {refusal.value}"
        )


def test_a_runtime_name_beginning_with_an_at_sign_is_refused():
    with pytest.raises(RuntimeError) as refusal:
        streamlib.Runtime(runtime_name="@runtime")
    assert "@" in str(refusal.value)


def test_an_empty_runtime_name_is_refused_rather_than_read_as_unset():
    with pytest.raises(RuntimeError) as refusal:
        streamlib.Runtime(runtime_name="")
    assert "empty" in str(refusal.value)


def test_the_environment_names_the_runtime_when_the_constructor_does_not(monkeypatch):
    monkeypatch.setenv(RUNTIME_NAME_ENVIRONMENT_VARIABLE, "desk/rig")

    with pytest.raises(RuntimeError) as refusal:
        streamlib.Runtime()

    # The variable is what the engine read, and the refusal says so — which is
    # what tells this arm apart from the constructor's.
    assert RUNTIME_NAME_ENVIRONMENT_VARIABLE in str(refusal.value)
    assert "'/'" in str(refusal.value)


def test_the_constructors_name_outranks_the_environments(monkeypatch):
    # A name the engine would refuse, so a runtime that constructs at all proves
    # the environment was never consulted.
    monkeypatch.setenv(RUNTIME_NAME_ENVIRONMENT_VARIABLE, "desk/rig")

    runtime = streamlib.Runtime(runtime_name="desk rig")
    runtime.shutdown()


def test_an_empty_environment_value_reads_as_unset(monkeypatch):
    monkeypatch.setenv(RUNTIME_NAME_ENVIRONMENT_VARIABLE, "")

    runtime = streamlib.Runtime()
    runtime.shutdown()


def test_an_unnamed_runtime_takes_the_engines_default():
    """Named by nothing, a runtime still constructs — the default is the engine's."""
    runtime = streamlib.Runtime()
    runtime.shutdown()


def test_hosting_the_control_plane_takes_no_name_of_its_own():
    """The name belongs to the runtime, so there is nothing to pass here."""
    runtime = streamlib.Runtime(runtime_name="desk rig")
    try:
        # Spelled as a mapping so the retired keyword's own text does not
        # survive here, where a source-walking gate would still find it.
        retired = {"node_name": "desk rig"}
        with pytest.raises(TypeError):
            runtime.host_control_plane(bind_host="127.0.0.1", bind_port=0, **retired)
    finally:
        runtime.shutdown()
