# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One processor-owned-window probe against a real source, in its real placement.

Run as a real `python app.py`: the probe owns its window from its own helper
process while the engine presents it here, and its observation reaches this app
— and the test driving it — over the child→parent log forwarding.

A `DisplayWindow` rides alongside every probe that gets a window at all. Two
windows on the app process's one event pump is the arrangement that pump exists
for — the probe owning one of them runs in its own helper process, as every
Python processor does — and a processor-owned window that only worked as the
sole window would be a regression nobody would see with one on screen.

The headless scenario is the same app with both display-server variables taken
away before the engine boots — the arrangement a container gives an author who
never asked for a window.
"""

import os
import sys

import streamlib

import processor_owned_window_probes


def _source(runtime: "streamlib.Runtime", source_name: str):
    if source_name == "camera":
        return runtime.add(
            streamlib.CameraSource,
            config={
                "device_id": os.environ.get("STREAMLIB_CAMERA_DEVICE", "/dev/video0")
            },
        )
    if source_name == "test_pattern":
        return runtime.add(
            streamlib.TestPatternSource, config={"width": 640, "height": 480}
        )
    raise SystemExit(f"unknown source {source_name!r}: use 'camera' or 'test_pattern'")


def scenario_beside_a_display_window(probe_class_name: str, source_name: str) -> None:
    """The arrangement a debug window is really used in: the pipeline's own
    display up, and a processor's window beside it."""
    runtime = streamlib.Runtime()
    source = _source(runtime, source_name)
    probe = runtime.add(getattr(processor_owned_window_probes, probe_class_name))
    display = runtime.add(streamlib.DisplayWindow, config={"title": DISPLAY_TITLE})
    runtime.connect(source.output("video"), probe.input("video_from_upstream"))
    runtime.connect(source.output("video"), display.input("video"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def scenario_with_no_display_server(probe_class_name: str, source_name: str) -> None:
    """The same app on a process that can get no window at all.

    Both variables go before anything reads them: winit picks X11 off `DISPLAY`
    and Wayland off `WAYLAND_DISPLAY`, and leaving either behind would leave
    the process able to get a window after all. No `DisplayWindow` here — it
    would fail for the same reason and take the app down before the probe could
    report the refusal it is testing.
    """
    os.environ.pop("DISPLAY", None)
    os.environ.pop("WAYLAND_DISPLAY", None)
    runtime = streamlib.Runtime()
    source = _source(runtime, source_name)
    probe = runtime.add(getattr(processor_owned_window_probes, probe_class_name))
    runtime.connect(source.output("video"), probe.input("video_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


# Deliberately not a superstring of the probe's own window title: a test
# looking one of them up by name would otherwise find both.
DISPLAY_TITLE = "streamlib harness — the pipeline's own display"

SCENARIOS = {
    "beside_a_display_window": scenario_beside_a_display_window,
    "with_no_display_server": scenario_with_no_display_server,
}


if __name__ == "__main__":
    SCENARIOS[sys.argv[1]](sys.argv[2], sys.argv[3])
