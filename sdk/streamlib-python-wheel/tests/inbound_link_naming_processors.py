# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The many-input processor `test_inbound_link_naming.py` drives.

In its own module because a helper process imports the class by its import
path, and a class declared inside a pytest module would have the child import
the test suite.
"""

from streamlib import (
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    input,
    output,
    processor,
)


@processor
class ReportsWhichLinkEachBagCameFrom:
    """One input port, any number of producers into it.

    The shape every many-track sink has: it never declares a port per producer,
    it reads the one port and asks each bag which link it arrived on.
    """

    @input(delivery_profile="ordered")
    def tracks(self) -> None: ...

    @output()
    def attributions_to_downstream(self) -> None: ...

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        # Links are wired before setup() runs, so a sink knows here — before a
        # single bag has arrived — how many producers it owes.
        self.links_at_setup = sorted(ctx.inputs.inbound_link_names("tracks"))

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        read = ctx.inputs.read_from_inbound_link("tracks")
        if read is None:
            return
        bag, inbound_link = read
        ctx.outputs.write(
            "attributions_to_downstream",
            {
                "value": bag["value"],
                "arrived_on": inbound_link,
                "links_at_setup": self.links_at_setup,
            },
        )
