---
name: verify-live
description: The live end-to-end verification for changes that touch GPU / camera / codec. Use when a change needs a real pipeline run (a plain Bash call dies at exit 144 in a sandboxed firing), when the owner asks to verify a change on the rig, or when a PR claims E2E evidence that needs auditing. Primary SELF-RUN mode — the session runs the pipeline itself via the dangerouslyDisableSandbox bypass, reads an exposed encoded port's track at the local relay with tatolab-read-track as any user would, decodes it to PNGs, and audits them (log gates, PNG content, PSNR); falls back to the owner-terminal command-block handshake only when the rig is unavailable.
---

# verify-live — real-pipeline verification

Unit tests come first and catch most bugs. This skill is for the cases they can't reach: GPU/driver, V4L2, the codecs — where a run is the only proof. A plain `Bash` call cannot run the pipeline (exit 144 in a sandboxed firing; the `rig-brake` hook only notes a rig-consuming command, it never blocks one), but the Bash `dangerouslyDisableSandbox` bypass unlocks the rig — so when the rig is present the session runs the pipeline **itself** (SELF-RUN mode, primary) rather than handing it to the owner. Only when the session can't run it (rig unavailable, bypass denied) does it fall back to the **handshake**: emit the command for the owner's terminal, then audit what it produced. The `evidence-verifier` agent owns the audit and the fallback command block; this skill is the reference it and any reviewer share.

## Device indices are never hardcoded
Read `docs/rig-profile.local.md` for this machine's video-node / GPU topology, then confirm with a probe (`v4l2-ctl --list-devices`, `--get-fmt-video`). A runtime probe always beats the file. Every `/dev/videoN` in a command block is resolved this way — the indices below are placeholders. The profile is gitignored and per-machine; when it is absent, the probe is the whole answer.

## Scenario decision tree
1. **Can a unit test cover it?** (pure logic, parser, state machine) → write the unit test, done. No rig.
2. **Touches GPU memory / Vulkan / V4L2?** → unit tests miss driver-only failure modes; you need a real run. Pick below.
3. **Affects an encoder or decoder?** → **encoder/decoder round trip** — the colour rig (below), a sender stream encoding and a receiver stream decoding, run for both codecs.
4. **Only camera / GPU-compute / GPU-texture, no codec?** → **camera-or-kernel through the encoder** — the `tatolab new` scaffold, its source or effect swapped for the change's, its encoder's track read and decoded.
5. **Frame-ordering / timestamp / drop-sensitive?** → **v4l2loopback motion** (a `testsrc2` source with a visible per-frame counter, so a drop/repeat shows by eye) through the same scaffold, plus `timestamp_ns` in the dump's `objects.jsonl`.
6. **Color-path change?** → the **PSNR fixture rig** (below), with at least one negative-injection mode to prove the gate isn't vacuous.
7. **Audio — capture, playback, the device seam, an audio built-in?** → not this skill. The **audio loopback rigs** (below) measure a known signal rather than pixels, and `/verify-audio` drives them.
8. **An extension wheel that carries media over a network — WHIP/WHEP?** → the **networking arm** (below). Same decode-back lock as the codec rigs, with a real endpoint in the middle.

When unsure, default to the more demanding scenario (the round trip also exercises the source and the encoder). Current run commands live in the fixture scripts under the engine's `tests/fixtures/` — read them for the exact invocation (they drift; don't cache them here).

## Reading pixels — a track at the relay

**The runtime has no door for verification.** A port is read the way any user reads it: exposed, it is a track at the machine's local relay, and `tatolab-read-track` — a stock MoQ subscriber built from this checkout, never part of the runtime unit — writes what it reads to files (`read-live-track`). No window, no display server, no runtime-side image conversion.

**Only an encoded port carries pixels off the process.** A raw-frame port's record carries a surface id, which means nothing outside the runtime. So the pixels you read are an encoder's output: the scaffold's `h264encoder/encoded_video`, or — for a mid-graph port — an `H264Encoder` the stream chains off that port, its `encoded_video` exposed. Chaining a node is a topology a user could ship; the observation adds no door.

**Deriving the address.** From `tatolab graph --stream <stream id>`: the `machine`, the `stream`, the encoder node's `name` and its output port under `ports.outputs`. The broadcast is `<machine>/<stream id>.hang` and the track `<node>/<port>`. A node's name is already cast to lowercase URL-safe (a defaulted `H264Encoder` is `h264encoder`, `name="encoder"` is `encoder`), so copy it out of the graph rather than spelling it from the stream source. `exposed` in the same graph says whether the port is a track yet; when it is not, `tatolab expose <stream id> <node> <port>` makes it a private one.

