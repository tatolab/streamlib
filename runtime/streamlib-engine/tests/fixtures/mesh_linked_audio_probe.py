# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A helper-placed consumer of a link that carries from another runtime.

The point of the fixture is that there is nothing remote about reading one. The
processor declares an ordinary `ordered` input, reads with the ordinary fan-in
read, and the only thing that says a machine boundary was crossed is the *name*
the read hands back: `<runtime name>/<display name>/<port>`, the port's address
on the mesh, rather than the hashed channel its ingress actually writes.

It runs in its own helper process like every Python processor, so it also
reports its pid — the link name has to survive the parent's wiring envelope to
get here, which is the half of the contract no in-process test can reach.

Everything it reports goes through the log at a marker prefix, because a
fixture that has to be read by a shell script and a fixture that has to be read
by a person want the same line.
"""

import os

from streamlib import AudioBlock, RuntimeContextLimitedAccess, input, log, processor

# What the shell arm greps for. One prefix per fact, so a missing one names
# itself rather than showing up as a parse failure.
ENUMERATED_MARKER = "MARKER:INBOUND_LINKS "
RECEIVED_MARKER = "MARKER:RECEIVED "


@processor
class MeshLinkedAudioProbe:
    """Reads audio off whatever is wired into it and names the link it came on."""

    def __init__(self) -> None:
        self._samples_by_inbound_link: "dict[str, int]" = {}
        self._bags_by_inbound_link: "dict[str, int]" = {}
        self._said_what_it_received = False

    @input(delivery_profile="ordered")
    def audio(self) -> None: ...

    def setup(self, ctx: RuntimeContextLimitedAccess) -> None:
        # Readable here because links are wired before setup runs — which is how
        # a many-track sink learns how many tracks it owes before a bag lands.
        # A remote link must be in this list under its address, not its channel.
        log.info(
            ENUMERATED_MARKER + ",".join(ctx.inputs.inbound_link_names("audio")),
            helper_pid=os.getpid(),
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        while (read := ctx.inputs.read_from_inbound_link("audio", into=AudioBlock)) is not None:
            block, inbound_link = read
            self._bags_by_inbound_link[inbound_link] = (
                self._bags_by_inbound_link.get(inbound_link, 0) + 1
            )
            self._samples_by_inbound_link[inbound_link] = (
                self._samples_by_inbound_link.get(inbound_link, 0) + block.sample_count
            )
            self._say_what_it_received()

    def teardown(self, ctx: RuntimeContextLimitedAccess) -> None:
        # Said again at the end whatever happened, so a run that never reached
        # the threshold still reports what it did get rather than nothing.
        self._said_what_it_received = False
        self._say_what_it_received()

    def _say_what_it_received(self) -> None:
        """Report once per link, the first time it has carried real audio.

        Once rather than per bag: the marker is a fact about the link — that it
        carried, and under which name — and repeating it every 10 ms would bury
        the rest of the log the arm has to read on a failure.
        """
        if self._said_what_it_received or not self._bags_by_inbound_link:
            return
        self._said_what_it_received = True
        for inbound_link, bags in sorted(self._bags_by_inbound_link.items()):
            log.info(
                f"{RECEIVED_MARKER}{inbound_link} "
                f"bags={bags} samples={self._samples_by_inbound_link[inbound_link]}",
                helper_pid=os.getpid(),
            )
