---
whoami: amos
name: Retest camera+encoder+display roundtrip after Vulkan-cleanup follow-ups and GPU capability rewrite
status: pending
description: Rollup retest #2 — after the #294 follow-ups (#335 h265 teardown race, #336 NV12 chroma, #337 harness hermeticity, #338 validation wiring) and the #319 GPU capability umbrella land. Verifies the real blockers from #294 are resolved and the capability refactor didn't introduce new regressions in the camera / encoder / decoder / display path.
github_issue: 341
dependencies:
  - "down:H.265 shutdown race when decoder lags encoder (Cam Link-class sources)"
  - "down:NV12 direct-ingest chroma collapse (RGBA↔NV12 converter)"
  - "down:E2E test harness non-hermetic on NVIDIA Linux"
  - "down:StreamRuntime does not honor VK_LOADER_LAYERS_ENABLE / VK_INSTANCE_LAYERS"
  - "down:GPU capability-based access (sandbox + escalate)"
adapters:
  github: builtin
---

@github:tatolab/streamlib#341

## Why this exists

#294 ran the post-Vulkan-cleanup rollup retest and discovered:

- Two real P0/P1 bugs that need code fixes (#335, #336).
- Two tooling gaps that block reliable retesting (#337, #338).
- Evidence that the existing GPU context / allocator shape is
  implicated in the driver-state contamination (#337) and likely
  interacts with the shutdown race (#335) — suggesting the
  #319 capability rewrite should land before the next
  retest so we're not chasing the same issues through old
  scaffolding.

This ticket is the next retest round. It does NOT start until all
five dependencies have merged. It is explicitly the "does the
capability rewrite plus the Vulkan fixes actually leave us
crash-free, color-correct, and CI-reliable?" gate.

## Branch

Create `test/post-fix-retest` from `main` after all dependencies
merge.

## Scope

Same matrix as #294, plus the scenarios that couldn't run in #294:

1. **Core roundtrip matrix** (same as #294):
   - Release h264 / h265 × vivid / Cam Link / v4l2loopback-testsrc2.
   - Debug h264 / h265 × vivid / Cam Link.
2. **Cold-start hermeticity protocol** (new — from #337):
   - Each scenario runs in a fresh shell via a harness script.
   - Between scenarios: `pkill -9` stragglers, GPU-idle barrier,
     5–10 s cooldown (exact protocol from #337 once that lands).
   - Run the first-sweep matrix *back-to-back* in one shell as a
     regression gate on #337 — if any scenario still FAILs in a
     position >1 that passed cold, #337 isn't really fixed.
3. **PSNR fixture rig** (h264 + h265):
   - Must PASS `complex_pattern` this time (V ≥ 30 dB). Previously
     24.3 dB.
   - Gates #336 (chroma converter fix).
4. **Validation-layer sweep** (new — from #338):
   - Run each core scenario under
     `VK_LOADER_LAYERS_ENABLE=*validation*` and grep the log for
     VUIDs and `Validation Error`. Zero is the pass bar (possibly
     an allowlist for pre-existing known-benign messages documented
     in #338's fix commit).
5. **Dynamic processor add/remove** (new — from #340 if landed):
   - Start camera-only, add encoder+decoder+display live, remove,
     add again. Confirms #319 reconfigure-via-escalate works.
6. **H.265 Cam Link shutdown stress** (new — #335 regression gate):
   - 10 back-to-back h265 Cam Link 30 s runs cold-started.
   - Pass bar: 10 / 10 clean exits, zero core dumps, zero shutdown
     hangs, decoder reaches within 50 % of encoder's final frame
     count before shutdown.

## Exit criteria

Report in the standardized E2E template from
[docs/testing.md](../docs/testing.md#standardized-test-output-template).
Stability score ≥ 90 / 100, specifically:

- Zero SIGSEGV, OOM, DEVICE_LOST in any release run (cold OR sequential).
- PSNR `complex_pattern` Y/U/V all ≥ 30 dB on both codecs.
- vivid and NV12-direct-ingest sources produce full-color output
  (visual inspection + PSNR).
- Debug and release behaviour match across h264 / h265 / vivid /
  Cam Link.
- Pipeline throughput ≥ 25 fps / 30 s on at least one source per
  codec (if #339 lands, vivid should qualify too).
- Validation layer: zero new VUIDs with the layer wired via #338.
- Dynamic add/remove: clean add, clean remove, no residual GPU
  resources (visible via pool-occupancy logs if #319 exposes them).
- Hermetic harness: full matrix runs back-to-back in one shell
  matches cold results within noise.

## Non-goals

- Color-management (primaries, transfer, range, tone mapping) —
  that's #312. This retest only needs chroma correctness.
- Pipeline resolution propagation — that's #310, queued behind
  #319.
- MoQ / color umbrella re-evaluation — that happens in #217's
  replan, not here.

## Reference

- #294 — prior retest and the report this one supersedes.
- docs/retests/294-post-vulkan-cleanup-retest.md — detailed findings,
  cold-verification matrix, failure signatures.