### The spelling you will use

```bash
cargo build --release -p tatolab-read-track
target/release/tatolab-read-track <relay-url> <machine>/<stream id>.hang <node>/encoded_video --count N --out <dir>/track
python3 -c '
import json, pathlib, sys
dump = pathlib.Path(sys.argv[1])
with open(sys.argv[2], "wb") as elementary_stream:
    for line in (dump / "objects.jsonl").read_text().splitlines():
        elementary_stream.write((dump / json.loads(line)["file"]).read_bytes())
' <dir>/track <dir>/encoded.h264
mkdir -p <dir>/frames
ffmpeg -hide_banner -loglevel error -nostdin -f h264 -i <dir>/encoded.h264 -fps_mode passthrough -pix_fmt rgb24 \
  <dir>/frames/%04d.png 2> <dir>/ffmpeg.log
```

`<relay-url>` is the local relay's loopback listener: `http://127.0.0.1:4443` for the machine's runtime and every fixture's, the port the session's runtime was given otherwise (`drive-running-stream`). `-f hevc` and `encoded.hevc` for `H265Encoder`. This is the colour rig's own decode (`e2e_fixture_psnr_test_pattern.sh`) — read it when this drifts.

- The dump's `objects.jsonl` must name `"payload":"codec_payload_of_the_hang_legacy_container"`; `as_sent` means the catalog did not list the track as video within `--catalog-wait-at-most`, and the files are the container, not Annex-B.
- The dump opens on a group's first object, so the elementary stream starts on a keyframe; `--count` frames read decode to `--count` PNGs. Fewer is a failure, not a partial success — `ffmpeg.log` and read-track's stderr say where they went.
- `--count N` fails naming how many objects arrived when `N` do not within `--wait-at-most` seconds (default 30).
- The PNGs are the **encoder's** output decoded by the rig's ffmpeg, at full resolution, never composited. A change to the engine's decoder is proven by the receiver stream of the round trip (its decoded-frame count off `graph`) and by its unit tests, not by these pixels.

**By eye**: `http://127.0.0.1:4446/watch/<machine>/<stream id>[/<node>/<port>]` plays the stream's encoded video tracks in a browser through the same relay. It is a viewer, never evidence — a report's PNGs come from the dump.

### On macOS
The same reader, the same decode, the same audit and PSNR scoring. What doesn't carry over is the capture rig: there is no V4L2, vivid or v4l2loopback on macOS, the camera is AVFoundation, and the `/dev/videoN` probes and V4L2 fixtures are Linux-only. Drive a macOS run with the `tatolab new` scaffold, launched as below.

### Naming the camera
A stream opens the camera its `CameraSource` `device_id` config names (`stream_builder.add(CameraSource, config={"device_id": "/dev/videoN"})`), else the first one found. `STREAMLIB_CAMERA_DEVICE` is read only by the runtime-backed camera tests' stream (`tests/stream-on-runtime/camera_under_test.py`), never by the engine, and a stream reads the runtime's environment, never `tatolab run`'s — so set it on the `tatolabd` the stream loads into.

## PSNR rig
One fixture rig guards the color path; it has bug-injection modes that must deterministically FAIL to prove the gate is live:
- **`e2e_fixture_psnr_test_pattern.sh <out> h264|h265`** — the encode colour gate and the codec round trip, no camera: a sender stream (`TestPatternSource` → encoder, its port exposed) and a receiver stream (`RelayTrackSource` → decoder) on one runtime, the sender's track read with `tatolab-read-track`, decoded by the rig's ffmpeg, and scored with `cargo xtask psnr bar-colors` — each SMPTE bar's median colour against a per-codec baseline (`psnr_test_pattern_bar_colors_baseline_<codec>.tsv`) at ±0.05. The receiver's decode is proven by its decoder's frame count off `graph`. Negative `INJECT_BUG=bt601-bt709` / `swap-channels` / `swap-chroma` must FAIL naming bars. Range-swap is refused; the fixture's header says why. `BASELINE_CAPTURE=1` re-takes a codec's baseline. The engine decoder's pixels are not in this rig's path, so its H.265 conformance-window crop is gated by `cargo test -p streamlib-media-builtins --test h265_decoder_completes_the_round_trip` on the rig, whose extent assertion is the only thing that fails when the window stops reaching published frames.

