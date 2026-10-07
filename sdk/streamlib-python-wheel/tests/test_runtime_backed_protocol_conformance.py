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
    contract_members_of_protocol,
    holding_findings,
    runtime_backed_protocols_tatolab_stream_declares,
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
        for member_name, member in contract_members_of_protocol(protocol).items()
        if member_name not in members_dropped
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
    return list(runtime_backed_function_registry.values())


def _native_class_members_a_copy_can_carry(native_class: type) -> "dict[str, Any]":
    return {
        member_name: member
        for member_name, member in vars(native_class).items()
        if member_name not in ("__dict__", "__weakref__")
    }


def _monotonic_timer_findings(
    protocol: type, native_class: "type | None" = None
) -> "dict[str, str]":
    """A Protocol's findings against the native timer, whose constructor
    `start_monotonic_timer` holds."""
    return _finding_texts_by_held_name(
        conformance_findings_for_protocol(
            protocol,
            native_class or getattr(_engine, "MonotonicTimer"),
            native_constructor_is_held_by_a_runtime_backed_function=True,
        )
    )


def test_the_engine_conforms_to_tatolab_stream_and_its_stub():
    findings = all_conformance_findings(
        _engine,
        tatolab.stream,
        runtime_backed_function_registry,
        ENGINE_STUB_PATH.read_text(),
    )
    assert [str(finding) for finding in findings] == []


def test_every_runtime_backed_name_the_ticket_lists_is_held_by_tatolab_stream():
    held_by_tatolab_stream = set(runtime_backed_protocols_tatolab_stream_declares(tatolab.stream)) | set(
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
    assert _monotonic_timer_findings(copy_of_monotonic_timer) == {}


def test_a_broken_protocol_reports_a_member_gained_one_lost_and_one_changed_by_name():
    def wait(self, timeout_in_milliseconds: int = 100) -> int: ...

    def restart(self) -> None: ...

    broken_monotonic_timer = _copy_of_protocol(
        tatolab.stream.MonotonicTimer,
        members_dropped=("close",),
        members_added={"wait": wait, "restart": restart},
    )

    findings = _monotonic_timer_findings(broken_monotonic_timer)

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
        "signature differs: native (timeout_ms=100), declared (timeout_in_milliseconds=100)"
    )


@pytest.mark.parametrize(
    ("replacement_wait", "expected_disagreement"),
    [
        (
            lambda self, timeout_ms: 0,
            "signature differs: native (timeout_ms=100), declared (timeout_ms)",
        ),
        (
            lambda self, timeout_ms=250: 0,
            "signature differs: native (timeout_ms=100), declared (timeout_ms=250)",
        ),
        (
            lambda self, *, timeout_ms=100: 0,
            "signature differs: native (timeout_ms=100), declared (*, timeout_ms=100)",
        ),
        (
            lambda self, timeout_ms=100, deadline_ns=None: 0,
            "signature differs: native (timeout_ms=100), "
            "declared (timeout_ms=100, deadline_ns=None)",
        ),
    ],
    ids=["default-dropped", "default-changed", "kind-changed", "parameter-added"],
)
def test_a_signature_change_is_reported_for_a_default_its_value_kind_and_order(
    replacement_wait, expected_disagreement
):
    broken_monotonic_timer = _copy_of_protocol(
        tatolab.stream.MonotonicTimer, members_added={"wait": replacement_wait}
    )
    assert _monotonic_timer_findings(broken_monotonic_timer) == {
        "MonotonicTimer.wait": expected_disagreement
    }


def test_a_default_pyo3_cannot_spell_is_held_to_presence_only():
    def native_wait(self, timeout_ms=...): ...

    def declared_wait_with_a_default(self, timeout_ms=250): ...

    def declared_wait_without_a_default(self, timeout_ms): ...

    native_timer_with_an_unspellable_default = type(
        "MonotonicTimer",
        (),
        {
            **_native_class_members_a_copy_can_carry(getattr(_engine, "MonotonicTimer")),
            "wait": native_wait,
        },
    )
    assert _monotonic_timer_findings(
        _copy_of_protocol(
            tatolab.stream.MonotonicTimer, members_added={"wait": declared_wait_with_a_default}
        ),
        native_timer_with_an_unspellable_default,
    ) == {}
    assert _monotonic_timer_findings(
        _copy_of_protocol(
            tatolab.stream.MonotonicTimer,
            members_added={"wait": declared_wait_without_a_default},
        ),
        native_timer_with_an_unspellable_default,
    ) == {
        "MonotonicTimer.wait": "signature differs: native (timeout_ms=…), declared (timeout_ms)"
    }


