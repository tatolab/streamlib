# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Scenarios that run one compute-kernel probe in its real placement.

Run as a real `python app.py`: the probe builds and dispatches its kernel from
a helper process, and its observation reaches this app — and the test driving
it — over the child→parent log forwarding.
"""

import sys

import streamlib

import compute_kernel_probes


def scenario_standalone_probe(probe_class_name: str) -> None:
    """A kernel probe needs no upstream: it acquires both surfaces itself and
    reports from `setup`."""
    runtime = streamlib.Runtime()
    runtime.add(getattr(compute_kernel_probes, probe_class_name))
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    scenario_standalone_probe(sys.argv[1])