**PSNR pass bar:** Y ≥ 35 dB good · 30–35 dB acceptable, flag it · < 30 dB regression (investigate color matrix / range / plane layout). Chroma has one floor and no acceptable band: either U or V under 30 dB fails the frame outright. For the video rig the bar lock and the injection modes live in `cargo xtask psnr` — pure GPU-free image math whose unit tests run in CI, so the scorer itself is gated even though the runs that feed it are not; ffmpeg decodes but does not score.

## Networking arm

One rig-only fixture, owned by the extension wheel it proves. It is the codec rigs' decode-back with a network hop inside it: the vivid camera and the microphone out through `H264Encoder` / `OpusEncoder` and back through the wheel's player into `H264Decoder` / `OpusDecoder`. Its reader and its whole-frame colour lock are deleted, so it cannot run until #2867 rebuilds it as a sender stream and a receiver stream on `TestPatternSource`, locked by `cargo xtask psnr bar-colors`. **The lock is that score, never a liveness check** — the network sits inside a path the codec rig already scored, so drift is the wheel's.

- **`packages/streamlib-webrtc/tests/live/whip_whep_roundtrip.sh [out]`** — WHIP publish to Cloudflare Stream and WHEP play-back of the same live input.

It takes `SAMPLE_COUNT`, `SAMPLE_EVERY`, `TOLERANCE`, `RUN_SECONDS` and `MEDIA_DEADLINE_SECONDS` from the environment; read the script header for the full list. `MEDIA_DEADLINE_SECONDS` is the one that matters when a run reports no frames — an ingest going live and a WHEP subscribe sit between the graph coming up and the first decoded frame, so the fixture waits for the first decoded frame before it samples.

**The wheel is measured through its own venv** (`packages/streamlib-webrtc/.venv`), which holds `tatolab-stream` and a current `maturin develop` build of the extension, and no engine — the stream runs on the runtime unit with `tatolab run`. The fixture refuses (exit 77) when the runtime unit is missing or that venv cannot import the wheel beside `tatolab-stream`; a stale `.so` that still imports is scored and reported as a pass for code that is not in the tree — so `maturin develop` before every run, and `cargo xtask build-runtime` after any engine edit, the same rule `/verify-audio` has.

### Credentials — and why absent ones are never a pass

Cloudflare Stream carries the stream key **in the URL path**, so each endpoint URL *is* the secret.

- `STREAMLIB_WHIP_URL` / `STREAMLIB_WHEP_URL` are read first and always win. When either is missing, the repo-root gitignored `.env` is sourced and its `CLOUDFLARE_WHIP_URL` / `CLOUDFLARE_WHEP_URL` fill them in — replacing any `CLOUDFLARE_*` already exported. Export the `STREAMLIB_` names to pin a run.
- **Absent credentials exit 77 — cannot-run, never a pass.** Report it as cannot-run in the template's Outcome line.
- **Never echo one.** The script redacts the endpoints in its own output; do the same in a report, a PR body, or a log excerpt you paste. `tatolab graph` renders every processor's config, so a WHIP or WHEP node's graph JSON contains the key — read it in a pipe, never save it into the evidence directory and never attach it.


## Audio loopback rigs
Two fixture rigs guard the audio path, measuring a known signal — tone frequency / amplitude / distortion, plus a DTMF symbol grid whose *spacing* is what a dropped block moves. Both gate on `virtual_audio_device.sh check` and **exit 77** (`SKIP: no virtual audio device available on this machine`) where no PipeWire session is reachable; 77 is cannot-run and is never reported as a pass:
- **`e2e_audio_loopback.sh <out>`** — `pw-play` into a null sink, `pw-record` off its monitor, no StreamLib in the path, so it answers "is the rig sound" when the engine won't build. Negative modes: `INJECT_BUG=silence` (30 ms of the tone body zeroed), `drop` (30 ms excised, shifting everything after), `gain` (0.6× amplitude).
- **`verify_audio_loopback.sh [--count N]`** — the same loop with `SpeakerSink` playing and `MicrophoneSource` capturing, so both ends are StreamLib; it runs the block-level channel contract on the microphone's port first. No injection mode — corrupting what the *engine* carries is a different fixture's job.

Drive these through **`/verify-audio`**, which owns the workflow: it picks the mode, re-runs the rig-only fixture when the through-engine one fails (so "the engine broke it" is never read as "the rig is broken"), and reports the numbers with the spectrogram.

