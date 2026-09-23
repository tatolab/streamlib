# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run one pixel-exchange probe in its real placement.

Run as a real `python app.py`: the probe executes in a helper process, and its
observation reaches this app — and the test driving it — over the same log
forwarding every child's records ride.
"""

import sys

import streamlib
import pixel_exchange_probes


def scenario_probe(probe_class_name: str) -> None:
    """One probe, one graph. The probe reports from `setup`, so the graph has
    nothing to do but exist until the test has read the result and interrupts."""
    runtime = streamlib.Runtime()
    runtime.add(getattr(pixel_exchange_probes, probe_class_name))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def scenario_inverting_effect() -> None:
    """Native source → Python effect, the user-facing story: the frames a
    native processor produces are edited in place by a child interpreter."""
    runtime = streamlib.Runtime()
    pattern = runtime.add(
        streamlib.TestPatternSource, config={"width": 320, "height": 180}
    )
    effect = runtime.add(pixel_exchange_probes.InvertingEffect)
    runtime.connect(
        pattern.output("video"), effect.input("video_from_upstream")
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


def scenario_cross_process_edit(skip_edit: bool) -> None:
    """Native source → Python effect → a second Python processor: the edit
    one child makes is read back by another, through the engine's memory."""
    runtime = streamlib.Runtime()
    pattern = runtime.add(
        streamlib.TestPatternSource, config={"width": 320, "height": 180}
    )
    effect = runtime.add(
        pixel_exchange_probes.ReportingInvertingEffect, config={"skip_edit": skip_edit}
    )
    verifier = runtime.add(pixel_exchange_probes.FrameDigestVerifier)
    runtime.connect(pattern.output("video"), effect.input("video_from_upstream"))
    runtime.connect(effect.output("video_to_downstream"), verifier.input("video_from_upstream"))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "inverting_effect":
        scenario_inverting_effect()
    elif sys.argv[1] == "cross_process_edit":
        scenario_cross_process_edit(skip_edit=False)
    elif sys.argv[1] == "cross_process_edit_negative_control":
        scenario_cross_process_edit(skip_edit=True)
    else:
        scenario_probe(sys.argv[1])
