# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The engine's native classes and functions conform to what `tatolab.stream` declares.

The real module is held whole, and a deliberately broken copy of a real
Protocol shows the gate catches each way a native class can drift from it.
"""

import types
from typing import Any, Protocol

import pytest

import tatolab.stream
from runtime_backed_protocol_conformance import (
    ENGINE_STUB_PATH,
    ConformanceFinding,
    all_conformance_findings,
    conformance_findings_for_protocol,
    conformance_findings_for_runtime_backed_function,
    holding_findings,
    protocols_tatolab_stream_declares,
    stubtest_allowlist_of_names_held_by_tatolab_stream,
)
from tatolab.runtime import _engine
from tatolab.stream._runtime_lend import runtime_backed_function_registry


def _copy_of_protocol(
    protocol: type,
    *,
    members_dropped: tuple[str, ...] = (),
    members_added: "dict[str, Any] | None" = None,
) -> type:
    """A new Protocol with `protocol`'s own members, less some and plus others."""
    copied_members = {
        member_name: member
        for member_name, member in vars(protocol).items()
        if member_name not in members_dropped
        and (not member_name.startswith("_") or member_name in ("__enter__", "__exit__"))
    }
    copied_members.update(members_added or {})
    return types.new_class(
        protocol.__name__,
        (Protocol,),
        exec_body=lambda namespace: namespace.update(copied_members),
    )


def _finding_texts_by_held_name(findings: "list[ConformanceFinding]") -> "dict[str, str]":
    return {finding.held_name: finding.disagreement for finding in findings}


def _real_registry_native_names() -> "list[str]":
    return [registration.native_callable_name for registration in runtime_backed_function_registry]


def test_the_engine_conforms_to_tatolab_stream_and_its_stub():
    findings = all_conformance_findings(
        _engine,
        tatolab.stream,
        runtime_backed_function_registry,
        ENGINE_STUB_PATH.read_text(),
    )
    assert [str(finding) for finding in findings] == []


def test_every_runtime_backed_name_the_ticket_lists_is_held_by_tatolab_stream():
    held_by_tatolab_stream = set(protocols_tatolab_stream_declares(tatolab.stream)) | set(
        _real_registry_native_names()
    )
    assert {
        "RuntimeContextFullAccess",
        "RuntimeContextLimitedAccess",
        "LinkInputDataReader",
        "LinkOutputDataWriter",
        "NodeLinkDataAccess",
        "GpuContextFullAccess",
        "GpuContextLimitedAccess",
        "GpuSurfaceHandle",
        "GpuSurfaceDeviceTensorScope",
        "GpuSurfaceCheckOutLease",
        "ComputeKernel",
        "GraphicsKernel",
        "RayTracingKernel",
        "KernelDispatchBatch",
        "AccelerationStructureHandle",
        "MonotonicTimer",
        "OpaqueFdTextureExport",
        "IOSurfaceMachPortExport",
        "NodeOwnedWindow",
        "NodeOwnedWindowEvents",
        "encode_bag_to_msgpack_bytes",
        "decode_msgpack_bytes_to_python_object",
        "monotonic_now_ns",
        "gpu_limited_access_of_the_typed_read_in_progress",
        "log_event",
    } <= held_by_tatolab_stream


def test_an_unbroken_copy_of_a_real_protocol_conforms():
    copy_of_monotonic_timer = _copy_of_protocol(tatolab.stream.MonotonicTimer)
    assert (
        conformance_findings_for_protocol(
            copy_of_monotonic_timer, getattr(_engine, "MonotonicTimer")
        )
        == []
    )


def test_a_broken_protocol_reports_a_member_gained_one_lost_and_one_changed_by_name():
    def wait(self, timeout_in_milliseconds: int = 100) -> int: ...

    def restart(self) -> None: ...

    broken_monotonic_timer = _copy_of_protocol(
        tatolab.stream.MonotonicTimer,
        members_dropped=("close",),
        members_added={"wait": wait, "restart": restart},
    )

    findings = _finding_texts_by_held_name(
        conformance_findings_for_protocol(
            broken_monotonic_timer, getattr(_engine, "MonotonicTimer")
        )
    )

    assert set(findings) == {
        "MonotonicTimer.close",
        "MonotonicTimer.restart",
        "MonotonicTimer.wait",
    }
    assert findings["MonotonicTimer.close"] == "on the native class and missing from the Protocol"
    assert findings["MonotonicTimer.restart"] == (
        "declared by the Protocol and missing from the native class"
    )
    assert findings["MonotonicTimer.wait"] == (
        "signature differs: native (timeout_ms=…), declared (timeout_in_milliseconds=…)"
    )