## Modes
- **SELF-RUN (primary — rig available).** Run the pipeline yourself, no owner in the path. Probe the rig first (device nodes, `/dev/dri/*`) and only self-run when it is present.
  1. **Launch** under the Bash `dangerouslyDisableSandbox` bypass — the sandbox blocks the rig, and the bypass is what unlocks GPU/V4L2. A stream loads into a runtime the session starts for itself, never the machine's (`drive-running-stream` § A runtime of the session's own): `cargo xtask build-runtime --machine-directories-under-a-test-root` (add `--release` for a release engine) lays out `target/tatolab-runtime/bin/tatolab` and `tatolabd`, the lend and the preview page, so rebuild it after any engine edit — the run measures whatever unit is on disk, not the tree. Start its `tatolabd` under a fresh `TATOLAB_TEST_MACHINE_ROOT=$(mktemp -d /tmp/tl-XXXXXX)`, backgrounded, with `TATOLAB_TEST_LOCAL_RELAY_LOOPBACK_PORT`, `TATOLAB_TEST_LOCAL_RELAY_EVERY_INTERFACE_PORT` and `TATOLAB_TEST_LOOPBACK_HTTP_PORT` set to free ports — a test build runs no local relay and serves no listing without them, and never binds the machine's — its output (the engine's log mirror, every stream's records among it) redirected to a log file you will grep, and every stream setting on it: a stream compiles and its processor interpreters start with the runtime's environment, never `tatolab run`'s. Poll `tatolab streams` until it answers. Then load: `tatolab run --dir <dir holding its stream.py>` — it records the stream in the root's state directory, prints `<stream id> running as "<display name>" (…)` and returns — and take that stream id (or the one `tatolab streams` lists) for every later verb. The runtime compiles the stream in the project's own `.venv`, which holds `tatolab-stream` and the nodes' dependencies and never the runtime — each processor interpreter borrows `tatolab.runtime` from the lend. Launch the change's own `stream.py`, or a `tatolab new <dir>` scaffold (`TestPatternSource` → a GPU effect → `H264Encoder`, its `encoded_video` exposed public, with a meter on a fan-out), its venv made from this checkout — `uv venv --python 3.12 <dir>/.venv && VIRTUAL_ENV=<dir>/.venv uv pip install -e sdk/tatolab-stream 'numpy>=2.1'` — because the change under test is the checkout's `tatolab-stream`, never the published one `uv sync` resolves. Swap `TestPatternSource` for `CameraSource` with `config={"device_id": "/dev/videoN"}` to put the vivid virtual camera (resolve N by probe) or a real one through it, or the effect for the kernel under test. Every `tatolab` verb is the unit's own `target/tatolab-runtime/bin/tatolab` — an installed `tatolab` on PATH is a release build and reaches the machine's runtime instead — and carries the same `TATOLAB_TEST_MACHINE_ROOT`, since shell state does not persist between Bash calls. The fixture scripts are the exception: they start their own `tatolabd` from the plain unit, on the machine's lock and the machine's relay ports, and refuse the test-only one by name — end the session's runtime and rebuild with plain `cargo xtask build-runtime` before running one. They refuse while the machine's runtime answers too, and that runtime is the owner's: ask before stopping it, and start it again when the fixture ends (`stop-running-stream` gives each platform's way). The Rust example crates under `examples/` are not workspace members, so they build and run from their own directory; the fixture scripts carry the current invocation.
  2. **Confirm it is live**: poll `tatolab graph --stream <stream id>` until every node's `components.state` is `Running` — a stream answers from its load, while its nodes are still `Pending`. Read the address from it as above and confirm the encoder's port is under `exposed` (`tatolab expose` it private when not). These are local API reads and writes — `rig-brake` lets them through, and they need no bypass.
  3. **Read pixels**: `tatolab-read-track` the encoded track at the session runtime's relay port, concatenate, decode with ffmpeg, as above. For a mid-graph port, chain an encoder off it in the stream and read that one.
  4. **Audit**: Read each decoded PNG and describe it / compute PSNR, per the checklist below. Attach the PNG(s) to R2 and embed them in the PR (see the `attach-artifact` skill).
  5. **Stop the stream, then the runtime**: `tatolab rm --dir <project>` queues the unload and forgetting of every stream of the project, and once it has run `tatolab streams` no longer lists them. Then SIGINT the session's `tatolabd` and require a clean exit — `0` inside ~15 s; `124` (its watchdog abandoned a stream's teardown) or needing SIGKILL is a finding, not a flake (`stop-running-stream`). Delete the test machine root.

  Read-only observation evals auto-run, but a real-world SAFETY gate (actuators, motors, drone control) still asks the owner first.
