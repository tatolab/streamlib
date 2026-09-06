---
name: verify-live
description: The live end-to-end verification for changes that touch GPU / camera / display / codec. Use when a change needs a real pipeline run (a plain Bash call dies at exit 144 in a sandboxed firing), when the owner asks to verify a change on the rig, or when a PR claims E2E evidence that needs auditing. Primary SELF-RUN mode — the session runs the pipeline itself via the dangerouslyDisableSandbox bypass, exchanges tapped surface ids for the frames' pixels, and audits them (log gates, PNG content, PSNR); falls back to the owner-terminal command-block handshake only when the rig is unavailable.
---

# verify-live — real-pipeline verification

Unit tests come first and catch most bugs. This skill is for the cases they can't reach: GPU/driver, V4L2, swapchain — where a run is the only proof. A plain `Bash` call cannot run the pipeline (exit 144 in a sandboxed firing; the `rig-brake` hook only notes a rig-consuming command, it never blocks one), but the Bash `dangerouslyDisableSandbox` bypass unlocks the rig — so when the rig is present the session runs the pipeline **itself** (SELF-RUN mode, primary) rather than handing it to the owner. Only when the session can't run it (rig unavailable, bypass denied) does it fall back to the **handshake**: emit the command for the owner's terminal, then audit what it produced. The `evidence-verifier` agent owns the audit and the fallback command block; this skill is the reference it and any reviewer share.

## Device indices are never hardcoded
Read `docs/rig-profile.local.md` for this machine's video-node / GPU topology, then confirm with a probe (`v4l2-ctl --list-devices`, `--get-fmt-video`). A runtime probe always beats the file. Every `/dev/videoN` in a command block is resolved this way — the indices below are placeholders. The profile is gitignored and per-machine; when it is absent, the probe is the whole answer.

## Scenario decision tree
1. **Can a unit test cover it?** (pure logic, parser, state machine) → write the unit test, done. No rig.
2. **Touches GPU memory / Vulkan / V4L2 / swapchain?** → unit tests miss driver-only failure modes; you need a real run. Pick below.
3. **Affects an encoder or decoder?** → **encoder/decoder roundtrip** (`camera → encoder → decoder → display`), run both codecs.
4. **Only camera / display / GPU-compute / GPU-texture, no codec?** → **camera-display-only** (faster, isolates the path).
5. **Frame-ordering / timestamp / drop-sensitive?** → **v4l2loopback motion** (a `testsrc2` source with a visible per-frame counter, so a drop/repeat shows by eye).
6. **Color-path change?** → the **PSNR fixture rigs** (below), with at least one negative-injection mode to prove the gate isn't vacuous.
7. **Audio — capture, playback, the device seam, an audio built-in?** → not this skill. The **audio loopback rigs** (below) measure a known signal rather than pixels, and `/verify-audio` drives them.
8. **An extension wheel that carries media over a network — WHIP/WHEP, MoQ?** → the **networking arm** (below). Same decode-back lock as the codec rigs, with a real endpoint in the middle.

When unsure, default to the more demanding scenario (encode/decode also exercises camera + display). Current run commands live in the fixture scripts under the engine's `tests/fixtures/` — read them for the exact invocation (they drift; don't cache them here).

## Reading pixels — the exchange door

**You do not need a window to see a frame.** The control plane serves `exchange`: a published surface id in, that frame's pixels out, out of process — no window in the graph, no display server in the path. This is how verification sees pixels.

It matters because window capture could only ever read a channel that *terminates in a window*, so observing a mid-graph channel meant adding a `DisplayWindow` to look at it — and you were then testing a topology you don't ship. Exchange reads any channel and leaves the graph alone. It is also exact: a window grab returns whatever the compositor composited (scaled, letterboxed), which makes PSNR against it meaningless.

**Ids come from bags, and the engine never reads one for you.** Tap is untouched — it forwards bags verbatim. The consumer decodes the bag, finds the surface id, and exchanges it. `streamlib tap <channel>` shows what a bag actually carries.

