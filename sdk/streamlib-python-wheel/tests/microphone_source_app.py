# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The audio built-in feeding one Python processor in its real placement.

Added by `stream.add` with no `config` at all — the spelling the plan blesses
for a block that needs no configuration, and the one that reaches the backend's
default device.
"""

import streamlib
from microphone_source_probes import AudioBlockProbe
from streamlib import Stream, compile_stream_to_graph, stream


@stream
def microphone_into_an_audio_block_probe(stream: Stream) -> None:
    microphone = stream.add(streamlib.MicrophoneSource)
    probe = stream.add(AudioBlockProbe)
    stream.connect(microphone.output("audio"), probe.input("audio_from_upstream"))


def main() -> None:
    graph = compile_stream_to_graph(microphone_into_an_audio_block_probe)
    runtime = streamlib.Runtime()
    runtime.load(graph)
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
