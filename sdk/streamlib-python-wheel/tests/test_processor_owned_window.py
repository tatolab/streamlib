# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A window a Python processor owns, driven the way an author drives one.

`[processor-owned-windows]` says window ownership is a processor capability:
any processor may ask for a window and own its policy, and for an owner outside
the app process the engine runs that window's present loop, fed by surface ids
the owner names. This suite is that claim at the authoring surface — a
pip-installed processor asking for a debug window in `setup()` and naming
frames to it from `process()`, in its own helper process, beside the pipeline's
own display.

Display tier: every test here needs a GPU *and* a window server, so they run on
the rig only. What runs everywhere is the translation each `show()` performs,
which is the wheel crate's own Rust suite — a window this suite cannot open
would take the colour maths with it if it lived here.

Every probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line; the tests drive the app out of process and
assert on that line.
"""

import ctypes
import json
import re
import shutil
import subprocess
import time
from pathlib import Path

import pytest

from processor_owned_window_probes import (
    REQUESTED_WINDOW_HEIGHT,
    REQUESTED_WINDOW_WIDTH,
    WINDOW_TITLE,
)

pytestmark = pytest.mark.requires_gpu

APP = Path(__file__).parent / "processor_owned_window_app.py"

PROBE_RESULT = re.compile(r"MARKER:PROBE_RESULT (\{.*\})")

# Xlib's own constants, named here because the gesture below is the only
# place this suite speaks to the window server directly.
_X_CLIENT_MESSAGE = 33
_SUBSTRUCTURE_NOTIFY_MASK = 1 << 19
_SUBSTRUCTURE_REDIRECT_MASK = 1 << 20
_CLOSE_REQUESTED_BY_A_USER_ACTION = 2


def run_probe(
    start_app_under_test,
    probe_class_name: str,
    *,
    scenario: str = "beside_a_display_window",
    source: str = "test_pattern",
) -> dict:
    """One probe, one observation dict — or a failure carrying the probe's own
    traceback, which names the cause better than a missing marker."""
    app = start_app_under_test(APP, scenario, probe_class_name, source)
    app.await_output_containing(
        "MARKER:PROBE_RESULT", f"the {probe_class_name} result"
    )
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    match = PROBE_RESULT.search(app.output)
    assert match is not None, f"no parseable probe result:\n{app.output}"
    observation = json.loads(match.group(1))
    if "failure" in observation:
        pytest.fail(f"the probe raised in its helper process:\n{observation['failure']}")
    return observation


def test_all_three_ways_of_naming_a_published_surface_reach_the_window(
    start_app_under_test,
):
    """The whole authoring surface of `show()`: a cast object, a handle a
    kernel wrote, and a bare id."""
    observed = run_probe(start_app_under_test, "EveryArgumentShapeReachesTheWindowProbe")

    assert observed["shapes_accepted"] == [
        "kernel_output_handle",
        "cast_object",
        "bare_surface_id",
    ], (
        "every shape that names a published surface must reach the window — a "
        "kernel's own output above all, which no bag ever carried"
    )
    assert observed["window_title"] == WINDOW_TITLE
    assert observed["is_closed"] is False


def test_the_window_reports_an_extent_of_its_own(start_app_under_test):
    """Not the requested one: the window server is free to hand back another,
    and the owner is told what it actually got."""
    observed = run_probe(start_app_under_test, "EveryArgumentShapeReachesTheWindowProbe")

    assert observed["drained_width"] > 0 and observed["drained_height"] > 0, (
        "a drain must report the window's real drawable extent; zero means the "
        "present target never told the owner what it minted"
    )
    assert observed["drained_width"] <= REQUESTED_WINDOW_WIDTH * 4
    assert observed["drained_height"] <= REQUESTED_WINDOW_HEIGHT * 4
    assert observed["close_requested_by_user"] is False
    assert observed["window_is_closed"] is False


def test_a_closed_window_leaves_the_pipeline_running_and_every_show_a_no_op(
    start_app_under_test,
):
    """A user's gesture must never become an exception in a per-frame path, so
    neither does the owner's own close."""
    observed = run_probe(start_app_under_test, "AnOwnerClosingItsOwnWindowProbe")

    assert observed["closed_before_close"] is False
    assert observed["closed_after_close"] is True
    assert observed["window_is_closed_after_drain"] is True
    # The probe reached its report after the close, having called `show()`
    # three more times — in all three argument shapes — without raising.