def test_a_dunder_the_native_class_gains_is_reported_whatever_its_name():
    native_class_with_gained_dunders = type(
        "MonotonicTimer",
        (),
        {
            **_native_class_members_a_copy_can_carry(getattr(_engine, "MonotonicTimer")),
            "__eq__": lambda self, other: self is other,
            "__index__": lambda self: 0,
        },
    )
    findings = _monotonic_timer_findings(
        tatolab.stream.MonotonicTimer, native_class_with_gained_dunders
    )
    assert findings == {
        "MonotonicTimer.__eq__": "on the native class and missing from the Protocol",
        "MonotonicTimer.__hash__": "on the native class and missing from the Protocol",
        "MonotonicTimer.__index__": "on the native class and missing from the Protocol",
    }


def test_a_native_constructor_is_reported_unless_a_runtime_backed_function_holds_it():
    native_surface_handle = getattr(_engine, "GpuSurfaceHandle")
    assert "__new__" not in vars(native_surface_handle)
    native_surface_handle_with_a_constructor = type(
        "GpuSurfaceHandle",
        (),
        {
            **_native_class_members_a_copy_can_carry(native_surface_handle),
            "__new__": lambda cls, surface_id: object.__new__(cls),
        },
    )
    assert _finding_texts_by_held_name(
        conformance_findings_for_protocol(
            tatolab.stream.GpuSurfaceHandle,
            native_surface_handle_with_a_constructor,
            native_constructor_is_held_by_a_runtime_backed_function=False,
        )
    ) == {
        "GpuSurfaceHandle.__new__": "a native constructor no runtime-backed function forwards to"
    }
    assert "__new__" in vars(getattr(_engine, "MonotonicTimer"))
    assert _finding_texts_by_held_name(
        conformance_findings_for_protocol(
            tatolab.stream.MonotonicTimer,
            getattr(_engine, "MonotonicTimer"),
            native_constructor_is_held_by_a_runtime_backed_function=False,
        )
    ) == {"MonotonicTimer.__new__": "a native constructor no runtime-backed function forwards to"}


def test_a_member_a_protocol_inherits_from_a_protocol_base_is_part_of_its_contract():
    class _MembersTheTimerShares(Protocol):
        def wait(self, timeout_ms: int = 100) -> int: ...

    timer_declaring_wait_on_a_base = types.new_class(
        "MonotonicTimer",
        (_MembersTheTimerShares, Protocol),
        exec_body=lambda namespace: namespace.update(
            {
                member_name: member
                for member_name, member in contract_members_of_protocol(
                    tatolab.stream.MonotonicTimer
                ).items()
                if member_name != "wait"
            }
        ),
    )
    assert "wait" not in vars(timer_declaring_wait_on_a_base)
    assert _monotonic_timer_findings(timer_declaring_wait_on_a_base) == {}


def test_a_property_declared_as_a_method_is_reported_as_a_kind_mismatch():
    def interval_ns(self) -> int: ...

    broken_monotonic_timer = _copy_of_protocol(
        tatolab.stream.MonotonicTimer, members_added={"interval_ns": interval_ns}
    )
    assert _monotonic_timer_findings(broken_monotonic_timer) == {
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
            runtime_backed_protocols_tatolab_stream_declares(tatolab.stream),
            _real_registry_native_names(),
            stub_source,
        )
    )
    assert set(findings) == {"GpuSurfaceHandle"}
    assert "declared in tatolab.stream and in _engine.pyi" in findings["GpuSurfaceHandle"]


def test_a_name_the_engine_exports_and_nothing_declares_is_reported():
    stub_source = ENGINE_STUB_PATH.read_text().replace(
        "def runtime_log_directory() -> Path:", "def renamed_runtime_log_directory() -> Path:"
    )
    findings = _finding_texts_by_held_name(
        holding_findings(
            _engine,
            runtime_backed_protocols_tatolab_stream_declares(tatolab.stream),
            _real_registry_native_names(),
            stub_source,
        )
    )
    assert set(findings) == {"runtime_log_directory", "renamed_runtime_log_directory"}
    assert "declared nowhere" in findings["runtime_log_directory"]
    assert findings["renamed_runtime_log_directory"] == "declared but not exported by the engine"


def test_the_stubtest_allowlist_names_exactly_what_only_tatolab_stream_declares():
    allowlist = stubtest_allowlist_of_names_held_by_tatolab_stream(
        runtime_backed_protocols_tatolab_stream_declares(tatolab.stream),
        _real_registry_native_names(),
        ENGINE_STUB_PATH.read_text(),
    )
    assert r"tatolab\.runtime\._engine\.__all__" in allowlist
    assert r"tatolab\.runtime\._engine\.GpuSurfaceHandle(\..*)?" in allowlist
    assert r"tatolab\.runtime\._engine\.monotonic_now_ns(\..*)?" in allowlist
    assert r"tatolab\.runtime\._engine\.RuntimeContextFullAccess(\..*)?" in allowlist
    assert r"tatolab\.runtime\._engine\.NodeLinkDataAccess(\..*)?" in allowlist
    assert r"tatolab\.runtime\._engine\.log_event(\..*)?" in allowlist
