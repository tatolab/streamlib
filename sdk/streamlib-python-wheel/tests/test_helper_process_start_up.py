# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""How a Python processor's helper process starts: loading its class, the
channel and engine build it is handed, and, from a real helper process, the
records it sends its parent once running.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Callable

import pytest

from helper_process_stand_in_parent import StandInParent
from helper_process_test_support import (
    PROBE_MODULE,
    engine_shaped_link_wiring,
)
from tatolab.runtime import _engine
from tatolab.runtime import _processor_interpreter_bootstrap as processor_interpreter_bootstrap
from tatolab.runtime._engine import engine_build_id_compiled_into_this_extension
from tatolab.runtime._processor_interpreter_bootstrap import (
    HelperProcessProtocolError,
    ParentProcessBridge,
    load_processor_class,
)

pytestmark = pytest.mark.usefixtures(
    "private_iceoryx2_domain_for_this_test_process",
    "log_records_this_process_sends_its_stand_in_parent",
)


def test_a_module_scope_class_loads_from_its_import_path():
    loaded = load_processor_class(f"{PROBE_MODULE}:PassThroughProbe")
    assert loaded.__name__ == "PassThroughProbe"


def test_a_nested_class_resolves_through_the_whole_dotted_qualname():
    """`stream_builder.add` deliberately admits `Outer.Inner` because a fresh
    interpreter can reach it — which it only can if the loader walks every
    segment. A single `getattr` on the joined qualname raises instead."""
    loaded = load_processor_class(f"{PROBE_MODULE}:OuterProbe.InnerProbe")
    assert loaded.__qualname__ == "OuterProbe.InnerProbe"


def test_an_unresolvable_import_path_names_the_segment_that_failed():
    with pytest.raises(HelperProcessProtocolError) as refusal:
        load_processor_class(f"{PROBE_MODULE}:OuterProbe.NoSuchProbe")
    assert "NoSuchProbe" in str(refusal.value)


def test_an_import_path_without_a_qualname_is_refused():
    with pytest.raises(HelperProcessProtocolError) as refusal:
        load_processor_class(PROBE_MODULE)
    assert "module:qualname" in str(refusal.value)


def test_a_helper_handed_no_iceoryx2_domain_root_refuses_to_start_by_name(
    monkeypatch: pytest.MonkeyPatch,
):
    """A helper opens its node only in the domain its parent resolved. One that
    guessed a root instead — from its working directory, iceoryx2's defaults or
    a config file — would open in a domain the parent is not in, where no
    message ever arrives and nothing errors.
    """
    monkeypatch.delenv("STREAMLIB_ICEORYX2_DOMAIN_ROOT")

    with pytest.raises(RuntimeError, match="STREAMLIB_ICEORYX2_DOMAIN_ROOT is not set"):
        _engine.open_node_link_data_access_for_helper_process()


def test_a_helper_started_without_its_channel_says_so(monkeypatch):
    monkeypatch.delenv(processor_interpreter_bootstrap.ESCALATE_FD_ENV, raising=False)
    with pytest.raises(HelperProcessProtocolError) as refusal:
        ParentProcessBridge.open_from_inherited_fd()
    assert processor_interpreter_bootstrap.ESCALATE_FD_ENV in str(refusal.value)


def test_a_non_numeric_channel_fd_is_refused_by_value(monkeypatch):
    monkeypatch.setenv(processor_interpreter_bootstrap.ESCALATE_FD_ENV, "not-an-fd")
    with pytest.raises(HelperProcessProtocolError) as refusal:
        ParentProcessBridge.open_from_inherited_fd()
    assert "not-an-fd" in str(refusal.value)


#: A build id no build of this checkout mints: its nonce is all zeros.
ENGINE_BUILD_ID_OF_ANOTHER_BUILD = (
    "0.0.1+0123456789abcdef0123456789abcdef01234567.00000000000000000000000000000000"
)

#: Long enough for a cold interpreter to import the whole wheel.
SECONDS_A_REAL_HELPER_HAS_TO_START = 30.0

# How long a helper has to send nothing before a test reads it as having sent
# everything it is going to. Comfortably past the forwarder's own 0.25 s pass,
# so a record it is holding is never mistaken for a record it never made.
SECONDS_A_QUIET_HELPER_SENDS_NOTHING_IN = 2.0


@pytest.fixture
def empty_iceoryx2_domain_root():
    """A domain root of the helper's own, empty until an iceoryx2 node opens in
    it — which is how a test sees whether one did. Short, under `/tmp`, so a
    helper that did get as far as a node is not refused on the socket budget
    instead."""
    domain_root = Path(tempfile.mkdtemp(prefix="sl-build-id-", dir="/tmp"))
    try:
        yield domain_root
    finally:
        shutil.rmtree(domain_root, ignore_errors=True)