def test_a_process_that_can_get_no_window_raises_at_setup(start_app_under_test):
    """The refusal an author wraps in `try/except` when the window is
    optional, carrying the pump's own account of why."""
    observed = run_probe(
        start_app_under_test,
        "AProcessThatCanGetNoWindowRefusesAtSetupProbe",
        scenario="with_no_display_server",
    )

    assert observed["window_was_granted"] is False, (
        "a process with no display server must be refused, never handed a "
        "window object that silently shows nothing"
    )
    refusal = observed["refusal"]
    assert WINDOW_TITLE in refusal, (
        f"the refusal must name the window that could not be had, got: {refusal}"
    )
    assert "window event pump" in refusal or "event loop" in refusal, (
        f"the refusal must carry the pump's own account of why, got: {refusal}"
    )
    assert "setup" not in refusal, (
        "a process with no display server is not a phase error — reporting it "
        f"as one sends the author moving the call rather than handling the "
        f"refusal, got: {refusal}"
    )


def test_the_optional_window_pattern_leaves_the_processor_running(
    start_app_under_test,
):
    """The `try/except` is the whole pattern: no window, no exception out of
    `setup`, and a processor that goes on doing its work."""
    observed = run_probe(
        start_app_under_test,
        "AProcessThatCanGetNoWindowRefusesAtSetupProbe",
        scenario="with_no_display_server",
    )

    # Reaching the marker at all is the assertion: the probe reports from
    # `setup` after catching, and an uncaught raise would have taken the
    # processor down before any line was written.
    assert observed["window_was_granted"] is False


class _XClientMessageEvent(ctypes.Structure):
    """`XClientMessageEvent`, padded out to the size of the `XEvent` union.

    Xlib reads an `XEvent` whatever the member, so a short struct would have
    it read past this allocation.
    """

    _fields_ = [
        ("type", ctypes.c_int),
        ("serial", ctypes.c_ulong),
        ("send_event", ctypes.c_int),
        ("display", ctypes.c_void_p),
        ("window", ctypes.c_ulong),
        ("message_type", ctypes.c_ulong),
        ("format", ctypes.c_int),
        ("data", ctypes.c_long * 5),
        ("padding_to_the_xevent_union", ctypes.c_long * 16),
    ]


def close_the_window_the_way_a_user_would(window_id: str) -> None:
    """Ask the window manager to close one window, by id.

    Exactly what clicking the titlebar's X does: `_NET_CLOSE_WINDOW` to the
    root, and the window manager sends the client the `WM_DELETE_WINDOW` the
    pump listens for.

    Not `xdotool windowclose`, which destroys the X window outright — the
    client never hears a close request and its swapchain simply goes invalid
    under it. Not the close chord either: that lands on whatever holds focus,
    and this suite runs on a rig somebody is working on.
    """
    xlib = ctypes.cdll.LoadLibrary("libX11.so.6")
    xlib.XOpenDisplay.restype = ctypes.c_void_p
    xlib.XInternAtom.restype = ctypes.c_ulong
    xlib.XDefaultRootWindow.restype = ctypes.c_ulong
    display = xlib.XOpenDisplay(None)
    assert display, "no display to send the close gesture on"
    try:
        gesture = _XClientMessageEvent(
            type=_X_CLIENT_MESSAGE,
            window=int(window_id),
            message_type=xlib.XInternAtom(
                ctypes.c_void_p(display), b"_NET_CLOSE_WINDOW", False
            ),
            format=32,
            # Timestamp `CurrentTime`, then the source indication a window
            # manager reads as "a user did this", which is what makes the
            # request one it honours rather than one it may defer.
            data=(ctypes.c_long * 5)(0, _CLOSE_REQUESTED_BY_A_USER_ACTION, 0, 0, 0),
        )
        xlib.XSendEvent(
            ctypes.c_void_p(display),
            xlib.XDefaultRootWindow(ctypes.c_void_p(display)),
            False,
            _SUBSTRUCTURE_NOTIFY_MASK | _SUBSTRUCTURE_REDIRECT_MASK,
            ctypes.byref(gesture),
        )
        xlib.XFlush(ctypes.c_void_p(display))
    finally:
        xlib.XCloseDisplay(ctypes.c_void_p(display))


