# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors driven through `SingleProcessorTestPipeline`.

In their own module because that is what the harness is for: a user's processor
lives in an importable module, and its helper process imports exactly that. A
class declared inside a pytest module would have the child import the test
suite.
"""

import numpy

from streamlib import AudioBlock, RuntimeContextLimitedAccess, input, output, processor


@processor
class DoublingFilter:
    """One input, one output — the shape the harness exists to drive."""

    @input(delivery_profile="every_sample")
    def numbers_from_upstream(self) -> None: ...

    @output()
    def numbers_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("numbers_from_upstream")
        if bag is None:
            return
        ctx.outputs.write("numbers_to_downstream", {"value": bag["value"] * 2})


@processor
class ConfiguredScaler:
    """Reads its factor from config, so the harness's `config=` is exercised."""

    def __init__(self, factor: int = 1) -> None:
        self.factor = factor

    @input(delivery_profile="every_sample")
    def numbers_from_upstream(self) -> None: ...

    @output()
    def numbers_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        bag = ctx.inputs.read("numbers_from_upstream")
        if bag is None:
            return
        ctx.outputs.write(
            "numbers_to_downstream", {"value": bag["value"] * self.factor}
        )


@processor
class AudioBlockInspector:
    """Reads an audio block as `AudioBlock` and reports what the view saw.

    The payload goes back out on the reply so the collector has to carry a
    byte buffer of its own — a bag whose samples never make it home reads as
    a processor that produced nothing.
    """

    @input(delivery_profile="every_sample")
    def audio_from_upstream(self) -> None: ...

    @output()
    def readings_to_downstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        block = ctx.inputs.read("audio_from_upstream", into=AudioBlock)
        if block is None:
            return
        samples = block.samples
        viewed = samples
        while viewed.base is not None and isinstance(viewed.base, numpy.ndarray):
            viewed = viewed.base
        ctx.outputs.write(
            "readings_to_downstream",
            {
                "samples": block.interleaved_sample_bytes,
                "shape": list(samples.shape),
                "numpy_type": samples.dtype.str,
                # Promoted before the magnitude: `numpy.abs` keeps `int16`,
                # where -32768 has no positive counterpart and comes back
                # negative.
                "loudest_sample": float(numpy.max(numpy.abs(samples.astype("<f8")))),
                "first_sample_timestamp_ns": block.first_sample_timestamp_ns,
                "samples_are_a_view_over_the_bag_bytes": (
                    viewed.base is block.interleaved_sample_bytes
                ),
            },
        )