- **Handshake fallback (rig unavailable / bypass denied).** Print the command block for the owner's terminal, then audit what it produced. Two sub-modes:
  - *Interactive* — print the command block now; the owner runs it; you audit the output directory in the same session.
  - *Async* — the owner comments "done, output in `<dir>`" on the issue; spawn `evidence-verifier` to audit `<dir>` when they report back.

## Auditing the output (all modes)
1. **Log gates — all zero.** Grep the pipeline log for `OUT_OF_DEVICE_MEMORY`, `DEVICE_LOST`, `process() failed`, `Validation Error`. Any nonzero fails (a `Validation Error` is acceptable only if it also exists on `main` for the same scenario — say so if you claim it).
2. **Progress markers** — first-frame-encoded/-decoded/-captured and ≥1 progress line fired.
3. **Read every decoded PNG with the Read tool and describe what it shows.** "Looks fine" is banned. A black/uniform frame with clean logs **IS a regression**.
4. **PSNR vs the pass bar** when a reference exists; for a real camera write `n/a — real-camera source` and treat the visual description as the sole gate.
5. **Fill the report template below, verbatim.**

## Standardized E2E report template (the single greppable source — fill verbatim)

````markdown
### E2E Test Report

- **Scenario**: encoder/decoder | camera-or-kernel through the encoder | networking (whip-whep)
- **Stream / fixture**: `tatolab new` scaffold (<what was swapped in>) | the change's `stream.py` | `e2e_fixture_psnr_test_pattern.sh` | `whip_whep_roundtrip.sh`
- **Codec**: h264 | h265 | n/a
- **Camera device**: `/dev/videoN` (vivid | Cam Link 4K | other)
- **Resolution**: 1920x1080 | 1280x720 | other
- **Run length**: <seconds the stream ran before it was stopped>
- **Build profile**: the runtime unit's — debug | release (`cargo xtask build-runtime [--release]`)
- **Command**:
    ```
    <exact launch command with env vars, the tatolab-read-track command, and the ffmpeg decode>
    ```

#### Log signals

- `OUT_OF_DEVICE_MEMORY`: <count> (0 = pass)
- `DEVICE_LOST`: <count> (0 = pass)
- `process() failed`: <count> (0 = pass)
- `Validation Error` (with `VK_LOADER_LAYERS_ENABLE=*validation*`): <count or "not enabled">
- `First frame encoded` / `First frame decoded` / `First frame captured`: <timestamps or "not seen">
- `Encode progress` / `Decode progress` high-water mark: <frames>

#### Frames read

- Track: `<node>/<port>` of `<machine>/<stream id>.hang` — <the stream's own encoder, or one chained off `<node>/<port>` and why>
- Exposure: already `<private|public>` | exposed private for the read, and removed after
- Read: `--count <N>`; `payload` in `objects.jsonl`: `codec_payload_of_the_hang_legacy_container`
- Decoded: <PNGs ffmpeg wrote> of <N> objects read
- PNGs read with Read tool: `<path1>`, `<path2>`, …
- **What was in the image(s)**: <one or two sentences per PNG read — what you
  actually saw. E.g., "frame 60: dark room with chair back and wood door,
  matches the Cam Link scene" or "frame 30: vivid green/purple SMPTE bars
  with `00:00:06:603` timecode overlay, matches expected test pattern". A
  response of "looks fine" is NOT acceptable — describe the content so a
  reviewer can tell at a glance whether you actually looked.>
- Groups joined mid-way and skipped / short reads: <from tatolab-read-track's stderr, or "none">
- Anomalies (black frames, tearing, wrong colors, off-center, etc.): <list or "none">

#### PSNR

- Reference frame: `<path>` or `n/a — <reason>`
- Y / U / V PSNR (dB): <y>, <u>, <v> — or `n/a — <reason>`
- Command used:
    ```
    <ffmpeg or equivalent command>
    ```

#### Outcome

- **Pass** / **Pass with caveats** / **Fail**
- Caveats / follow-ups filed: <list of issue numbers, or "none">
````

Paste the filled template (one per scenario) into the PR description or the issue comment requesting review — verbatim structure so it's grep-able across PRs.
