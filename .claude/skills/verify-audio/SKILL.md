---
name: verify-audio
description: The live verification for changes that touch audio — capture, playback, the audio device seam, an audio built-in, or the block's timestamps. Runs the known-signal loopback and reports the verdict, the measured numbers and the spectrogram. Use when an audio change needs a real run, when the owner asks to verify audio on the rig, or when a PR claims audio evidence that needs auditing. It distinguishes a broken engine from a broken rig automatically, and reports an environment that cannot run the fixture as cannot-run rather than as a pass.
---

# verify-audio — known-signal loopback verification

Sister to `verify-live`, which guards the GPU / camera / display / codec path. This one guards
audio: a signal whose every property is known is played, captured back, and measured — the tone's
frequency, amplitude and distortion, and a DTMF symbol grid whose *spacing* is what catches a
dropped block. Unit tests come first and catch most audio bugs; this is for what they can't reach,
which is whether the samples that left actually arrived.

**The skill owns the workflow; the fixture owns the measurement.** Nothing here re-implements the
signal generation or the analysis, and nothing here second-guesses a verdict. If this document and
the fixture ever disagree about what a passing run means, **the fixture is right** — read it and
fix this.

## The two fixtures, and which question each answers

Both live in `runtime/streamlib-engine/tests/fixtures/` and both measure with the same analyser,
so their reports are directly comparable.

- **`e2e_audio_loopback.sh [out_dir]` — rig-only.** `pw-play` into a null sink, `pw-record` off
  its monitor, no StreamLib anywhere in the path. It answers "is this machine's audio sound",
  which is the question a tool living inside the runtime can never answer — it runs whether or not
  the engine compiles.
- **`verify_audio_loopback.sh [--count N] [--port PORT]` — through-engine.** `SpeakerSink` plays
  the signal into the same null sink and `MicrophoneSource` captures it back off that sink's
  monitor, so both ends are StreamLib. It also runs the block-level channel contract on the
  microphone's own port on the way through (cadence, timestamp continuity, a frame the engine did
  not re-stamp), and fails on that before it ever measures the signal.

**Through-engine is the default.** It is the question a PR usually has. Run rig-only alone only
when the engine will not build, or when you are checking the machine rather than the change.

## Preflight — cannot-run is an answer, never a pass

Both fixtures gate on `virtual_audio_device.sh check` and **exit 77** when no PipeWire session is
reachable, printing `SKIP: no virtual audio device available on this machine`. A container with no
session audio daemon hits this, and so does a machine where `pw-cli` / `pw-play` / `pw-record` are
not installed.

**77 is reported as cannot-run and never as a pass.** There is no verdict to report: nothing was
measured. Say the environment cannot run the fixture, say which of the two reasons it gave, and
stop — do not fall back to a unit test and call the area verified.

To check before committing to a run:

```bash
runtime/streamlib-engine/tests/fixtures/virtual_audio_device.sh check
```

## Running it

The rig is real, so these need the Bash `dangerouslyDisableSandbox` bypass. `rig-brake` prompts on
any `tests/fixtures/e2e_*.sh` path, which covers the rig-only fixture; the through-engine one does
not match that pattern but still brings up a GPU context and an iceoryx2 node, so give it the
bypass too.

```bash
# through-engine (default)
PYTHON="$PWD/sdk/streamlib-python-wheel/.venv/bin/python" \
  runtime/streamlib-engine/tests/fixtures/verify_audio_loopback.sh

# rig-only
PYTHON="$PWD/sdk/streamlib-python-wheel/.venv/bin/python" \
  runtime/streamlib-engine/tests/fixtures/e2e_audio_loopback.sh /tmp/verify-audio-rig
```

**`PYTHON` must be absolute.** The through-engine fixture starts the node from a subshell that
`cd`s into the fixtures directory first, so a repo-relative interpreter path resolves against the
wrong directory and dies there. It defaults to `python3`, which only works if that interpreter can
already `import streamlib` — the wheel venv is the reliable answer.

**The through-engine fixture hosts its control plane on port 9077, and a busy 9077 does not fail
the run — it misdirects it.** The API server walks up to ten ports looking for a free one and says
so only at `INFO` (`Port 9077 in use, bound to 9078 instead`), while the fixture keeps asking the
port it was given. What you see is `no control plane reachable at http://127.0.0.1:9077
(Connection refused)` and a channel contract that failed, from a graph that is running perfectly.
Free the port or pass `--port`; `node.log` in the artifacts directory is where the run says which
one it took.

## Reading what came back

**Exit status is the verdict** and needs no parsing: `0` pass, `1` fail, `77` cannot-run, `2` bad
arguments, `130` interrupted.

**stdout is the report JSON and nothing else**, so it pipes. Progress goes to stderr — and on a
through-engine run the *channel* report JSON goes to stderr as well, because that verdict is an
intermediate one. Don't mistake it for the signal report on stdout.

