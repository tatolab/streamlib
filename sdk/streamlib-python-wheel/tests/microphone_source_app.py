# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The audio built-in feeding one Python processor in its real placement.

Added by `stream_builder.add` with no `config` at all — the spelling the plan blesses
for a block that needs no configuration, and the one that reaches the backend's
default device.
"""

import sys
from pathlib import Path

import tatolab.runtime
import tatolab.stream
from microphone_source_probes import AudioBlockProbe
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream


@stream
def microphone_into_an_audio_block_probe(stream_builder: StreamBuilder) -> None:
    microphone = stream_builder.add(tatolab.stream.MicrophoneSource)
    probe = stream_builder.add(AudioBlockProbe)
    stream_builder.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


def main() -> None:
    graph = compile_stream_to_graph(microphone_into_an_audio_block_probe)
    runtime = tatolab.runtime.Runtime()
    runtime.load(
        graph,
        project_directory=Path(__file__).resolve().parent,
        interpreter=sys.executable,
    )
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
