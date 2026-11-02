# tatolab-microphone-proof

Throwaway proof for the media-modules change (tracer bullet 1): a microphone
captured inside a Python node's own helper process, built the way a third party
would build it, measured against the built-in `MicrophoneSource`.

- `MicrophoneProofSource` opens an ALSA PCM (at runtime, through
  `libasound.so.2`) and reads whole periods on its own native thread into a
  lock-free ring. The node's one Python drain thread waits on the ring with the
  GIL released and writes each block's ready-made audio-block message, so
  Python runs once per block, never per sample.
- `AudioBlockArrivalRecorder` writes one CSV row per block it receives. The A/B
  harness puts it behind each source in turn.

Linux only. The real audio module replaces this once the engine exports its
audio device interface through the host table.

## The A/B run

```sh
uv venv .venv
uv pip install --python .venv/bin/python -e ../../sdk/tatolab-stream
VIRTUAL_ENV=$PWD/.venv maturin develop --release
cargo xtask build-runtime   # from the repository root
tests/ab/microphone_ab.sh [output_dir]
```

`RUN_SECONDS` (default 600) is each arm's length. `CPU_STRESS_PROCESSES`
(default: one per core) busy processes run beside both arms, and
`PROOF_GIL_CHURN_THREADS` (default 0) pure-Python threads contend for the proof
node's own GIL. `PROOF_PCM_NAME` (default `default`) is the ALSA PCM the proof
opens; the built-in opens the audio backend's default source, so both capture
the same device. The verdict is `tests/ab/audio_block_arrival_report.py`'s.
