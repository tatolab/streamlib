# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors driven through `SingleProcessorTestPipeline`.

In their own module because that is what the harness is for: a user's processor
lives in an importable module, and its helper process imports exactly that. A
class declared inside a pytest module would have the child import the test
suite.
"""

from streamlib import RuntimeContextLimitedAccess, input, output, processor


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
