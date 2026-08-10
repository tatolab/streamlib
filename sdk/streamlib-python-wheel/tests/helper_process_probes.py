# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processor classes the helper-process suite loads by import path.

They live in their own module, not in the test module, because that is the
whole contract under test: a helper process reaches a class by importing the
module it was declared in, and importing a pytest module would re-run the
suite inside the child.
"""

from streamlib import input, output, processor


@processor
class PassThroughProbe:
    """Copies every bag from its input to its output."""

    def __init__(self, tag: str = "untagged") -> None:
        self.tag = tag

    @input(delivery_profile="latest")
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
