# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A stereo tone encoded and decoded back, with no Python in the codec path.

`StereoToneSource → OpusEncoder → OpusDecoder`, a probe fanned off each of the
two links. The source states 48 kHz stereo `f32`, which is what the encoder's
window contract asks the stage to resample to — so nothing between the source
and the measurement is a resampler, and the channel count the encoder follows
is this app's own fact.

Two probes off one run rather than two runs is what makes the trim assertion
possible: the decoded side's entry block has to be matched against the stamp
of the encoded packet the decoder anchored on, and only one run has both.

The source publishes 480-sample blocks and the encoder's port declares
960/960, so the window stage frames two source blocks into each Opus packet —
there is no rechunker between them and no configuration that could add one.
"""

import threading

import streamlib
from opus_blocks_probes import (
    DecodedAudioBlockProbe,
    EncodedAudioPacketProbe,
    StereoToneSource,
)

READINESS_TIMEOUT_SECONDS = 20.0


def main() -> None:
    runtime = streamlib.Runtime()
    source = runtime.add(StereoToneSource)
    encoder = runtime.add(streamlib.OpusEncoder)
    decoder = runtime.add(streamlib.OpusDecoder)
    encoded_probe = runtime.add(EncodedAudioPacketProbe)
    decoded_probe = runtime.add(DecodedAudioBlockProbe)

    runtime.connect(source.output("audio"), encoder.input("audio"))
    runtime.connect(encoder.output("encoded_audio"), decoder.input("encoded_audio"))
    runtime.connect(
        encoder.output("encoded_audio"),
        encoded_probe.input("encoded_audio_from_upstream"),
    )
    runtime.connect(
        decoder.output("audio"), decoded_probe.input("audio_from_upstream")
    )

    def watch_readiness() -> None:
        try:
            runtime.wait_until_every_processor_is_running(
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
