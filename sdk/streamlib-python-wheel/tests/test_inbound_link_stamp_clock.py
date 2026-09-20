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

import pytest

from streamlib import RuntimeContextFullAccess
from streamlib._engine import ProcessorLinkDataAccess

pytestmark = pytest.mark.usefixtures("private_iceoryx2_domain_for_this_test_process")

INPUT_PORT = "tracks"

BOOT_SESSION_UUID = re.compile(
    r"\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\Z"
)


def _a_context_reading_one_link(
    request: pytest.FixtureRequest,
    channel_service_name: str,
    inbound_link_name: str,
) -> RuntimeContextFullAccess:
    """One input port wired with the two names the engine sends for a link."""
    unique = f"stampclock{os.getpid()}_{request.node.name}"
    destination = ProcessorLinkDataAccess()
    destination.wire_input_link(
        INPUT_PORT, channel_service_name, inbound_link_name,
        f"{unique}_dest/notify", "read_next_in_order", 8, 8, 2, 1, f"L-{unique}",
    )  # fmt: skip
    return RuntimeContextFullAccess.open_for_helper_process(
        {}, destination, "runtime-under-test", "processor-under-test"
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


def test_a_link_from_another_runtime_names_nothing_with_no_runtime_to_ask(
    request: pytest.FixtureRequest,
):
    """The two names differ for a link carrying from another runtime — it rides
    a channel hashed from the source port's mesh address — and a helper holds
    no mesh session to say which machine is behind it.

    With no parent to ask, the answer is `None`. Fail-without-fix: answer this
    machine for every link, and a processor comparing a local track's stamps
    against a remote one's is told the two are on one clock.
    """
    unique = f"stampclockremote{os.getpid()}"
    context = _a_context_reading_one_link(
        request,
        f"{unique}/meshlink-deadbeefdeadbeef",
        "bench-cam-a1b2/CameraSource/video",
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