def start_a_real_helper_process(
    parent: StandInParent,
    domain_root: Path,
    parent_engine_build_id: "str | None",
    *,
    rust_log: "str | None" = None,
) -> subprocess.Popen:
    """`python <lend>/tatolab/runtime/_processor_interpreter_bootstrap.py` as the
    spawn host starts it — its channel, its class, its domain — handed
    `parent_engine_build_id`, or no id at all.

    Everything a helper needs to reach its own iceoryx2 node is supplied, so a
    helper that let its start through would open one and wait on `parent`.

    `rust_log` is the level the child runs at, and is the environment's own
    only when a test names it: a developer's `RUST_LOG` would otherwise decide
    what the helper says."""
    environment = {
        name: value
        for name, value in os.environ.items()
        if name not in (processor_interpreter_bootstrap.ENGINE_BUILD_ID_ENV, "RUST_LOG")
    }
    if rust_log is not None:
        environment["RUST_LOG"] = rust_log
    if parent_engine_build_id is not None:
        environment[processor_interpreter_bootstrap.ENGINE_BUILD_ID_ENV] = parent_engine_build_id
    environment.update(
        {
            processor_interpreter_bootstrap.ENTRYPOINT_ENV: f"{PROBE_MODULE}:PassThroughProbe",
            processor_interpreter_bootstrap.PROCESSOR_ID_ENV: "Pbuildid",
            processor_interpreter_bootstrap.ESCALATE_FD_ENV: str(parent.child_end.fileno()),
            "STREAMLIB_ICEORYX2_DOMAIN_ROOT": str(domain_root),
            "PYTHONPATH": os.pathsep.join(
                entry
                for entry in (str(Path(__file__).parent), os.environ.get("PYTHONPATH"))
                if entry
            ),
        }
    )
    return subprocess.Popen(
        [sys.executable, processor_interpreter_bootstrap.__file__],
        env=environment,
        pass_fds=[parent.child_end.fileno()],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def standard_error_of_a_helper_that_refused_its_start(helper: subprocess.Popen) -> str:
    try:
        _, standard_error = helper.communicate(timeout=SECONDS_A_REAL_HELPER_HAS_TO_START)
    except subprocess.TimeoutExpired:
        helper.kill()
        _, standard_error = helper.communicate()
        pytest.fail(f"the helper let its start through and ran on:\n{standard_error}")
    assert helper.returncode == 1, standard_error
    return standard_error


def test_a_helper_that_imported_another_engine_build_refuses_before_it_opens_anything(
    stand_in_parent, empty_iceoryx2_domain_root
):
    """A stale `tatolab.runtime` earlier on a helper's `sys.path`, or an engine built
    against another iceoryx2, otherwise fails every service open as a
    corrupted service. The refusal is on raw stderr, naming both builds,
    because the log channel does not exist yet — and no node has opened.

    Fail-without-fix: skip the comparison and this helper opens its node in
    the domain root and waits on its parent until the timeout.
    """
    helper = start_a_real_helper_process(
        stand_in_parent, empty_iceoryx2_domain_root, ENGINE_BUILD_ID_OF_ANOTHER_BUILD
    )

    standard_error = standard_error_of_a_helper_that_refused_its_start(helper)

    assert (
        f"this helper imported engine build {engine_build_id_compiled_into_this_extension()}"
        in standard_error
    ), standard_error
    assert (
        f"its parent is engine build {ENGINE_BUILD_ID_OF_ANOTHER_BUILD}" in standard_error
    ), standard_error
    assert list(empty_iceoryx2_domain_root.iterdir()) == []
    assert stand_in_parent.receive(timeout_seconds=0.1) is None


def test_a_helper_handed_no_engine_build_id_refuses_rather_than_passing(
    stand_in_parent, empty_iceoryx2_domain_root
):
    """An absent id is not a pass: a helper that cannot tell whether its engine
    is its parent's does not start.

    Fail-without-fix: treat an unset variable as agreement — the check this
    replaced did — and this helper opens its node and waits.
    """
    helper = start_a_real_helper_process(
        stand_in_parent, empty_iceoryx2_domain_root, parent_engine_build_id=None
    )

    standard_error = standard_error_of_a_helper_that_refused_its_start(helper)

    assert (
        f"{processor_interpreter_bootstrap.ENGINE_BUILD_ID_ENV} is not set" in standard_error
    ), standard_error
    assert engine_build_id_compiled_into_this_extension() in standard_error
    assert list(empty_iceoryx2_domain_root.iterdir()) == []


def test_a_helper_handed_its_own_engine_build_id_starts_and_opens_its_node(
    stand_in_parent, empty_iceoryx2_domain_root
):
    """The control arm: parent and helper importing one wheel start exactly as
    before — the helper reports itself started on its channel with its node
    open in the domain it was handed."""
    helper = start_a_real_helper_process(
        stand_in_parent,
        empty_iceoryx2_domain_root,
        engine_build_id_compiled_into_this_extension(),
    )
    try:
        deadline = time.monotonic() + SECONDS_A_REAL_HELPER_HAS_TO_START
        started = None
        while started is None and time.monotonic() < deadline:
            frame = stand_in_parent.receive(timeout_seconds=deadline - time.monotonic())
            if frame is None:
                break
            if frame.get("op") == "log" and frame.get("message") == "helper process started":
                started = frame
        assert started is not None, "the helper never reported itself started"
        assert (empty_iceoryx2_domain_root / "nodes").is_dir()
    finally:
        helper.kill()
        helper.communicate()


def test_the_bootstrap_is_runnable_by_path():
    """The parent execs `python <lend>/tatolab/runtime/_processor_interpreter_bootstrap.py`;
    a file without a `__main__` guard would exec cleanly and do nothing."""
    with open(processor_interpreter_bootstrap.__file__, encoding="utf-8") as source:
        assert '__name__ == "__main__"' in source.read()


def frames_a_real_helper_sends(
    parent: StandInParent,
    *,
    until_each_of: "list[Callable[[dict], bool]]",
    seconds: float,
) -> "list[dict]":
    """Every frame the helper sends until each matcher has seen one of its own,
    or the wait ends.

    Each of, never the first of: a helper's lifecycle answers come from its main
    thread and its forwarded engine records from another, so neither is a
    barrier for the other and a frame a test wants can arrive after the frame it
    would have stopped on. The whole run is returned either way, so a failure
    reads as what the helper did send rather than as a bare timeout.
    """
    frames: "list[dict]" = []
    still_waiting = list(until_each_of)
    deadline = time.monotonic() + seconds
    while still_waiting and time.monotonic() < deadline:
        frame = parent.receive(timeout_seconds=deadline - time.monotonic())
        if frame is None:
            break
        frames.append(frame)
        still_waiting = [matches for matches in still_waiting if not matches(frame)]
    return frames


def every_frame_a_real_helper_sends_before_it_falls_quiet(
    parent: StandInParent, *, quiet_seconds: float, seconds: float
) -> "list[dict]":
    """Every frame the helper sends until it has sent none for `quiet_seconds`.

    What a test asserts is absent needs this rather than a frame to stop on: a
    record the forwarder sends after the answer a test would have stopped on is
    a record that test never saw.
    """
    frames: "list[dict]" = []
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        frame = parent.receive(
            timeout_seconds=min(quiet_seconds, deadline - time.monotonic())
        )
        if frame is None:
            break
        frames.append(frame)
    return frames


def a_captured_engine_record_whose_target_holds(text: str) -> "Callable[[dict], bool]":
    def matches(frame: dict) -> bool:
        return (
            frame.get("op") == "log"
            and frame.get("source") == "rust"
            and text in frame.get("target", "")
        )

    return matches


def test_a_helpers_own_engine_records_reach_its_parent_as_rust_records(
    stand_in_parent, empty_iceoryx2_domain_root
):
    """A helper hosts no engine and writes no log, so an engine record made in
    one reaches a reader only by riding the channel home. It travels as the
    Rust record it is — its own target, its own level, its processor — so the
    same call site reads in the runtime's log wherever it ran.

    Fail-without-fix: the child has no tracing subscriber, the record reaches
    nobody, and the run that a helper's input wiring explains is silent.
    """
    helper = start_a_real_helper_process(
        stand_in_parent,
        empty_iceoryx2_domain_root,
        engine_build_id_compiled_into_this_extension(),
        rust_log="debug",
    )
    try:
        stand_in_parent.send(
            {"cmd": "setup", "capability": "full", "config": {}, "ports": {}}
        )
        stand_in_parent.send(
            {
                "cmd": "wire_link",
                "direction": "input",
                "link": engine_shaped_link_wiring("input", "L-engine-records"),
            }
        )

        frames = frames_a_real_helper_sends(
            stand_in_parent,
            until_each_of=[
                a_captured_engine_record_whose_target_holds("iceoryx2::input")
            ],
            seconds=SECONDS_A_REAL_HELPER_HAS_TO_START,
        )

        captured = [
            frame
            for frame in frames
            if a_captured_engine_record_whose_target_holds("iceoryx2::input")(frame)
        ]
        assert captured, (
            "the engine's own record of the port it opened never reached the parent; "
            f"the helper sent {frames}"
        )
        record = captured[0]
        assert record["level"] == "debug", (
            "a captured record keeps the level its call site gave it, so a debug "
            f"record never arrives as a warning; got {record}"
        )
        assert record["processor_id"] == "Pbuildid", (
            "one helper hosts one processor, so its engine records name it"
        )
        assert int(record["source_seq"]) >= 1, (
            "a captured record shares the helper's sequence, so a gap in it still "
            "reads as records lost on the way home"
        )
        assert record["intercepted"] is False, (
            "a record handed over on the channel was never captured off a descriptor"
        )
    finally:
        helper.kill()
        helper.communicate()


def test_iceoryx2s_own_records_inside_a_helper_reach_the_parent_rather_than_stderr(
    stand_in_parent, empty_iceoryx2_domain_root
):
    """iceoryx2 refuses a port whose ring is deeper than the channel, and says
    why through its own logger. In a helper that logger is the engine's bridge,
    at the level the engine is configured to, so the reason reaches the log
    instead of a stderr line the parent re-emits as somebody else's warning.

    Fail-without-fix: with no bridge and no level set, iceoryx2 stays at its
    default and writes to stderr, so the refusal is either never made or
    arrives as an intercepted `fd2` warning attributed to nothing.
    """
    helper = start_a_real_helper_process(
        stand_in_parent,
        empty_iceoryx2_domain_root,
        engine_build_id_compiled_into_this_extension(),
        rust_log="debug",
    )
    try:
        stand_in_parent.send(
            {"cmd": "setup", "capability": "full", "config": {}, "ports": {}}
        )
        unopenable = engine_shaped_link_wiring("input", "L-unopenable")
        unopenable["channel_service_creation_depth"] = 4
        unopenable["input_port_ring_depth"] = 64
        stand_in_parent.send(
            {"cmd": "wire_link", "direction": "input", "link": unopenable}
        )

        frames = frames_a_real_helper_sends(
            stand_in_parent,
            until_each_of=[
                lambda frame: frame.get("rpc") == "link_wire_failed",
                lambda frame: frame.get("target") == "iceoryx2",
            ],
            seconds=SECONDS_A_REAL_HELPER_HAS_TO_START,
        )

        assert any(frame.get("rpc") == "link_wire_failed" for frame in frames), (
            f"the port was expected to refuse to open; the helper sent {frames}"
        )
        from_iceoryx2 = [
            frame
            for frame in frames
            if a_captured_engine_record_whose_target_holds("iceoryx2")(frame)
            and frame.get("target") == "iceoryx2"
        ]
        assert from_iceoryx2, (
            "iceoryx2's own account of the refusal never reached the parent; the "
            f"helper sent {frames}"
        )
        assert all("origin" in frame["attrs"] for frame in from_iceoryx2), (
            "the bridge carries iceoryx2's origin field, which is what says which "
            f"port object refused; got {from_iceoryx2}"
        )
    finally:
        helper.kill()
        standard_output, standard_error = helper.communicate()
        assert "iceoryx2" not in standard_error.lower(), (
            "iceoryx2's records go through the bridge, so none are written to the "
            f"helper's stderr for the parent to re-emit as a warning: {standard_error}"
        )


def test_a_helper_at_the_engines_default_level_sends_no_debug_records(
    stand_in_parent, empty_iceoryx2_domain_root
):
    """The level a helper captures at is the engine's own, not a level of its
    own choosing: with nothing configured that is `info`, and the debug records
    a wiring makes stay unsent.

    It says nothing on its own — a helper that captures nothing at all passes
    it too — and holds the other half of the pair: the test above has the same
    wiring send those records at `RUST_LOG=debug`, so together they say the
    level is read rather than fixed. Fail-without-fix for this one is capture
    pinned to trace, where every helper pays for every debug record the engine
    and iceoryx2 make, on the channel its data plane shares.
    """
    helper = start_a_real_helper_process(
        stand_in_parent,
        empty_iceoryx2_domain_root,
        engine_build_id_compiled_into_this_extension(),
    )
    try:
        stand_in_parent.send(
            {"cmd": "setup", "capability": "full", "config": {}, "ports": {}}
        )
        stand_in_parent.send(
            {
                "cmd": "wire_link",
                "direction": "input",
                "link": engine_shaped_link_wiring("input", "L-default-level"),
            }
        )

        frames = every_frame_a_real_helper_sends_before_it_falls_quiet(
            stand_in_parent,
            quiet_seconds=SECONDS_A_QUIET_HELPER_SENDS_NOTHING_IN,
            seconds=SECONDS_A_REAL_HELPER_HAS_TO_START,
        )

        assert any(frame.get("rpc") == "link_wired" for frame in frames), (
            f"the port was expected to open; the helper sent {frames}"
        )
        assert not [
            frame
            for frame in frames
            if frame.get("op") == "log" and frame.get("level") in ("debug", "trace")
        ], f"a helper at the default level sends nothing below info; got {frames}"
    finally:
        helper.kill()
        helper.communicate()
