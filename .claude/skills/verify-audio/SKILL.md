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
  not re-stamp), and fails on that before it ever measures the signal. `--count` is how many bags
  that intermediate tap collects, not anything about the signal; `--port` moves the control plane.

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

The rig is real, so these need the Bash `dangerouslyDisableSandbox` bypass. `rig-brake` never
prompts here: it can note an executed `tests/fixtures/e2e_*.sh` script, and the project baseline
turns even that note off. Both fixtures drive real audio devices, so give both the bypass; the
through-engine one also brings up a GPU context and an iceoryx2 node.

```bash
# through-engine (default)
PYTHON="$PWD/sdk/streamlib-python-wheel/.venv/bin/python" \
  runtime/streamlib-engine/tests/fixtures/verify_audio_loopback.sh

# rig-only — a fresh directory, because the fixture never clears the one you give it
PYTHON="$PWD/sdk/streamlib-python-wheel/.venv/bin/python" \
  runtime/streamlib-engine/tests/fixtures/e2e_audio_loopback.sh \
  "$(mktemp -d -t verify-audio-rig-XXXXXX)"
```

**`PYTHON` must be absolute.** The through-engine fixture starts the node from a subshell that
`cd`s into the fixtures directory first, so a repo-relative interpreter path resolves against the
wrong directory and dies there. It defaults to `python3`, which only works if that interpreter can
already `import streamlib` — the wheel venv is the reliable answer.

**The through-engine fixture hosts its control plane on port 9077, and a busy 9077 misdirects the
run rather than failing it.** The API server walks up to ten ports looking for a free one and says
so only at `INFO` (`Port 9077 in use, bound to 9078 instead`), while the fixture keeps asking the
port it was given. So the node is alive and serving — somewhere else.

**Budget for it: a busy port stalls the run for up to half an hour before it gives up.** The
startup poll is 60 attempts at a call bounded by `CONTROL_VERB_TIMEOUT_SECONDS = 30.0`, so a port
held by something that accepts and never answers burns the full 30 s per attempt.

**So check the port is free before you start**, or pass `--port`. This is the one preflight that
prevents an unearned green, and it costs nothing:

```bash
ss -ltn | grep ':9077' || echo "9077 free"
```

What a busy port looks like depends on what holds it, and none of these is `Connection refused`:

- **A second StreamLib node that declares a `MicrophoneSource`** → **no error at all.** The tap
  resolves against *that* node's graph, so the channel contract is measured on a different
  process and the run can exit 0 and report `PASS`. The realistic instance is an orphaned
  `audio_loopback_node.py` from a killed run. Its own default is 9000 — 9077 is this fixture's
  (`verify_audio_loopback.sh`), which exports it into the node — so an orphan of a *previous run
  of this same fixture* sits on exactly the port the next one wants, declaring exactly the
  processor it looks for. **This is the dangerous one**: every other failure mode here announces
  itself.
- **A second StreamLib node without one** → the query succeeds against the wrong graph and the run
  dies at `no processor named MicrophoneSource in the running graph`.
- **A socket that accepts and never answers** → `no control plane reachable at
  http://127.0.0.1:9077 (timed out)`.
- **A foreign HTTP server** → an HTTP status rather than a refusal, e.g. `answered 404`.

**Confirm the run measured its own node before believing a pass.** The tap prints the channel it
read (`tapping <processor-id>/audio for N bags`); that id must belong to the node whose `node.log`
you have, or the verdict is about someone else's graph:

```bash
grep -i "<tapped-processor-id>" "<artifacts-dir>/node.log"
```

`-i` is not optional. The tap lowercases the id and `node.log` does not, so the exact-case
spelling returns zero hits on a perfectly healthy run — a false collision alarm every time.

**`Connection refused` on 9077 means the opposite — nothing is listening there at all**, so the
node died or never served. That is a real failure and must never be waved away as a port
collision. `node.log` is where the run says which port it took, and whether it got that far.

## Reading what came back

**Exit status is the verdict** and needs no parsing: `0` pass, `1` fail, `77` cannot-run, `130`
interrupted. The through-engine fixture adds `2` for a bad argument; the rig-only one has no
argument parser, so it takes its output directory positionally and cannot report that.

**stdout is the report JSON and nothing else**, so it pipes. Progress goes to stderr — and on a
through-engine run the *channel* report JSON goes to stderr as well, because that verdict is an
intermediate one. Don't mistake it for the signal report on stdout.

**Artifacts land in a `mktemp` directory named on the last `artifacts: <dir>` line of stderr.** A
through-engine run prints two: the first is the nested channel tap's directory, the last is the
loopback's own. Take the last one — that is where `captured.wav`, `spectrogram.png` and `node.log`
are.