**Deriving the channel name is the one fiddly step.** A channel is `{source_processor_id}/{output_port}`, but the processor-id chunk is **lowercased** — `streamlib graph` prints `Po8z66n…` and the channel is `po8z66n…/video`. Copying the id out of the graph verbatim gets you `no tappable channel named …`. The port name is author-supplied and is *not* normalized, so it rides through exactly as declared. Lowercasing is the whole recipe for a default `P{cuid2}` id; a name long enough to overflow the wire bound hash-legalizes the processor chunk instead, so derive it rather than assume (`runtime/streamlib-engine/src/iceoryx2/channel_name.rs`).

### The spelling you will use — CLI, channel form

```bash
streamlib exchange --channel <processor>/<output_port> --out <dir> --count N [--every N] [--field NAME]
```

One warm process: it taps a bounded round of bags, reads a surface id out of each sampled bag, and exchanges them — per tap round, not per bag as it lands. Writes **exact full-resolution** PNGs into `--out` and prints their paths on stdout, one per line. **Read those printed paths** — `--out` is not cleared, so listing the directory can hand you an older run's frames.

- `--count N` — frames to exchange before returning (default 1). A short sample **exits 1**: fewer frames than you asked for is a failure, not a partial success.
- `--every N` — exchange every Nth sampled bag, for temporal spread.
- `--field NAME` — the bag field carrying the id (default `surface_id`). If the run reports bags carrying no id in the named field, name the right one from `streamlib tap`.
- `--url` / `--node` pin the target node; with neither, the sole live node is resolved.

**Two shipped ceilings, so you can tell a bound from a regression.** The run gives up after **8 tap rounds** and returns short (exit 1) — on a fast-recycling channel that is the bound, not a broken channel. And `tap` previews only the first **4096 bytes** of a bag: a *selected* bag longer than that fails the channel form by name, and the single-id form is the way through (tap the channel, pull the id yourself, exchange it).

The other spellings, when they fit:
- **One id you already have**: `streamlib exchange <SURFACE_ID> --out <dir>`.
- **In-session view (MCP host wired to the node)**: the `exchange` tool returns an image content block, downscaled to a declared long-edge cap, stating the surface's true extent and the REST route for exact bytes.
- **Exact bytes over HTTP**: `GET /api/surfaces/{surface_id}/image` → binary `image/png`, full resolution. The evidence and PSNR path. **Percent-encode the id**: its `#<generation>` suffix is a URL fragment otherwise, so `curl …/api/surfaces/abc#105/image` asks for `/api/surfaces/abc` and the generation never reaches the server — send `abc%23105`. On a token-enabled node this route is bearer-gated beside the tap WebSocket.

### Staleness is a retry, never wrong pixels
A surface id is per-frame (`<slot>#<generation>`). Resolving a retired one is refused by name (`410 Gone`) before any bytes move — it never answers with the slot's newer pixels. So sample-and-exchange **as you go**; batching ids to resolve later cannot work. The refusal states both generations ("this id published generation 105, the slot is on generation 163"), and that gap measures how far behind the sample fell. The channel form already retries against newer bags and reports on stderr what it retried, how many bags it examined, and over how many tap rounds.

### The one surviving env var
`STREAMLIB_CAMERA_DEVICE` overrides which capture node an example opens. It is read by the example app's own `app.py`, **not** by the engine — so it works for the apps under `examples/` and nowhere else.

## Window capture — only when the window is the subject
Grabbing the window is still the right tool for exactly one thing: proving the **present / swapchain** path, which is the one stretch of pipeline that exchange does not traverse. `runtime/streamlib-engine/tests/fixtures/e2e_camera_display.sh` does it (`xdotool search --name` → ImageMagick `import`, or `xwd` + `capture_window.py`), and it needs `$DISPLAY`. For anything upstream of the present — the camera frame, a kernel's output, a filter's result — use exchange; a composited grab is the wrong pixels for that job.

