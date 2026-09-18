# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""How a runtime is told where it sits on the runtime mesh.

`Runtime()` boots the engine without starting it, so every arm here runs on
every pull request rather than only on the rig. What each value *does* — where
the session opens, who it finds — is the engine's own to prove; what these prove
is that each door reaches the engine at all.

The door is easy to half-build: the constructor is a `#[pyclass]`'s `__new__`,
and `streamlib.Runtime` is a Python subclass whose `__init__` `type.__call__`
hands the same arguments. A keyword the engine takes and that subclass does not
is a `TypeError` at the call, whatever the engine's signature says.
"""

import pytest

import streamlib

MESH_ENVIRONMENT_VARIABLES = (
    "STREAMLIB_MESH_NAME",
    "STREAMLIB_MESH_PEER_ENDPOINTS",
    "STREAMLIB_MESH_LISTEN_ENDPOINTS",
    "STREAMLIB_MESH_MULTICAST_DISCOVERY",
)


@pytest.fixture(autouse=True)
def a_runtime_told_nothing_by_its_environment(monkeypatch):
    """No inherited mesh values, so an arm that sets none exercises a default.

    Discovery stays off for every arm here: each one constructs a real runtime,
    and none of them may go looking for the machine's actual mesh.
    """
    for variable in MESH_ENVIRONMENT_VARIABLES:
        monkeypatch.delenv(variable, raising=False)
    monkeypatch.setenv("STREAMLIB_MESH_MULTICAST_DISCOVERY", "0")


def test_every_mesh_value_reaches_the_engine_by_keyword():
    runtime = streamlib.Runtime(
        runtime_name="desk rig",
        mesh_name="lab",
        mesh_peer_endpoints=[],
        mesh_listen_endpoints=["udp/127.0.0.1:0?rel=1"],
        mesh_multicast_discovery=False,
    )
    runtime.shutdown()


def test_a_mesh_name_outside_the_chunk_grammar_is_refused_naming_what_is_wrong():
    for outside_the_grammar in ["Lab", "9lab", "lab/two", "lab two", "@lab", ""]:
        with pytest.raises(RuntimeError) as refusal:
            streamlib.Runtime(mesh_name=outside_the_grammar)
        assert "mesh name" in str(refusal.value), (
            f"the refusal of {outside_the_grammar!r} must say what it was: {refusal.value}"
        )


def test_an_endpoint_this_build_cannot_open_is_refused_at_construction():
    for endpoint, what_the_refusal_names in [
        ("quic/127.0.0.1:7447", "quic"),
        ("udp/127.0.0.1:7447", "rel=1"),
        ("not an endpoint", "not an endpoint"),
    ]:
        with pytest.raises(RuntimeError) as dialled:
            streamlib.Runtime(mesh_peer_endpoints=[endpoint])
        assert what_the_refusal_names in str(dialled.value), (
            f"the refusal of the dialled {endpoint!r} must name "
            f"{what_the_refusal_names!r}: {dialled.value}"
        )

        with pytest.raises(RuntimeError) as listened_on:
            streamlib.Runtime(mesh_listen_endpoints=[endpoint])
        assert what_the_refusal_names in str(listened_on.value), (
            f"the refusal of the listened-on {endpoint!r} must name "
            f"{what_the_refusal_names!r}: {listened_on.value}"
        )


def test_an_unreachable_peer_never_fails_the_runtime():
    # Nothing is listening there, and nothing ever will be. A mesh that cannot
    # be reached is not a reason a runtime fails to start.
    runtime = streamlib.Runtime(mesh_peer_endpoints=["tcp/127.0.0.1:1"])
    runtime.shutdown()


def test_the_environment_carries_a_mesh_name_when_the_constructor_does_not(monkeypatch):
    monkeypatch.setenv("STREAMLIB_MESH_NAME", "lab/two")

    with pytest.raises(RuntimeError) as refusal:
        streamlib.Runtime()
    assert "STREAMLIB_MESH_NAME" in str(refusal.value)


def test_the_constructors_mesh_name_outranks_the_environments(monkeypatch):
    monkeypatch.setenv("STREAMLIB_MESH_NAME", "lab/two")

    runtime = streamlib.Runtime(mesh_name="lab")
    runtime.shutdown()


def test_the_environment_carries_the_endpoint_lists_comma_separated(monkeypatch):
    monkeypatch.setenv("STREAMLIB_MESH_PEER_ENDPOINTS", "tcp/127.0.0.1:1,quic/127.0.0.1:2")

    with pytest.raises(RuntimeError) as refusal:
        streamlib.Runtime()
    assert "quic" in str(refusal.value)


def test_a_discovery_value_that_is_neither_one_nor_zero_is_refused_by_name(monkeypatch):
    monkeypatch.setenv("STREAMLIB_MESH_MULTICAST_DISCOVERY", "yes")

    with pytest.raises(RuntimeError) as refusal:
        streamlib.Runtime()
    assert "STREAMLIB_MESH_MULTICAST_DISCOVERY" in str(refusal.value)


def test_the_mesh_values_are_keyword_only():
    with pytest.raises(TypeError):
        streamlib.Runtime(None, "lab")  # type: ignore[call-arg]
