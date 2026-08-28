# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A microphone wired straight to a speaker, with no Python in the sample path.

Added with no `config` on either end, so this is also the added-without-config
proof for the playback built-in: the config travels to the engine as JSON and
every field of a built-in's config struct carries a serde default, so `{}`
deserializes and `null` does not.

The two ends must agree on rate, channels and dtype, because there is no
resampler on this rung and `SpeakerSink` refuses what it cannot play. That
holds by construction on the null backend — both ends take the pacing clock's
rate and one channel — and on a session whose default source and default sink
run the same format, which is the ordinary desktop case. Where they differ the
refusal names both, which is the behaviour under test rather than a flake.

A probe hangs off the same output the speaker reads, so the test has a marker
saying enough blocks have really flowed rather than a sleep guessing that they
have. It is a second consumer of the microphone's port, not a stage between the
two built-ins — the samples the speaker plays never enter an interpreter.
"""

import threading

import streamlib
from speaker_sink_probes import AudioBlockCountingProbe

READINESS_TIMEOUT_SECONDS = 20.0


def main() -> None:
    runtime = streamlib.Runtime()
    microphone = runtime.add(streamlib.MicrophoneSource)
    speaker = runtime.add(streamlib.SpeakerSink)
    runtime.connect(microphone.output("audio"), speaker.input("audio"))

    probe = runtime.add(AudioBlockCountingProbe)
    runtime.connect(microphone.output("audio"), probe.input("audio_from_upstream"))

    def watch_readiness() -> None:
        try:
            runtime.wait_until_every_processor_is_running(
                timeout=READINESS_TIMEOUT_SECONDS
            )
            print("MARKER:EVERY_PROCESSOR_RUNNING", flush=True)
        except RuntimeError as refusal:
            print(f"MARKER:NOT_EVERY_PROCESSOR_RUNNING {refusal}", flush=True)
            # Shut down rather than leave `run()` holding the main thread: a
            # speaker that refused the microphone's format will never reach
            # Running, so waiting for it is waiting for nothing.
            runtime.shutdown()

    threading.Thread(target=watch_readiness, daemon=True).start()
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
