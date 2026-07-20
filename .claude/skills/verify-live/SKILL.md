---
name: verify-live
description: The live end-to-end verification for changes that touch GPU / camera / display / codec. Use when a change needs a real pipeline run (a plain Bash call can't — the rig-brake blocks it), when the owner asks to verify a change on the rig, or when a PR claims E2E evidence that needs auditing. Primary LOOP-RUN mode — the loop runs the pipeline itself via the dangerouslyDisableSandbox bypass, captures the window, and audits it (log gates, PNG content, PSNR); falls back to the owner-terminal command-block handshake only when the rig is unavailable.
---

# verify-live — real-pipeline verification

Unit tests come first and catch most bugs. This skill is for the cases they can't reach: GPU/driver, V4L2, swapchain — where a run is the only proof. A plain `Bash` call cannot run the pipeline (exit 144; the `rig-brake` hook blocks rig-consuming commands), but the Bash `dangerouslyDisableSandbox` bypass unlocks the rig — so when the rig is present the loop runs the pipeline **itself** (LOOP-RUN mode, primary) rather than handing it to the owner. Only when the loop can't run it (rig unavailable, bypass denied) does it fall back to the **handshake**: emit the command for the owner's terminal, then audit what it produced. The `evidence-verifier` agent executes both; this skill is the reference it and any reviewer share.

## Device indices are never hardcoded
Read `docs/rig-profile.local.md` for this machine's video-node / GPU topology, then confirm with a probe (`v4l2-ctl --list-devices`, `--get-fmt-video`). A runtime probe always beats the file. Every `/dev/videoN` in a command block is resolved this way — the indices below are placeholders.

## Scenario decision tree
1. **Can a unit test cover it?** (pure logic, parser, state machine) → write the unit test, done. No rig.
2. **Touches GPU memory / Vulkan / V4L2 / swapchain?** → unit tests miss driver-only failure modes; you need a real run. Pick below.
3. **Affects an encoder or decoder?** → **encoder/decoder roundtrip** (`camera → encoder → decoder → display`), run both codecs.
4. **Only camera / display / GPU-compute / GPU-texture, no codec?** → **camera-display-only** (faster, isolates the path).
5. **Frame-ordering / timestamp / drop-sensitive?** → **v4l2loopback motion** (a `testsrc2` source with a visible per-frame counter, so a drop/repeat shows by eye).
6. **Color-path change?** → the **PSNR fixture rigs** (below), with at least one negative-injection mode to prove the gate isn't vacuous.

When unsure, default to the more demanding scenario (encode/decode also exercises camera + display). Current run commands live in the fixture scripts under the engine's `tests/fixtures/` — read them for the exact invocation (they drift; don't cache them here).

## Display PNG-sampler env vars
Set on any windowed run so it self-terminates and writes AI-readable samples:

| env var | effect |
|---|---|
| `STREAMLIB_DISPLAY_PNG_SAMPLE_DIR` | directory to write PNG samples into; unset disables sampling |
| `STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY` | sample interval in displayed frames (default 30) |
| `STREAMLIB_DISPLAY_FRAME_LIMIT` | auto-exit after N frames — **always set this** so no winit window strands |
| `STREAMLIB_CAMERA_DEVICE` | override the default camera node |

