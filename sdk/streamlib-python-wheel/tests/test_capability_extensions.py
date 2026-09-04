# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The capability-extension mechanism: discovery, both call sites, hard-fail.

`pip install` of a wheel declaring a `streamlib.extensions` entry point is the
whole of enabling it, so what these prove is that the engine reads pip's
registry and runs what it finds — in the app process as `Runtime()` is
constructed, and in each helper before the processor's module is imported —
and that a hook which cannot complete stops the process it was loading into
rather than half loading.

The fixture distributions are put on `sys.path` rather than installed: a
raising hook in the shared venv would fail every other suite's `Runtime()`.
`extension_fixtures/README.md` says why that is still pip's registry.
"""

import importlib
import json
import threading
from pathlib import Path
from typing import Any

import pytest

from streamlib import _capability_extensions
from streamlib._capability_extensions import (
    CapabilityExtensionLoadError,
    load_installed_capability_extensions_once_per_process,
    run_every_installed_capability_extension_hook,
)

CAPABILITY_EXTENSION_APP = Path(__file__).parent / "capability_extension_app.py"
EXTENSION_FIXTURES = Path(__file__).parent / "extension_fixtures"

#: Every distribution `extension_fixtures/` supplies. Assertions about which
#: hooks ran are filtered to these: discovery reads the whole environment, and
#: a venv that happens to carry a real extension wheel — a developer building
#: `streamlib-webrtc` beside this one — is not this suite's failure.
FIXTURE_DISTRIBUTIONS = frozenset(
    {
        "streamlib-test-extension",
        "streamlib-raising-extension",
        "streamlib-duplicate-extension",
        "streamlib-helper-raising-extension",
        "streamlib-blocking-extension",
    }
)


@pytest.fixture
def capability_extension_app(start_app_under_test):
    """Starts this suite's app; the shared fixture owns the cleanup."""
    return lambda scenario: start_app_under_test(CAPABILITY_EXTENSION_APP, scenario)


def fixture_distributions_handed_a_host(hosts) -> "list[str]":
    """Which of this suite's own fixtures the loop reached, in call order."""
    return [
        host.distribution
        for host in hosts
        if host.distribution in FIXTURE_DISTRIBUTIONS
    ]


def marker_value(app, prefix: str) -> str:
    """The single marker starting with `prefix`, minus the prefix."""
    matching = [marker for marker in app.markers() if marker.startswith(prefix)]
    assert len(matching) == 1, (
        f"expected exactly one {prefix!r} marker, got {matching}; output:\n{app.output}"
    )
    return matching[0][len(prefix) :]


# =============================================================================
# The app process
# =============================================================================


def test_an_installed_hook_runs_as_the_runtime_is_constructed(capability_extension_app):
    """`Runtime()` is the whole of enabling an installed extension.

    Nothing registers the extension with the app and nothing imports it: the
    engine reads `importlib.metadata` and calls what pip recorded.
    """
    app = capability_extension_app("a_hook_runs_and_registers")
    app.await_marker("EXTENSION_HOOK_RAN_AS_app")
    app.await_clean_exit()
    assert marker_value(app, "HOST_ROLE=") == "app"


def test_a_second_runtime_in_one_process_re_runs_no_hook(capability_extension_app):
    """A hook brings a stack up; bringing it up twice is what the latch stops."""
    app = capability_extension_app("the_hook_runs_once_however_many_runtimes")
    app.await_clean_exit()
    assert marker_value(app, "HOOK_CALL_COUNT=") == "1"


def test_a_raising_hook_fails_the_runtime_naming_the_distribution(
    capability_extension_app,
):
    """Hard-fail, not skip-and-log: a half-loaded extension fails per frame.

    The message has to carry both halves an operator needs — which entry point
    and which installed distribution — because neither is visible from the
    traceback of a hook that raises somewhere inside a vendored stack.
    """
    app = capability_extension_app("a_raising_hook_fails_the_runtime")
    app.await_clean_exit()

    refusal = marker_value(app, "RUNTIME_REFUSED=")
    assert "streamlib-raising-extension" in refusal, refusal
    assert "raising_extension" in refusal, refusal
    assert "this extension's stack could not be brought up" in refusal, refusal


