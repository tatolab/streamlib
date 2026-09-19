#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""One end of a cross-runtime link, authored in Python.

`cross_runtime_link_rig.rs` proves the link between two started runtimes from
Rust. This is the other authoring surface and the other placement: the source
is a Python processor, the reader spells the remote port with
`rt.remote_processor_output(...)`, and the destination is a *helper-placed*
Python processor — so the link's name has to survive the parent's wiring
envelope into a child interpreter to be read at all.

`--source` publishes the known signal, with nothing on its own runtime reading
it. `--reader <source runtime name>` pulls
`<that name>/KnownAudioSignalSource/audio` into the probe.

Audio rather than video for the Rust rig's reason: a video bag names a surface,
and a surface id means nothing on another machine, so the mesh carries no
pixels until #2290. An `AudioBlock`'s samples ride inline, and the known signal
needs no audio hardware.

Both ends host a control plane on loopback, because the arm reads `graph` on
each: the reader for the link's state and its tap, the source for
`mesh.egress_ports`.
"""

import argparse

import streamlib

# The display name the source gives its signal generator, and the half of the
# address the reader spells. Stated rather than defaulted so the two ends agree
# without either reading the other's code.
THE_SOURCES_DISPLAY_NAME = "KnownAudioSignalSource"

# The port the source publishes and the reader links from.
THE_PORT = "audio"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    end = parser.add_mutually_exclusive_group(required=True)
    end.add_argument(
        "--source",
        action="store_true",
        help="publish the known signal and offer it on the mesh",
    )
    end.add_argument(
        "--reader",
        metavar="SOURCE_RUNTIME_NAME",
        help="pull the named runtime's audio port into a helper-placed probe",
    )
    parser.add_argument("--control-plane-port", type=int, default=9000)
    arguments = parser.parse_args()

    runtime = streamlib.Runtime()
    if arguments.source:
        from known_audio_signal_source import KnownAudioSignalSource

        # Nothing here reads the port: the reader across the mesh is its only
        # consumer, which is the whole of what this end proves (#2344).
        runtime.add(KnownAudioSignalSource, display_name=THE_SOURCES_DISPLAY_NAME)
    else:
        from mesh_linked_audio_probe import MeshLinkedAudioProbe

        probe = runtime.add(MeshLinkedAudioProbe, display_name="probe")
        # The whole point of the arm: a port on another runtime, named by its
        # mesh address, wired with the same `connect` a local link takes. It
        # never waits on the network — the link is applied now and reads
        # `awaiting_remote` in `graph` until the source turns up.
        runtime.connect(
            runtime.remote_processor_output(
                arguments.reader, THE_SOURCES_DISPLAY_NAME, THE_PORT
            ),
            probe.input(THE_PORT),
        )

    # Loopback rather than the default every interface: this node exists to be
    # read from the machine it runs on, and it carries no authentication.
    runtime.host_control_plane(
        bind_host="127.0.0.1", bind_port=arguments.control_plane_port
    )
    runtime.run()


if __name__ == "__main__":
    main()