## PSNR rigs
Three fixture rigs guard the color path; each has bug-injection modes that must deterministically FAIL to prove the gate is live:
- **`e2e_fixture_psnr.sh <out> h264|h265`** — the checked-in reference PNGs through `codec_roundtrip_rig` (fixture source → encoder → decoder → display), scored with `cargo xtask psnr score`. One cold rig run per reference, because a decoded bag carries nothing to pair on; frames are read by tap + exchange, never off the display. Negative modes: `PSNR_INJECT_BUG=swap-channels` (R↔B), `bt601-bt709` (matrix), `range-swap` (PC/TV range), `swap-chroma` (Cb↔Cr transposition — what the chroma floor exists for). **A whole-set `swap-chroma` run does not prove the chroma floor**: out-of-gamut clamping drags Y down on `complex_pattern` and `solid_blue`, so those two fail on luma alone and the run would exit non-zero with the floor deleted. What proves it is the `solid_red` and `solid_green` rows failing at Y ≥ 35 dB — run `REFERENCE_STEMS="solid_red solid_green"` to make that the whole arm. Both codec arms score against the same references at the same floors: Y ≥ 35 dB pass / 30–35 warn / < 30 fail, and either chroma plane under 30 dB failing outright. **This rig cannot see a lost CTU crop** — `score_one_pair` crops the decode to the reference extent before comparing, and the conformance window's origin is (0, 0), so an uncropped 1088-tall decode scores byte-identically to a correctly cropped one. The crop is gated by `cargo test -p streamlib-media-builtins --test h265_decoder_completes_the_round_trip` on the rig, whose extent assertion is the only thing that fails when the window stops reaching published frames.
- **`e2e_fixture_psnr_vivid.sh <out> h264|h265`** — V4L2 colorimetry gate on a saturated single-color pattern vs a checked-in baseline TSV, via `cargo xtask psnr channel-means`; negative `INJECT_BUG=bt601-bt709` / `swap-channels` / `swap-chroma` (a transposition turns a saturated primary into another one, which is the largest drift a channel mean can carry). Each codec locks against its own baseline file (`psnr_vivid_baseline.tsv` for h264, `psnr_vivid_baseline_h265.tsv` for h265). Measured 2026-08-31 the two agree to 0.0001 against a ±0.05 tolerance, so the split is headroom for a codec that does diverge, not a gap one shared file could not have covered. (Range-swap is refused here — a saturated primary sits at the end of the coded range and clips straight back; the main rig's gradients catch it.)
- **`e2e_fixture_psnr_jpeg.sh <out>`** — GPU JPEG decode. Still on the *pre-#2085* shape: it drives the `jpeg-psnr` example and scores with ffmpeg, because the JPEG rung is held (#1212). It implements the three colour modes in its own `case` statement and refuses `swap-chroma` by name, so it is the two video rigs that carry the fourth.

**PSNR pass bar:** Y ≥ 35 dB good · 30–35 dB acceptable, flag it · < 30 dB regression (investigate color matrix / range / plane layout). Chroma has one floor and no acceptable band: either U or V under 30 dB fails the frame outright. For the two video rigs the bars and the four injection modes live in `cargo xtask psnr` — pure GPU-free image math whose unit tests run in CI, so the scorer itself is gated even though the runs that feed it are not; ffmpeg and ImageMagick are out of *those* two scoring paths. The JPEG rig still shells out to ffmpeg.

## Networking arm

Two rig-only fixtures, one per extension wheel, each owned by the wheel it proves. They are the codec rigs' decode-back with a network hop inside it: the vivid camera and the microphone out through `H264Encoder` / `OpusEncoder`, back through the wheel's player or subscriber into `H264Decoder` / `OpusDecoder`, and the decoded frames read by tap + exchange and scored with `cargo xtask psnr channel-means` against `psnr_vivid_baseline.tsv` at ±0.05. **The lock is that score, never a liveness check** — the network sits inside a path the codec rig already scored, so drift is the wheel's.

- **`packages/streamlib-webrtc/tests/live/whip_whep_roundtrip.sh [out]`** — WHIP publish to Cloudflare Stream and WHEP play-back of the same live input.
- **`packages/streamlib-moq/tests/live/moq_broadcast_roundtrip.sh [out]`** — publish and subscribe through a draft-16 relay, plus the CMAF **interop arm**: `moq-sub`, built from `cloudflare/moq-rs` (`cargo install --git https://github.com/cloudflare/moq-rs moq-sub`), reads the same broadcast. That is the interop proof the owner asked for on 2026-09-05 — a third-party client parsing the catalog, accepting the init segment and decoding the media beats matching a captured reference in-repo. Only an absent `moq-sub` binary or `SKIP_INTEROP=1` downgrades it to a report; a missing subscribe credential is a cannot-run like any other, and a `moq-sub` that runs and refuses fails the run.

  **Two things that arm gets wrong if you rebuild it from scratch**, both found by review after a green run: `moq-sub` fetches `.catalog` only when passed `--catalog`, and without it silently falls back to hardcoded `0.mp4` / `{track_id}.m4s` names — so the catalog writer can be entirely broken and the arm still passes. And `cargo xtask mp4-inspect` bails only on a missing `moov`, so a capture holding just the init segment parses perfectly: the verdict has to read the *fragment* count, not the exit code.

  **The MoQ fixture runs both containers**, one node each, in turn: `cmaf` — which is the only one `moq-sub` can read, so the interop arm belongs to it alone — and `streamlib_bag`, which carries a **data arm** beside the media. `CONTAINER_FORMATS` selects them (default `"cmaf streamlib_bag"`); each arm takes the next control port up, its own broadcast name and its own subdirectory of the output. The data arm publishes a telemetry bag per tick through `track_names=["video", "audio", "telemetry"]` and reads it back off the subscriber's `data_bags`. Its verdict is **exact, not tolerant** — every bag says which frame it is, and both its `blob` and its `stamp_ns` are derived from that, so each bag carries its own expected value across the network; `verify_tapped_telemetry_bags.py` recomputes them. A bag that came back a `str` instead of `bytes`, one bag replayed as every bag, and a restamp on the way all fail it, and the stamp is checked twice over — from the payload and from the transport frame's header, which agree only if the producer's instant survived untouched. Several tap rounds are merged because one tap collects over a window of about half a second.

Each takes `SAMPLE_COUNT`, `SAMPLE_EVERY`, `TOLERANCE`, `RUN_SECONDS` and `MEDIA_DEADLINE_SECONDS` from the environment; read the script header for the full list. `MEDIA_DEADLINE_SECONDS` is the one that matters when a run reports no frames — a relay connect and a CMAF init handshake sit between the graph coming up and the first decoded frame, so the fixture waits for one bag before spending the exchange budget.

**Each wheel is measured through its own venv** (`packages/<wheel>/.venv`), which must hold the engine wheel *and* a current `maturin develop` build of the extension. A stale `.so` there would be scored and reported as a pass for code that is not in the tree, so the fixture refuses by name rather than measuring it — `maturin develop` before every run, the same rule `/verify-audio` has.

### Credentials — and why absent ones are never a pass

Every endpoint in this arm carries its own credential **in the URL path**: Cloudflare Stream puts the stream key there, and a draft-16 MoQ relay is provisioned per account with its token on the CONNECT `:path`. There is no credential-free draft-16 relay, so the URL *is* the secret.

- They are read from the environment, with the repo-root gitignored `.env` as the fallback: `CLOUDFLARE_WHIP_URL`, `CLOUDFLARE_WHEP_URL`, `CLOUDFLARE_MOQ_DRAFT_16_URL`, `CLOUDFLARE_MOQ_PUB_SUB_TOKEN`, `CLOUDFLARE_MOQ_SUB_TOKEN`. An exported `STREAMLIB_*` value always wins.
- **Absent credentials exit 77 — cannot-run, never a pass.** Report it as cannot-run in the template's Outcome line.
- **Never echo one.** The scripts redact the endpoint in their own output; do the same in a report, a PR body, or a log excerpt you paste. `streamlib graph` renders every processor's config, so a MoQ or WHIP node's graph JSON contains the token — read it in a pipe, never save it into the evidence directory and never attach it.
- **One unavoidable exception, so it is not mistaken for a leak.** `moq-sub` takes its URL positionally and reads no environment variable, so the *subscribe* token sits in that process's `/proc/<pid>/cmdline` for the arm's 25 seconds. It is the subscribe-only token, and the fixture scrubs the tool's stderr before keeping the log.


## Audio loopback rigs
Two fixture rigs guard the audio path, measuring a known signal — tone frequency / amplitude / distortion, plus a DTMF symbol grid whose *spacing* is what a dropped block moves. Both gate on `virtual_audio_device.sh check` and **exit 77** (`SKIP: no virtual audio device available on this machine`) where no PipeWire session is reachable; 77 is cannot-run and is never reported as a pass:
- **`e2e_audio_loopback.sh <out>`** — `pw-play` into a null sink, `pw-record` off its monitor, no StreamLib in the path, so it answers "is the rig sound" when the engine won't build. Negative modes: `INJECT_BUG=silence` (30 ms of the tone body zeroed), `drop` (30 ms excised, shifting everything after), `gain` (0.6× amplitude).
- **`verify_audio_loopback.sh [--count N] [--port PORT]`** — the same loop with `SpeakerSink` playing and `MicrophoneSource` capturing, so both ends are StreamLib; it runs the block-level channel contract on the microphone's port first. No injection mode — corrupting what the *engine* carries is a different fixture's job.

Drive these through **`/verify-audio`**, which owns the workflow: it picks the mode, re-runs the rig-only fixture when the through-engine one fails (so "the engine broke it" is never read as "the rig is broken"), and reports the numbers with the spectrogram.

## Modes
- **SELF-RUN (primary — rig available).** Run the pipeline yourself, no owner in the path. Probe the rig first (device nodes, `$DISPLAY`, `/dev/dri/*`) and only self-run when it is present.
  1. **Launch** under the Bash `dangerouslyDisableSandbox` bypass — the sandbox blocks the rig, and the bypass is what unlocks GPU/V4L2/X11. A *Python* app under `examples/` has no build step between an edit and the run: `streamlib run --dir <app>`, backgrounded, with its output redirected to a log file you will grep. The codec rig is an engine example and *is* a workspace member — `cargo run -p streamlib-engine --example codec_roundtrip_rig` (`--codec h264|h265`, `--source fixture|camera`, and `--camera /dev/videoN` for the camera arm — the rig takes its device as an argument and reads no environment variable). The Rust example crates under `examples/` are not workspace members, so they build and run from their own directory; the fixture scripts carry the current invocation. Point it at the vivid virtual camera with `STREAMLIB_CAMERA_DEVICE=/dev/videoN` (resolve N by probe; the default grabs whatever capture device the engine finds first). The CLI ships in the wheel — use `sdk/streamlib-python-wheel/.venv/bin/streamlib` when nothing is on PATH.
  2. **Confirm it is live**: `streamlib nodes` until the node registers, then `streamlib graph` for the topology. Derive the channel name from it as above, and `streamlib tap` it once to confirm both the name and the id field before you exchange. These are control-plane reads — `rig-brake` lets them through, and they need no bypass.
  3. **Read pixels**: `streamlib exchange --channel <chan> --out <dir> --count N`, on a channel chosen for what the change touched. Prefer a **mid-graph** channel — that it needs no window is the proof the observation isn't changing the graph.
  4. **Audit**: Read each printed PNG and describe it / compute PSNR, per the checklist below. Attach the PNG(s) to R2 and embed them in the PR (see the `attach-artifact` skill).
  5. **Stop the node** with SIGTERM and require a clean exit — `rt.run()` owns teardown, and a node that needs SIGKILL is a finding, not a flake.

  Read-only observation evals auto-run, but a real-world SAFETY gate (actuators, motors, drone control) still asks the owner first.
- **Handshake fallback (rig unavailable / bypass denied).** Print the command block for the owner's terminal, then audit what it produced. Two sub-modes:
  - *Interactive* — print the command block now; the owner runs it; you audit the output directory in the same session.
  - *Async* — the owner comments "done, output in `<dir>`" on the issue; spawn `evidence-verifier` to audit `<dir>` when they report back.

## Auditing the output (all modes)
1. **Log gates — all zero.** Grep the pipeline log for `OUT_OF_DEVICE_MEMORY`, `DEVICE_LOST`, `process() failed`, `Validation Error`. Any nonzero fails (a `Validation Error` is acceptable only if it also exists on `main` for the same scenario — say so if you claim it).
2. **Progress markers** — first-frame-encoded/-decoded/-captured and ≥1 progress line fired.
3. **Read every exchanged PNG with the Read tool and describe what it shows.** "Looks fine" is banned. A black/uniform frame with clean logs **IS a regression**.
4. **PSNR vs the pass bar** when a reference exists; for a real camera write `n/a — real-camera source` and treat the visual description as the sole gate.
5. **Fill the report template below, verbatim.**

## Standardized E2E report template (the single greppable source — fill verbatim)

````markdown
### E2E Test Report

- **Scenario**: encoder/decoder | camera+display-only | networking (whip-whep | moq-broadcast cmaf | moq-broadcast streamlib_bag)
- **App / fixture**: `examples/camera-python-effects` | `examples/camera-display` | `e2e_camera_display.sh` | `whip_whep_roundtrip.sh` | `moq_broadcast_roundtrip.sh`
- **Codec**: h264 | h265 | n/a
- **Camera device**: `/dev/videoN` (vivid | Cam Link 4K | other)
- **Resolution**: 1920x1080 | 1280x720 | other
- **Run length**: <seconds the node was up before SIGTERM>
- **Build profile**: debug | release | n/a — Python app, no build step
- **Command**:
    ```
    <exact launch command with env vars, and the exchange command>
    ```

#### Log signals

- `OUT_OF_DEVICE_MEMORY`: <count> (0 = pass)
- `DEVICE_LOST`: <count> (0 = pass)
- `process() failed`: <count> (0 = pass)
- `Validation Error` (with `VK_LOADER_LAYERS_ENABLE=*validation*`): <count or "not enabled">
- `First frame encoded` / `First frame decoded` / `First frame captured`: <timestamps or "not seen">
- `Encode progress` / `Decode progress` high-water mark: <frames>

#### Exchanged frames

- Channel: `<processor>/<output_port>` — <mid-graph, or terminal and why>
- `DisplayWindow` in the observation path: no | yes — <why the window was the subject>
- Sampling: `--count <N>` `--every <N>`
- PNGs read with Read tool: `<path1>`, `<path2>`, … (the paths the run printed)
- **What was in the image(s)**: <one or two sentences per PNG read — what you
  actually saw. E.g., "frame 60: dark room with chair back and wood door,
  matches the Cam Link scene" or "frame 30: vivid green/purple SMPTE bars
  with `00:00:06:603` timecode overlay, matches expected test pattern". A
  response of "looks fine" is NOT acceptable — describe the content so a
  reviewer can tell at a glance whether you actually looked.>
- Recycled ids retried / bags missing the id field: <from the run's stderr report, or "none">
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