def test_a_hook_that_failed_once_fails_every_later_runtime(capability_extension_app):
    """The cached failure, not a retry: the first `Runtime()` already half ran the loop.

    Both markers are load-bearing. `REFUSAL_COUNT` alone reads the same whether
    the failure was cached or the hook was run again — the call count is what
    tells them apart, and a hook that brings a stack up must not be re-entered
    after it failed partway.
    """
    app = capability_extension_app("a_raising_hook_keeps_failing_every_later_runtime")
    app.await_clean_exit()
    assert marker_value(app, "REFUSAL_COUNT=") == "2"
    assert marker_value(app, "HOOK_CALL_COUNT=") == "1"


def test_two_distributions_on_one_capability_name_refuse_naming_both(
    capability_extension_app,
):
    """Two wheels claiming one name is an installation only the operator can resolve."""
    app = capability_extension_app("two_distributions_on_one_capability_name")
    app.await_clean_exit()

    refusal = marker_value(app, "RUNTIME_REFUSED=")
    assert "streamlib-test-extension" in refusal, refusal
    assert "streamlib-duplicate-extension" in refusal, refusal
    assert "test-capability" in refusal, refusal


# =============================================================================
# The loop itself — the function both call sites run
# =============================================================================


class FakeCapabilityExtensionHost:
    """Stands in for the compiled host, which has no Python constructor."""

    def __init__(self, role: str, distribution: str) -> None:
        self.role = role
        self.distribution = distribution
        self.registered: "list[tuple[str, str]]" = []

    def register_capability(self, name: str, version: str) -> None:
        self.registered.append((name, version))


@pytest.fixture
def hosts_handed_to_hooks() -> "list[FakeCapabilityExtensionHost]":
    return []


@pytest.fixture
def mint_a_fake_helper_host(hosts_handed_to_hooks):
    def mint(distribution: str) -> Any:
        host = FakeCapabilityExtensionHost("helper", distribution)
        hosts_handed_to_hooks.append(host)
        return host

    return mint


@pytest.fixture
def installed_fixture_distributions(monkeypatch):
    """Puts named fixture distributions where `importlib.metadata` finds them."""

    def install(*variants: str) -> None:
        for variant in variants:
            monkeypatch.syspath_prepend(str(EXTENSION_FIXTURES / variant))

    return install


def test_the_loop_hands_each_hook_a_host_carrying_its_own_distribution(
    installed_fixture_distributions, mint_a_fake_helper_host, hosts_handed_to_hooks
):
    """One host per entry point — that is what lets a refusal name both wheels.

    This is the seam `_helper.py` runs, driven in process with a fake host: the
    helper's own call site has no runtime to register into, so the host it is
    handed is the whole of what a hook there can reach.
    """
    installed_fixture_distributions("registering")

    run_every_installed_capability_extension_hook(mint_a_fake_helper_host)

    assert fixture_distributions_handed_a_host(hosts_handed_to_hooks) == [
        "streamlib-test-extension"
    ]
    handed = hosts_handed_to_hooks[-1]
    assert handed.role == "helper"
    assert handed.registered == [("test-capability", "1.4.2")]


def test_the_loop_stops_at_the_first_hook_that_raises(
    installed_fixture_distributions, mint_a_fake_helper_host, hosts_handed_to_hooks
):
    """It raises rather than carrying on, and names what failed."""
    installed_fixture_distributions("raising")

    with pytest.raises(CapabilityExtensionLoadError) as refusal:
        run_every_installed_capability_extension_hook(mint_a_fake_helper_host)

    assert "streamlib-raising-extension" in str(refusal.value)
    assert "raising_extension" in str(refusal.value)
    assert refusal.value.__cause__ is not None, (
        "the hook's own exception must stay chained, or its traceback is lost"
    )


def test_no_installed_extensions_is_not_an_error(
    mint_a_fake_helper_host, hosts_handed_to_hooks
):
    """The overwhelmingly common case: nothing installed, nothing run."""
    run_every_installed_capability_extension_hook(mint_a_fake_helper_host)

    assert fixture_distributions_handed_a_host(hosts_handed_to_hooks) == []


