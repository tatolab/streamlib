# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processor classes the helper-process suite loads by import path.

They live in their own module, not in the test module, because that is the
whole contract under test: a helper process reaches a class by importing the
module it was declared in, and importing a pytest module would re-run the
suite inside the child.
"""

import time
from typing import TypedDict

from streamlib import input, output, processor


class PassThroughProbeConfig(TypedDict, total=False):
    """A TypedDict config, so a real helper run covers that kind too."""

    tag: str


@processor
class PassThroughProbe:
    """Copies every bag from its input to its output."""

    def __init__(self, config: PassThroughProbeConfig) -> None:
        self.tag = config.get("tag", "untagged")

    @input(delivery_profile="newest")
    def frames_from_upstream(self) -> None: ...

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("frames_from_upstream")
        if bag is not None:
            ctx.outputs.write("frames_to_downstream", {**bag, "tag": self.tag})


class OuterProbe:
    """Holds a nested processor, so the dotted-qualname walk has a target."""

    @processor(execution="manual")
    class InnerProbe:
        @output()
        def frames_to_downstream(self) -> None: ...


@processor(execution="manual")
class RefusesSetupProbe:
    """Raises out of `setup`, which the parent must hear about."""

    @output()
    def frames_to_downstream(self) -> None: ...

    def setup(self, ctx) -> None:
        raise RuntimeError("this processor cannot set itself up")


@processor
class SlowPassThroughProbe:
    """Copies every bag to its output, slower than a burst arrives."""

    @input(delivery_profile="newest")
    def frames_from_upstream(self) -> None: ...

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        bag = ctx.inputs.read("frames_from_upstream")
        if bag is not None:
            time.sleep(0.005)
            ctx.outputs.write("frames_to_downstream", bag)


# The hooks the interrupt probes below actually reached, in order. A test reads
# it to tell "the interrupt ended the helper" from "the interrupt was absorbed
# and the rest of the ladder ran".
HOOKS_THE_INTERRUPT_PROBES_REACHED: list[str] = []


@processor(execution="continuous", interval_ms=0)
class InterruptedInProcessProbe:
    """Takes a `KeyboardInterrupt` inside `process()`, the way the parent's
    shutdown ladder delivers one to a callback that outran its budget."""

    @output()
    def frames_to_downstream(self) -> None: ...

    def __init__(self) -> None:
        self.already_took_the_interrupt = False

    def process(self, ctx) -> None:
        if not self.already_took_the_interrupt:
            self.already_took_the_interrupt = True
            HOOKS_THE_INTERRUPT_PROBES_REACHED.append("process-interrupted")
            raise KeyboardInterrupt
        time.sleep(0.005)

    def stop(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("stop")

    def teardown(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("teardown")


@processor(execution="manual")
class InterruptedInSetupProbe:
    """Takes a `KeyboardInterrupt` inside `setup()`. Unlike a `setup()` that
    raises on its own, this one is still owed its `teardown()`."""

    @output()
    def frames_to_downstream(self) -> None: ...

    def setup(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("setup-interrupted")
        raise KeyboardInterrupt

    def teardown(self, ctx) -> None:
        HOOKS_THE_INTERRUPT_PROBES_REACHED.append("teardown")


# When the pacing probe's `process()` ran, in monotonic nanoseconds. A test
# reads it to measure how often a continuous loop called the processor.
WHEN_THE_PACING_PROBE_PROCESSED_NS: list[int] = []


@processor(execution="continuous", interval_ms=250)
class ContinuousPacingProbe:
    """Records when each `process()` ran, so the interval the loop kept is
    measurable. The interval under test is the one the parent's `run` names."""

    @output()
    def frames_to_downstream(self) -> None: ...

    def process(self, ctx) -> None:
        WHEN_THE_PACING_PROBE_PROCESSED_NS.append(time.monotonic_ns())


class ReconfigurableProbeConfig(TypedDict, total=False):
    gain: int


@processor(execution="manual")
class RefusesReconfigurationProbe:
    """Defines `configure` and refuses every configuration handed to it."""

    @output()
    def frames_to_downstream(self) -> None: ...

    def __init__(self, config: ReconfigurableProbeConfig) -> None:
        self.gain = config.get("gain", 1)

    def configure(self, config: ReconfigurableProbeConfig) -> None:
        raise ValueError(f"a gain of {config.get('gain')} is out of range")


@processor(execution="manual")
class TakesReconfigurationProbe:
    """Defines `configure` and takes whatever it is handed."""

    @output()
    def frames_to_downstream(self) -> None: ...

    def __init__(self, config: ReconfigurableProbeConfig) -> None:
        self.gain = config.get("gain", 1)

    def configure(self, config: ReconfigurableProbeConfig) -> None:
        self.gain = config.get("gain", 1)
