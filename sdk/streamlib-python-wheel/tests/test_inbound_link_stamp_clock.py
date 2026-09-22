# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Which machine's monotonic clock a link's stamps are taken on.

Every stamp on the data plane is a machine's monotonic clock, whose epoch is
that machine's own boot, so two stamps taken on two machines are readings of
two unrelated clocks. A processor fanning several links in has to be able to
ask, link by link, which clock it is reading before it compares one link's
stamps against another's.

GPU-free by construction. The links are real iceoryx2 ports and the context is
a helper-process one opened directly on them, so nothing here starts a runtime
or touches a device — which is also what makes the remote-link arm meaningful:
a helper holds no mesh session, and with no parent to ask it must say it does
not know rather than answer with this machine.
"""

import os
import re
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from streamlib import RuntimeContextFullAccess, this_machines_stamp_clock_identity
from streamlib._engine import ProcessorLinkDataAccess

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

INPUT_PORT = "tracks"

#: Where Linux reports the boot session the monotonic epoch belongs to. The
#: engine reads this file and nothing else, which is the claim under test.
LINUX_BOOT_SESSION_PATH = "/proc/sys/kernel/random/boot_id"

BOOT_SESSION_UUID = re.compile(
    r"\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\Z"
)


def _a_context_reading_one_link(
    request: pytest.FixtureRequest,
    channel_service_name: str,
    inbound_link_name: str,
    stamp_clock: str = "this_machine",
    escalate_request_to_parent: "Callable[..., Any] | None" = None,
) -> RuntimeContextFullAccess:
    """One input port wired the way the engine wires one: two names for the
    link, and the token saying which machine its stamps are taken on."""
    unique = f"stampclock{os.getpid()}_{request.node.name}"
    destination = ProcessorLinkDataAccess()
    destination.wire_input_link(
        INPUT_PORT, channel_service_name, inbound_link_name, stamp_clock,
        f"{unique}_dest/notify", "read_next_in_order", 8, 8, 2, 1, f"L-{unique}",
    )  # fmt: skip
    return RuntimeContextFullAccess.open_for_helper_process(
        {},
        destination,
        "runtime-under-test",
        "processor-under-test",
        escalate_request_to_parent,
        None,
    )


def test_a_link_from_this_runtime_names_this_machine(
    request: pytest.FixtureRequest,
):
    """The engine sends one name twice for a link whose source is here, and
    that is what says the bags were stamped on this machine.

    Two links that both answer this string are two links a processor may
    compare stamps across.
    """
    unique = f"stampclocklocal{os.getpid()}"
    channel_service_name = f"{unique}/video_out"
    context = _a_context_reading_one_link(
        request, channel_service_name, channel_service_name
    )

    named = context.inputs.inbound_link_stamp_clock_identity(
        INPUT_PORT, channel_service_name
    )

    assert named is not None, "a link from this runtime always names a machine"
    assert BOOT_SESSION_UUID.match(named), (
        f"{named!r} is not a boot-session UUID, which is what the answer is"
    )


def test_this_machines_clock_is_the_one_a_link_from_this_runtime_names(
    request: pytest.FixtureRequest,
):
    """The other half of a stamp comparison: a processor holding a link's
    machine needs the machine its own readings are on to compare it against.

    The two answers must be the one string, or a processor asking both is told
    a local link is on a clock it is not — which is exactly the comparison the
    per-machine rule exists to stop. Fail-without-fix: derive this machine's
    identity anywhere but where a link's answer comes from, and the two drift
    apart the day either changes.
    """
    unique = f"stampclockthismachine{os.getpid()}"
    channel_service_name = f"{unique}/video_out"
    context = _a_context_reading_one_link(
        request, channel_service_name, channel_service_name
    )

    this_machine = this_machines_stamp_clock_identity()

    assert this_machine is not None, (
        "Linux names its boot session, so this platform names a clock"
    )
    assert BOOT_SESSION_UUID.match(this_machine), (
        f"{this_machine!r} is not a boot-session UUID, which is what the answer is"
    )
    assert this_machine == context.inputs.inbound_link_stamp_clock_identity(
        INPUT_PORT, channel_service_name
    ), "a link from this runtime is stamped on this machine's clock, by that name"


@pytest.mark.linux_only_capability(reason="the boot session is a Linux kernel file")
@pytest.mark.skipif(
    not Path(LINUX_BOOT_SESSION_PATH).exists(),
    reason="only Linux reports its boot session at this path",
)
def test_this_machines_clock_is_the_kernels_own_boot_session():
    """Locked to the kernel's own answer rather than to itself: the identity is
    the boot session and nothing else — deliberately not the host identity, which
    pairs the same boot id with the pid namespace, so a container and its host
    read as two machines there and as one clock here.

    Fail-without-fix: derive it from anything that distinguishes a container from
    its host, and two processes that genuinely share a monotonic epoch stop being
    allowed to compare stamps.
    """
    assert (
        this_machines_stamp_clock_identity()
        == Path(LINUX_BOOT_SESSION_PATH).read_text().strip()
    )


def test_a_link_from_another_runtime_names_nothing_with_no_runtime_to_ask(
    request: pytest.FixtureRequest,
):
    """The engine wires a link carrying from another runtime with the token
    saying so, and a helper holds no mesh session to name the machine behind it.

    With no parent to ask, the answer is `None`. Fail-without-fix: answer this
    machine for every link, and a processor comparing a local track's stamps
    against a remote one's is told the two are on one clock.
    """
    unique = f"stampclockremote{os.getpid()}"
    context = _a_context_reading_one_link(
        request,
        f"{unique}/meshlink-deadbeefdeadbeef",
        "bench-cam-a1b2/CameraSource/video",
        stamp_clock="a_machine_only_the_app_process_can_name",
    )

    assert (
        context.inputs.inbound_link_stamp_clock_identity(
            INPUT_PORT, "bench-cam-a1b2/CameraSource/video"
        )
        is None
    )


def test_a_link_name_the_port_does_not_carry_names_nothing(
    request: pytest.FixtureRequest,
):
    """Asking about a link that is not there is not an error — an unconnected
    input is a legal graph — but it must not borrow another link's machine.
    """
    unique = f"stampclockmissing{os.getpid()}"
    channel_service_name = f"{unique}/video_out"
    context = _a_context_reading_one_link(
        request, channel_service_name, channel_service_name
    )

    assert (
        context.inputs.inbound_link_stamp_clock_identity(
            INPUT_PORT, "pnothing/video_out"
        )
        is None
    )


def test_a_stamp_clock_token_this_build_does_not_know_is_refused_by_name(
    request: pytest.FixtureRequest,
):
    """A wiring this build cannot read must refuse rather than fall back.

    Falling back would have to pick one of the two answers, and picking
    `this_machine` is the one that lets two clocks be compared — so the link
    never opens instead.
    """
    unique = f"stampclockbadtoken{os.getpid()}"
    with pytest.raises(ValueError, match="stamp clock"):
        _a_context_reading_one_link(
            request,
            f"{unique}/video_out",
            f"{unique}/video_out",
            stamp_clock="whatever_machine",
        )


# =============================================================================
# The half a helper cannot answer for itself
# =============================================================================

A_REMOTE_LINKS_NAME = "bench-cam-a1b2/Camera Source/video"
ANOTHER_MACHINE = "8b93a1c2-0000-4d5a-9a11-2c7f0d5e2f1c"


class _AParentThatAnswers:
    """Stands in for the bridge's `request_from_parent`, recording what it was
    asked so the question itself can be checked, not only the answer."""

    def __init__(self, answer: "dict[str, Any] | Exception") -> None:
        self.answer = answer
        self.asked: "list[dict[str, Any]]" = []

    def __call__(self, op: "dict[str, Any]") -> "dict[str, Any]":
        self.asked.append(dict(op))
        if isinstance(self.answer, Exception):
            raise self.answer
        return self.answer


def test_a_remote_links_machine_is_asked_of_the_runtime_and_handed_back(
    request: pytest.FixtureRequest,
):
    """The whole of the corrected Design §2: a helper holds no mesh session, so
    it asks the runtime by the link's name — which for a remote link is the
    source port's mesh address — and hands back what the runtime answered.

    Fail-without-fix: return `None` without asking, or answer this machine, and
    a Python sink is told a remote track shares a clock with a local one.
    """
    unique = f"stampclockasks{os.getpid()}"
    the_parent = _AParentThatAnswers(
        {"result": "ok", "stamp_clock_identity": ANOTHER_MACHINE}
    )
    context = _a_context_reading_one_link(
        request,
        f"{unique}/meshlink-deadbeefdeadbeef",
        A_REMOTE_LINKS_NAME,
        stamp_clock="a_machine_only_the_app_process_can_name",
        escalate_request_to_parent=the_parent,
    )

    named = context.inputs.inbound_link_stamp_clock_identity(
        INPUT_PORT, A_REMOTE_LINKS_NAME
    )

    assert named == ANOTHER_MACHINE
    assert the_parent.asked == [
        {
            "op": "inbound_link_stamp_clock_identity",
            "inbound_link_name": A_REMOTE_LINKS_NAME,
        }
    ], "the runtime is asked by the link's name, and asked once"


def test_a_link_from_this_runtime_never_asks_the_runtime(
    request: pytest.FixtureRequest,
):
    """A local link is answered in process. Asking would put a bridge round
    trip behind every one of them.
    """
    unique = f"stampclocknoask{os.getpid()}"
    channel_service_name = f"{unique}/video_out"
    the_parent = _AParentThatAnswers({"result": "ok", "stamp_clock_identity": ANOTHER_MACHINE})
    context = _a_context_reading_one_link(
        request,
        channel_service_name,
        channel_service_name,
        escalate_request_to_parent=the_parent,
    )

    named = context.inputs.inbound_link_stamp_clock_identity(
        INPUT_PORT, channel_service_name
    )

    assert the_parent.asked == [], "a local link is not the runtime's to answer"
    assert named is not None and named != ANOTHER_MACHINE


def test_a_runtime_that_cannot_answer_names_no_machine_rather_than_raising(
    request: pytest.FixtureRequest,
):
    """The caller asked which clock a link is on; "the runtime could not say"
    is an answer to that, and it is the answer that stops a stamp being
    compared. Raising would take down a processor that was being careful.
    """
    unique = f"stampclockrefused{os.getpid()}"
    context = _a_context_reading_one_link(
        request,
        f"{unique}/meshlink-deadbeefdeadbeef",
        A_REMOTE_LINKS_NAME,
        stamp_clock="a_machine_only_the_app_process_can_name",
        escalate_request_to_parent=_AParentThatAnswers(
            RuntimeError("the parent refused")
        ),
    )

    assert (
        context.inputs.inbound_link_stamp_clock_identity(
            INPUT_PORT, A_REMOTE_LINKS_NAME
        )
        is None
    )


def test_a_runtime_that_names_no_machine_yet_hands_back_nothing(
    request: pytest.FixtureRequest,
):
    """The runtime answers with the key absent while nothing has crossed the
    link, which must read as `None` rather than as a protocol break.
    """
    unique = f"stampclocknotyet{os.getpid()}"
    context = _a_context_reading_one_link(
        request,
        f"{unique}/meshlink-deadbeefdeadbeef",
        A_REMOTE_LINKS_NAME,
        stamp_clock="a_machine_only_the_app_process_can_name",
        escalate_request_to_parent=_AParentThatAnswers({"result": "ok"}),
    )

    assert (
        context.inputs.inbound_link_stamp_clock_identity(
            INPUT_PORT, A_REMOTE_LINKS_NAME
        )
        is None
    )