**Artifacts land in a `mktemp` directory named on the last `artifacts: <dir>` line of stderr.** A
through-engine run prints two: the first is the nested channel tap's directory, the last is the
loopback's own. Take the last one — that is where `captured.wav`, `spectrogram.png` and `node.log`
are. Only the rig-only fixture takes an output directory as an argument and tees a `report.json`
into it; the through-engine one keeps its report on stdout alone, so capture it yourself.

The fields that matter when reading a failure — the analyser has already judged them, so these are
for saying *what* broke, not *whether*:

- `verdict` and `failed` — the named checks that tripped. Report the list verbatim.
- `fundamental_hz`, `amplitude`, `thd_percent` — are these samples audio at all.
- `symbols` vs `symbols_expected` — the signal's identity survived.
- `symbol_interval_error_ms`, `cumulative_interval_error_ms` — spacing, which is what a dropped
  block moves. Per-span and total, because loss spread thinly passes the first and not the second.
- `missing_loud_audio_ms`, `silent_stretch_ms`, `emptiest_region` — a hole, how big, and where.

The thresholds live in `known_audio_signal.py` beside the reasoning for each. Don't cache them
here; they drift and the analyser is the one applying them.

**Read the spectrogram with the Read tool and describe it.** A verdict alone is not reviewable.
A healthy run reads as a solid horizontal bar (the 440 Hz tone) followed by six evenly spaced
symbol stacks; a dropped block shows as a vertical broadband stripe cutting through the tone,
which is the splice. Describing it is what tells a reviewer you looked — "looks fine" is banned
here for the same reason it is in `verify-live`.

## A failing through-engine run gets the rig fixture before it gets reported

"The rig is broken" and "the rig is fine, the engine broke it" are different answers with
different owners, and the user should not have to work out which they got.

So on any **non-zero, non-77** exit from `verify_audio_loopback.sh`, run `e2e_audio_loopback.sh`
before reporting anything, and report the pair:

- rig-only **passes** → the rig is sound, so the failure is on the StreamLib side of the loop.
  Read `node.log` before calling it a regression: a port collision presents as exactly this, and
  so does anything else that stopped the fixture reaching a graph that ran fine.
- rig-only **fails too** → the machine's audio path is broken. Nothing has been proven about the
  engine either way, so say that rather than blaming the change.
- rig-only **skips (77)** → the through-engine failure is uninterpretable; report cannot-run.

## Proving the gate is live

A gate only ever observed green is a gate nobody has evidence is still wired up. The rig-only
fixture carries three injected faults, each of which must FAIL deterministically:

```bash
INJECT_BUG=silence  # 30 ms of the tone body replaced with silence
INJECT_BUG=drop     # 30 ms excised, so everything after it shifts earlier
INJECT_BUG=gain     # the whole signal at 0.6× amplitude
```

**The through-engine fixture has no injection mode**, so that half of the gate has only been seen
red by accident — an earlier revision of the fixture failing it during development. Injecting into
it means corrupting what the engine carries rather than what is played, which is fixture work and
not this skill's.

## Say which mode ran

The report names the mode. A rig-only pass is not evidence about the engine, and reporting one
without the label invites it to be read as though it were.

## Report template — fill verbatim

````markdown
### Audio Verification Report

- **Mode**: through-engine | rig-only | through-engine + rig-only (failure triage)
- **Fixture**: `verify_audio_loopback.sh` | `e2e_audio_loopback.sh`
- **Injected fault**: none | `silence` | `drop` | `gain`
- **Command**:
    ```
    <exact command with env vars>
    ```
- **Exit status**: <0 | 1 | 77>

#### Measured

- `verdict`: PASS | FAIL
- `failed`: <the named checks, or "none">
- Tone: `fundamental_hz` <n> · `amplitude` <n> · `thd_percent` <n>
- Symbols: `<recovered>` of `<expected>`
- Spacing: `symbol_interval_error_ms` <n> (worst `<span>`) · `cumulative_interval_error_ms` <n>
- Holes: `missing_loud_audio_ms` <n> · `silent_stretch_ms` <n> · `emptiest_region` <region>
- Channel contract (through-engine only): <verdict + `bags_dropped_by_the_tap`, or "n/a">

#### Spectrogram

- Path: `<artifacts dir>/spectrogram.png`
- **What it shows**: <one or two sentences describing what you actually saw — the tone bar, the
  six symbol stacks, and any vertical stripe or gap. "Looks fine" is not acceptable.>

#### Triage (only when the through-engine run failed)

- Rig-only re-run: PASS | FAIL | SKIP
- **Therefore**: the failure is on the StreamLib side | the machine's audio is broken |
  uninterpretable
- What `node.log` said: <the run's own explanation, or "nothing that explains it">


#### Outcome

- **Pass** / **Fail** / **Cannot run here** — <and for cannot-run, which reason the fixture gave>
````

Paste the filled template into the PR description or the issue comment requesting review, and
attach the spectrogram (see the `attach-artifact` skill) — the image is what makes a failure
legible to a reader who was not there.