## PSNR rigs
Three fixture rigs guard the color path; each has bug-injection modes that must deterministically FAIL to prove the gate is live:
- **`e2e_fixture_psnr.sh <out> {h264|h265}`** — reference PNGs through `BgraFileSource → encoder → decoder → display`, Y/U/V PSNR vs reference. Negative modes: `PSNR_INJECT_BUG=swap-channels` (R↔B), `bt601-bt709` (matrix), `range-swap` (PC/TV range).
- **`e2e_fixture_psnr_vivid.sh <out> h264`** — V4L2 colorimetry gate on a saturated single-color pattern vs a checked-in baseline TSV; negative `INJECT_BUG=bt601-bt709`. (Range-swap is intentionally not covered here — saturated patterns are range-insensitive; the main rig's gradients catch it.)
- **`e2e_fixture_psnr_jpeg.sh <out>`** — GPU JPEG decode, same shape and same injection modes.

**PSNR pass bar:** Y ≥ 35 dB good · 30–35 dB acceptable, flag it · < 30 dB regression (investigate color matrix / range / plane layout).

## Modes
- **LOOP-RUN (primary — rig available).** The loop runs the pipeline itself, no owner in the path. Build in the sandbox as usual, then run the built binary under the Bash `dangerouslyDisableSandbox` bypass (the sandbox blocks the rig; the bypass is what unlocks GPU/V4L2/X11). Recipe that works on this rig: run with `DISPLAY=:1` and `STREAMLIB_CAMERA_DEVICE=/dev/video0` for the vivid virtual camera (the default `None` grabs the Cam Link 4K on `/dev/video4`); set the PNG-sampler env vars below (always `STREAMLIB_DISPLAY_FRAME_LIMIT` so the window self-terminates); capture the window with `xdotool search --name <window> | import -window <id> <png>`; then Read the PNG and describe it / compute PSNR, and audit per the checklist below. Attach the PNG(s) to R2 and embed them in the PR (see the `attach-artifact` skill). Gate on `capabilities.live_verify == available` (milestone-loop step 1 preflight); read-only observation evals auto-run, but a real-world SAFETY gate (actuators, motors, drone control) still asks the owner first.
- **Handshake fallback (rig unavailable / bypass denied).** Print the command block for the owner's terminal, then audit what it produced. Two sub-modes:
  - *Interactive* — print the command block now; the owner runs it; you audit the output directory in the same session.
  - *Async* — the owner comments "done, output in `<dir>`" on the issue; the next `milestone-loop` turn spawns `evidence-verifier` to audit `<dir>`.

## Auditing the output (all modes)
1. **Log gates — all zero.** Grep the pipeline log for `OUT_OF_DEVICE_MEMORY`, `DEVICE_LOST`, `process() failed`, `Validation Error`. Any nonzero fails (a `Validation Error` is acceptable only if it also exists on `main` for the same scenario — say so if you claim it).
2. **Progress markers** — first-frame-encoded/-decoded/-captured and ≥1 progress line fired.
3. **Read every sampled PNG with the Read tool and describe what it shows.** "Looks fine" is banned. A black/uniform frame with clean logs **IS a regression**.
4. **PSNR vs the pass bar** when a reference exists; for a real camera write `n/a — real-camera source` and treat the visual description as the sole gate.
5. **Fill the report template below, verbatim.**

## Standardized E2E report template (the single greppable source — fill verbatim)

````markdown
### E2E Test Report

- **Scenario**: encoder/decoder | camera+display-only
- **Example**: `vulkan-video-roundtrip` | `camera-display` | `e2e_camera_display.sh`
- **Codec**: h264 | h265 | n/a
- **Camera device**: `/dev/videoN` (vivid | Cam Link 4K | other)
- **Resolution**: 1920x1080 | 1280x720 | other
- **Duration / frame limit**: `STREAMLIB_DISPLAY_FRAME_LIMIT=… ` (run length in seconds)
- **Build profile**: debug | release
- **Command**:
    ```
    <exact cargo run command with env vars>
    ```

#### Log signals

- `OUT_OF_DEVICE_MEMORY`: <count> (0 = pass)
- `DEVICE_LOST`: <count> (0 = pass)
- `process() failed`: <count> (0 = pass)
- `Validation Error` (with `VK_LOADER_LAYERS_ENABLE=*validation*`): <count or "not enabled">
- `First frame encoded` / `First frame decoded` / `First frame captured`: <timestamps or "not seen">
- `Encode progress` / `Decode progress` high-water mark: <frames>

#### PNG samples

- Directory: `<OUT>/png_samples/`
- Sample count: <N>
- Sample interval: `STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=<N>`
- PNGs read with Read tool: `<file1>`, `<file2>`, …
- **What was in the image(s)**: <one or two sentences per PNG read — what you
  actually saw. E.g., "frame 60: dark room with chair back and wood door,
  matches the Cam Link scene" or "frame 30: vivid green/purple SMPTE bars
  with `00:00:06:603` timecode overlay, matches expected test pattern". A
  response of "looks fine" is NOT acceptable — describe the content so a
  reviewer can tell at a glance whether you actually looked.>
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
