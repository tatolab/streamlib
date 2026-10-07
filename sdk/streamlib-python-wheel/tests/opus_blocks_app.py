# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stereo tone encoded and decoded back, with no Python in the codec path.

`StereoToneSource → OpusEncoder → OpusDecoder`, a probe fanned off each of the
two links. The source states 48 kHz stereo `f32`, which is what the encoder's
window contract asks the stage to resample to — so nothing between the source
and the measurement is a resampler, and the channel count the encoder follows
is this app's own fact.

Two probes off one run rather than two runs is what makes the trim assertion
possible: a decoded block's stamp is paired against the stamp of the encoded
packet a lookahead later, and two runs would have two anchors and nothing to
pair across.

The source publishes 480-sample blocks and the encoder's port declares
960/960, so the window stage frames two source blocks into each Opus packet —
there is no rechunker between them and no configuration that could add one.
"""

import threading

import tatolab.runtime
import tatolab.stream
from opus_blocks_probes import (
    DecodedAudioBlockProbe,
    EncodedAudioPacketProbe,
    StereoToneSource,
)
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream

READINESS_TIMEOUT_SECONDS = 20.0


@stream
def stereo_tone_through_the_opus_pair_probed_on_both_links(stream_builder: StreamBuilder) -> None:
    source = stream_builder.add(StereoToneSource)
    encoder = stream_builder.add(tatolab.stream.OpusEncoder)
    decoder = stream_builder.add(tatolab.stream.OpusDecoder)
    encoded_probe = stream_builder.add(EncodedAudioPacketProbe)
    decoded_probe = stream_builder.add(DecodedAudioBlockProbe)

    stream_builder.connect(source.output("audio"), encoder.input("audio"))
    stream_builder.connect(encoder.output("encoded_audio"), decoder.input("encoded_audio"))
    stream_builder.connect(
        encoder.output("encoded_audio"),
        encoded_probe.input("encoded_audio_from_upstream"),
    )
    stream_builder.connect(
        decoder.output("audio"), decoded_probe.input("audio_from_upstream")
    )


def main() -> None:
    graph = compile_stream_to_graph(
        stereo_tone_through_the_opus_pair_probed_on_both_links
    )
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)

    def watch_readiness() -> None:
        try:
            runtime.wait_until_every_node_is_running(
                timeout=READINESS_TIMEOUT_SECONDS
            )
            print("MARKER:EVERY_PROCESSOR_RUNNING", flush=True)
        except RuntimeError as refusal:
            print(f"MARKER:NOT_EVERY_PROCESSOR_RUNNING {refusal}", flush=True)
            runtime.shutdown()

    threading.Thread(target=watch_readiness, daemon=True).start()
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