@pytest.mark.parametrize(
    ("replacement_wait", "expected_disagreement"),
    [
        (
            lambda self, timeout_ms: 0,
            "signature differs: native (timeout_ms=…), declared (timeout_ms)",
        ),
        (
            lambda self, *, timeout_ms=100: 0,
            "signature differs: native (timeout_ms=…), declared (*, timeout_ms=…)",
        ),
        (
            lambda self, timeout_ms=100, deadline_ns=None: 0,
            "signature differs: native (timeout_ms=…), declared (timeout_ms=…, deadline_ns=…)",
        ),
    ],
    ids=["default-dropped", "kind-changed", "parameter-added"],
)
def test_a_signature_change_is_reported_for_presence_of_a_default_kind_and_order(
    replacement_wait, expected_disagreement
):
    broken_monotonic_timer = _copy_of_protocol(
        tatolab.stream.MonotonicTimer, members_added={"wait": replacement_wait}
    )
    findings = _finding_texts_by_held_name(
        conformance_findings_for_protocol(
            broken_monotonic_timer, getattr(_engine, "MonotonicTimer")
        )
    )
    assert findings == {"MonotonicTimer.wait": expected_disagreement}


def test_a_property_declared_as_a_method_is_reported_as_a_kind_mismatch():
    def interval_ns(self) -> int: ...

    broken_monotonic_timer = _copy_of_protocol(
        tatolab.stream.MonotonicTimer, members_added={"interval_ns": interval_ns}
    )
    findings = _finding_texts_by_held_name(
        conformance_findings_for_protocol(
            broken_monotonic_timer, getattr(_engine, "MonotonicTimer")
        )
    )
    assert findings == {
        "MonotonicTimer.interval_ns": "a property on the native class, a method on the Protocol"
    }


def test_a_broken_runtime_backed_function_declaration_is_reported_by_name():
    def monotonic_now_ns(clock_identity: int) -> int: ...

    findings = conformance_findings_for_runtime_backed_function(
        monotonic_now_ns, getattr(_engine, "monotonic_now_ns"), "monotonic_now_ns"
    )
    assert [str(finding) for finding in findings] == [
        "monotonic_now_ns: signature differs: native (), declared (clock_identity)"
    ]


def test_the_timer_start_is_held_to_the_native_timer_constructor():
    def start_monotonic_timer(interval: int) -> object: ...

    findings = conformance_findings_for_runtime_backed_function(
        start_monotonic_timer, getattr(_engine, "MonotonicTimer"), "start_monotonic_timer"
    )
    assert [str(finding) for finding in findings] == [
        "start_monotonic_timer: signature differs: native (interval_ns), declared (interval)"
    ]


def test_a_name_declared_in_the_stub_and_as_a_protocol_is_reported():
    stub_source = ENGINE_STUB_PATH.read_text() + (
        "\nclass GpuSurfaceHandle:\n    def close(self) -> None: ...\n"
    )
    findings = _finding_texts_by_held_name(
        holding_findings(
            _engine,
            protocols_tatolab_stream_declares(tatolab.stream),
            _real_registry_native_names(),
            stub_source,
        )
    )
    assert set(findings) == {"GpuSurfaceHandle"}
    assert "declared in tatolab.stream and in _engine.pyi" in findings["GpuSurfaceHandle"]


def test_a_class_typed_as_its_protocol_that_constructs_with_arguments_is_reported():
    class NodeLinkDataAccess:
        def __init__(self, ring_capacity: int) -> None: ...

    engine_with_a_changed_constructor = types.ModuleType(_engine.__name__)
    vars(engine_with_a_changed_constructor).update(vars(_engine))
    setattr(engine_with_a_changed_constructor, "NodeLinkDataAccess", NodeLinkDataAccess)

    findings = _finding_texts_by_held_name(
        holding_findings(
            engine_with_a_changed_constructor,
            protocols_tatolab_stream_declares(tatolab.stream),
            _real_registry_native_names(),
            ENGINE_STUB_PATH.read_text(),
        )
    )
    assert findings == {
        "NodeLinkDataAccess": "the native class constructs with (ring_capacity); "
        "`type[NodeLinkDataAccess]` declares a constructor taking no arguments"
    }


def test_a_name_the_engine_exports_and_nothing_declares_is_reported():
    stub_source = ENGINE_STUB_PATH.read_text().replace(
        "def runtime_log_directory() -> Path:", "def renamed_runtime_log_directory() -> Path:"
    )
    findings = _finding_texts_by_held_name(
        holding_findings(
            _engine,
            protocols_tatolab_stream_declares(tatolab.stream),
            _real_registry_native_names(),
            stub_source,
        )
    )
    assert set(findings) == {"runtime_log_directory", "renamed_runtime_log_directory"}
    assert "declared nowhere" in findings["runtime_log_directory"]
    assert findings["renamed_runtime_log_directory"] == "declared but not exported by the engine"


def test_the_stubtest_allowlist_names_exactly_what_only_tatolab_stream_declares():
    allowlist = stubtest_allowlist_of_names_held_by_tatolab_stream(
        protocols_tatolab_stream_declares(tatolab.stream),
        _real_registry_native_names(),
        ENGINE_STUB_PATH.read_text(),
    )
    assert r"tatolab\.runtime\._engine\.__all__" in allowlist
    assert r"tatolab\.runtime\._engine\.GpuSurfaceHandle(\..*)?" in allowlist
    assert r"tatolab\.runtime\._engine\.monotonic_now_ns(\..*)?" in allowlist
    assert r"tatolab\.runtime\._engine\.RuntimeContextFullAccess(\..*)?" in allowlist
    assert not [entry for entry in allowlist if "NodeLinkDataAccess" in entry]
    assert r"tatolab\.runtime\._engine\.log_event(\..*)?" in allowlist