def the_window_titled(title: str) -> str:
    """The window server's id for the one visible window under `title`.

    `--onlyvisible` because a bare search also returns unmapped windows, and a
    window that exists but was never mapped is not one a user could close.
    """
    found = subprocess.run(
        ["xdotool", "search", "--onlyvisible", "--name", f"^{title}$"],
        capture_output=True,
        text=True,
        check=False,
    )
    ids = [line for line in found.stdout.splitlines() if line.strip()]
    assert ids, f"no visible window titled {title!r} — the owner never got one"
    return ids[-1]


def test_a_users_close_leaves_the_pipeline_running_and_the_owner_informed(
    start_app_under_test,
):
    """The gesture itself, performed against the window server.

    The owner's own `close()` is covered above; this is the half only a real
    user can do — and the half the contract is written for, because a window
    someone shut must never take a running pipeline down with it.
    """
    if shutil.which("xdotool") is None:
        pytest.skip("xdotool is what finds the window the gesture is aimed at")

    app = start_app_under_test(
        APP,
        "beside_a_display_window",
        "EveryArgumentShapeReachesTheWindowProbe",
        "test_pattern",
    )
    app.await_output_containing("MARKER:PROBE_RESULT", "the window to be up and presenting")

    close_the_window_the_way_a_user_would(the_window_titled(WINDOW_TITLE))
    app.await_output_containing(
        "MARKER:THE_USER_CLOSED_THE_WINDOW", "the owner to notice the gesture"
    )

    reacted = re.search(r"MARKER:THE_USER_CLOSED_THE_WINDOW (\{.*\})", app.output)
    assert reacted is not None, f"no parseable close reaction:\n{app.output}"
    observed = json.loads(reacted.group(1))
    assert observed["close_gestures_reported"] == 1, (
        "a gesture is reported exactly once — the drain that reports it clears "
        "it — and the owner drained on every frame after it"
    )
    assert observed["window_is_closed"] is True, (
        "the engine closes the window on an unread close request; an owner "
        "reacts to the gesture and cannot veto it"
    )
    assert observed["is_closed"] is True

    # The pipeline is still running with the window gone — including the
    # `show()` calls the probe went on making, which no-op rather than raise.
    time.sleep(1.0)
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()
    assert app.output.count("MARKER:THE_USER_CLOSED_THE_WINDOW") == 1


def test_showing_something_that_names_no_surface_is_refused_by_the_three_shapes(
    start_app_under_test,
):
    """Refused in the caller's own stack, naming what it could have been
    given — never a round trip that comes back "the parent refused"."""
    observed = run_probe(
        start_app_under_test, "ShowingSomethingThatNamesNoSurfaceIsRefusedProbe"
    )

    for refusal in (
        observed["refusal_for_an_object_naming_nothing"],
        observed["refusal_for_an_integer"],
    ):
        assert "read(port, into=T)" in refusal, refusal
        assert "GpuSurfaceHandle" in refusal, refusal
        assert "surface id" in refusal, refusal
