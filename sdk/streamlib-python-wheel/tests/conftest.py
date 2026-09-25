# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Fixtures shared by the suites that drive an app out of process."""

import os
import shutil
import sys
import tempfile
from pathlib import Path
from typing import Callable, Iterator

import pytest

from app_under_test import AppUnderTest, start_app

ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE = "STREAMLIB_ICEORYX2_DOMAIN_ROOT"
MESH_MULTICAST_DISCOVERY_ENVIRONMENT_VARIABLE = "STREAMLIB_MESH_MULTICAST_DISCOVERY"
DOMAIN_ROOT_NAME_PREFIX = "sl-iox2-"
# Not `tempfile.gettempdir()`: on macOS that is a `/var/folders/...` path long
# enough to overrun the budget iceoryx2's socket paths leave a domain root.
SHARED_TEMPORARY_DIRECTORY = Path("/tmp")


# Every runtime this suite constructs — in this process and in every app it
# launches — stays off whatever runtime mesh the machine is actually on. Set at
# import rather than in a fixture because an app subprocess inherits the
# environment as it is spawned, and a test that forgets to ask for a fixture
# would otherwise join the owner's desk.
os.environ.setdefault(MESH_MULTICAST_DISCOVERY_ENVIRONMENT_VARIABLE, "0")


#: Set to 1 to run the tests an `awaiting_macos_parity` mark would not run on
#: macOS — what the ticket bringing them does to prove it.
RUN_AWAITING_MACOS_PARITY_ENVIRONMENT_VARIABLE = "STREAMLIB_RUN_AWAITING_MACOS_PARITY"

#: Set to 1, with someone listening, to run the tests an `audible_on_macos`
#: mark would not run on macOS. The standing sweep there stays silent.
RUN_ATTENDED_AUDIBLE_TESTS_ENVIRONMENT_VARIABLE = "STREAMLIB_RUN_ATTENDED_AUDIBLE_TESTS"


def pytest_collection_modifyitems(config: pytest.Config, items: "list[pytest.Item]") -> None:
    """Turns the platform markers and the audible marker into a skip or a strict xfail."""
    for item in items:
        audible = item.get_closest_marker("audible_on_macos")
        if audible is not None:
            reason = audible.kwargs.get("reason")
            if not reason:
                raise pytest.UsageError(f"{item.nodeid}: audible_on_macos needs reason=")
            if (
                sys.platform == "darwin"
                and os.environ.get(RUN_ATTENDED_AUDIBLE_TESTS_ENVIRONMENT_VARIABLE) != "1"
            ):
                item.add_marker(
                    pytest.mark.skip(
                        reason=f"audible on a Mac, so attended only ({reason}) — set "
                        f"{RUN_ATTENDED_AUDIBLE_TESTS_ENVIRONMENT_VARIABLE}=1 with someone listening"
                    )
                )
        linux_only = item.get_closest_marker("linux_only_capability")
        if linux_only is not None:
            reason = linux_only.kwargs.get("reason")
            if not reason:
                raise pytest.UsageError(f"{item.nodeid}: linux_only_capability needs reason=")
            if sys.platform != "linux":
                item.add_marker(pytest.mark.skip(reason=f"Linux-only: {reason}"))
        awaiting = item.get_closest_marker("awaiting_macos_parity")
        if awaiting is not None:
            issue = awaiting.kwargs.get("issue")
            if not isinstance(issue, int):
                raise pytest.UsageError(f"{item.nodeid}: awaiting_macos_parity needs issue=<number>")
            if sys.platform == "darwin":
                # Not run by default: a test waiting on another ticket's code
                # fails slowly, often at a timeout. The ticket that brings it
                # sets the variable, and the strict xfail then turns red the
                # moment the test passes, forcing the mark off.
                item.add_marker(
                    pytest.mark.xfail(
                        strict=True,
                        run=os.environ.get(RUN_AWAITING_MACOS_PARITY_ENVIRONMENT_VARIABLE) == "1",
                        reason=f"#{issue} brings this to macOS",
                    )
                )


@pytest.fixture
def start_app_under_test():
    """Hands out apps and kills their process groups no matter how a test ends.

    Without the teardown a failed assertion strands a live engine holding a GPU
    context, an iceoryx2 node and a socket, silently contaminating every later
    run on the same rig.

    `launcher` picks the launch arrangement — `python app.py` unless a suite
    names one of `app_under_test`'s others. The reaping is the same whichever
    it is, which is the point of routing them all through here.
    """
    started: "list[AppUnderTest]" = []

    def start(
        app_path: Path,
        *arguments: str,
        launcher: "Callable[..., AppUnderTest]" = start_app,
    ) -> AppUnderTest:
        app = launcher(app_path, *arguments)
        started.append(app)
        return app

    try:
        yield start
    finally:
        for app in started:
            app.kill_process_group()


def _remove_iceoryx2_domain_roots_whose_test_process_is_gone() -> None:
    """A root outlives its session: a node can still be dropping as the process
    exits, and iceoryx2 warns when its files vanish first. So a root is swept by
    a later session once the process named in it has gone."""
    for domain_root in SHARED_TEMPORARY_DIRECTORY.glob(f"{DOMAIN_ROOT_NAME_PREFIX}*"):
        try:
            owning_process_id = int(domain_root.name[len(DOMAIN_ROOT_NAME_PREFIX) :].split("-")[0])
            os.kill(owning_process_id, 0)
        except ProcessLookupError:
            shutil.rmtree(domain_root, ignore_errors=True)
        except (ValueError, PermissionError, OSError):
            continue


@pytest.fixture(scope="session")
def private_iceoryx2_domain_for_this_test_process() -> "Iterator[Path]":
    """Stands in for the parent runtime: gives this process's helper nodes an
    iceoryx2 domain of their own, handed over the way a parent hands a helper
    its root.

    A test that builds `ProcessorLinkDataAccess()` directly has no parent to
    hand it one, and a helper handed none refuses to start. Every node this
    process opens then shares the one domain, which is what lets a source and a
    destination built side by side reach each other, and no other process's
    nodes are in it. The root is short: iceoryx2's socket paths are budgeted.
    """
    _remove_iceoryx2_domain_roots_whose_test_process_is_gone()
    domain_root = Path(
        tempfile.mkdtemp(
            prefix=f"{DOMAIN_ROOT_NAME_PREFIX}{os.getpid()}-", dir=SHARED_TEMPORARY_DIRECTORY
        )
    )
    previous_domain_root = os.environ.get(ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
    os.environ[ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE] = str(domain_root)
    try:
        yield domain_root
    finally:
        if previous_domain_root is None:
            os.environ.pop(ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, None)
        else:
            os.environ[ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE] = previous_domain_root