**On any non-zero exit, recover the directory rather than trusting the last `artifacts:` line.**
An analyser `FAIL` prints that line exactly as a pass does, so far so good. But a *channel*
contract that fails its analysis prints exactly one `artifacts:` line — the nested tap's — and
"take the last one" then lands you in a `streamlib-audio-channel-` directory with no `node.log`,
no `captured.wav` and no spectrogram. Anything that fails earlier still (channel resolution, the
tap itself, the speaker-format refusal, a node that never served) prints none at all. Those are
exactly the runs whose `node.log` you need:

```bash
dirname "$(/bin/ls -t /tmp/streamlib-audio-loopback-*/node.log | head -1)"
```

Two traps are why it is spelled that way. `/bin/ls` because `ls` is commonly aliased (to `eza`
here), and `eza` reads `-t` as `--time=FIELD` — it swallows the first match as that flag's value
and lists the rest alphabetically, exit 0, no error, an answer that can be hours stale. And it
selects on `node.log` because the rig-only fixture defaults to the *same*
`streamlib-audio-loopback-` prefix, so any rig-only run left to its default leaves a directory
carrying a `report.json` and a spectrogram but no `node.log` — a convincing decoy. That is why
the triage step below is given an explicit directory.

The rig-only fixture is the only one that takes an output directory as an argument, and it tees a
`report.json` into it; the nested channel tap writes one into its own directory too. The
through-engine fixture's own signal report is the one that exists on stdout alone — capture it
yourself.

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

The image says *that* audio went missing, not *which way*: a hole and a splice both draw the same
stripe. The `failed` list is what separates them — a splice moves the symbol intervals, a hole
does not. And an amplitude fault draws nothing at all: the spectrogram of a signal at 0.6× looks
exactly like a healthy one, so a run can fail with a picture that shows no defect. Report both,
never the picture alone.

## A failing through-engine run gets the rig fixture before it gets reported

"The rig is broken" and "the rig is fine, the engine broke it" are different answers with
different owners, and the user should not have to work out which they got.

So on **exit 1** — the fixture's failure status — run the rig-only fixture before reporting
anything, and report the pair. Exit `2` and `130` are not failures of the thing under test: a bad
argument and a cancelled run say nothing about the rig or the engine, so they earn no triage and
no report.

Give it a **fresh** directory each time, and one outside the `streamlib-audio-loopback-*` glob so
it cannot become the decoy described above. Fresh matters as much as the name: the fixture only
`mkdir -p`s the directory you hand it and never clears it, so a triage run that skips at the
device check leaves the *previous* run's `report.json` and spectrogram sitting there — saying
`PASS`.

```bash
TRIAGE_DIR="$(mktemp -d -t verify-audio-triage-XXXXXX)"
PYTHON="$PWD/sdk/streamlib-python-wheel/.venv/bin/python" \
  runtime/streamlib-engine/tests/fixtures/e2e_audio_loopback.sh "$TRIAGE_DIR"
```

- rig-only **passes** → the rig is sound, so the failure is on the StreamLib side of the loop.
  Read `node.log` before calling it a regression — a run that never reached the graph it meant to
  measure fails here too, and the port section above is how to tell the two apart.
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
- **Exit status**: <0 | 1 | 77>  — 2 and 130 are not verification outcomes; do not file a report

#### Measured

- `verdict`: PASS | FAIL | n/a — <no analysis ran: cannot-run, or a failure before the analyser>
- `failed`: <the named checks, "none", or "n/a — no analysis ran">
- Tone: `fundamental_hz` <n> · `amplitude` <n> · `thd_percent` <n> — or `n/a`
- Symbols: `<recovered>` of `<expected>`
- Spacing: `symbol_interval_error_ms` <n> (worst `<span>`) · `cumulative_interval_error_ms` <n>
- Holes: `missing_loud_audio_ms` <n> · `silent_stretch_ms` <n> · `emptiest_region` <region>
- Channel contract (through-engine only): <verdict + `bags_dropped_by_the_tap`, or "n/a">
- Tapped channel belonged to this run's own node: yes — `<tapped id>` vs `node.log` | n/a

#### Spectrogram

- Path: `<artifacts dir>/spectrogram.png` — or `n/a — the run failed before the analyser`
- **What it shows**: <one or two sentences describing what you actually saw — the tone bar, the
  six symbol stacks, and any vertical stripe or gap. "Looks fine" is not acceptable.>

#### Triage (only when the through-engine run failed)

- Rig-only re-run: PASS | FAIL | SKIP — exit <n>, artifacts `<dir>`
- **Therefore**: the failure is on the StreamLib side | the machine's audio is broken |
  uninterpretable
- What `node.log` said: <the run's own explanation, or "nothing that explains it">

#### Outcome

- **Pass** / **Fail** / **Cannot run here** — <and for cannot-run, which reason the fixture gave>
````

Paste the filled template into the PR description or the issue comment requesting review, and
attach the spectrogram (see the `attach-artifact` skill) — the image is what makes a failure
legible to a reader who was not there.