def test_hooks_still_running_hold_off_a_second_thread_rather_than_re_running(
    monkeypatch,
    installed_fixture_distributions,
    mint_a_fake_helper_host,
    hosts_handed_to_hooks,
):
    """Two `Runtime()`s at once must not both run the hooks.

    A hook that brings a real stack up takes time, and the check-then-run in
    `load_installed_capability_extensions_once_per_process` spans it. Without
    the lock both threads pass the checks and both run every hook — and since
    the second registration of one capability name is a refusal, the latch
    would cache that refusal and fail every later `Runtime()` in the process,
    permanently.

    Deterministic, not a lucky interleaving: the fixture's hook blocks inside
    the first call until this test releases it, so the second thread is
    guaranteed to arrive mid-flight.
    """
    installed_fixture_distributions("blocking")
    # Imported dynamically because it only exists on `sys.path` once the line
    # above puts it there — a static import would read as an ordinary
    # dependency of this file, which it is not.
    blocking_extension = importlib.import_module("streamlib_blocking_extension")

    # The latch is module state; leave it as this test found it.
    monkeypatch.setattr(_capability_extensions, "_HOOKS_HAVE_RUN", False)
    monkeypatch.setattr(_capability_extensions, "_HOOK_FAILURE", None)

    def load_the_extensions() -> None:
        load_installed_capability_extensions_once_per_process(mint_a_fake_helper_host)

    try:
        first = threading.Thread(target=load_the_extensions, name="first-runtime")
        first.start()
        assert blocking_extension.hook_has_been_entered.wait(timeout=30.0), (
            "the hook never started; the fixture is not on the path"
        )

        second = threading.Thread(target=load_the_extensions, name="second-runtime")
        second.start()
        # The second thread is either blocked on the lock (correct) or already
        # inside the hook (the bug). Releasing lets both finish either way, so
        # the call count is what tells them apart.
        blocking_extension.hook_may_return.set()
        first.join(timeout=30.0)
        second.join(timeout=30.0)
        assert not first.is_alive() and not second.is_alive()

        assert blocking_extension.hook_call_count == 1
        assert fixture_distributions_handed_a_host(hosts_handed_to_hooks) == [
            "streamlib-blocking-extension"
        ]
    finally:
        blocking_extension.hook_may_return.set()


# =============================================================================
# The helper process — a real spawn
# =============================================================================


@pytest.mark.requires_gpu
def test_a_helper_runs_every_hook_before_it_imports_the_processor(
    capability_extension_app,
):
    """The child's own hooks, in the child's own interpreter.

    Ordering is the claim: the processor's module can reach for a stack the
    same wheel's extension brings up, so the hook has to have run by the time
    that module is imported.
    """
    app = capability_extension_app("a_helper_runs_the_hook_before_the_processor")
    app.await_marker("EXTENSION_HOOK_RAN_AS_helper")
    app.await_marker("HELPER_LOADED_THE_EXTENSION=True")
    app.await_clean_exit()


@pytest.mark.requires_gpu
def test_graph_renders_every_capability_the_app_process_registered(
    capability_extension_app,
):
    """The whole path, end to end: `register_capability` → the runtime → `graph`.

    Needs a real node because the control plane is a processor in the graph, so
    it only serves once the runtime is running.
    """
    app = capability_extension_app("graph_renders_the_registered_capability")
    app.await_clean_exit()

    extensions = json.loads(marker_value(app, "GRAPH_EXTENSIONS="))
    assert extensions == [
        {
            "name": "test-capability",
            "version": "1.4.2",
            "distribution": "streamlib-test-extension",
        }
    ]


@pytest.mark.requires_gpu
def test_a_raising_hook_in_a_helper_refuses_that_processor_by_name(
    capability_extension_app,
):
    """The helper exits before importing the processor; the parent says which one.

    Both halves of the report matter and neither is enough alone: the child's
    own log line is the only place the distribution and entry point are named,
    and the parent's refusal is the only place the processor is.
    """
    app = capability_extension_app("a_raising_hook_refuses_the_processor")
    app.await_clean_exit()

    assert "the helper could not load a capability extension" in app.output
    assert "streamlib-helper-raising-extension" in app.output
    assert "helper_raising_extension" in app.output
    assert (
        "[ReportsTheExtensionItsHelperLoaded] its helper process died before it "
        "finished setting up" in app.output
    ), app.output

    # The processor never reached Running, so the app's own wait refused it.
    assert marker_value(app, "PROCESSOR_REFUSED=") != "it started anyway"
