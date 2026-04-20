# Workflow: video end-to-end verification

This workflow applies to issues that touch the camera → encoder → decoder
→ display pipeline. Use it when the issue's **Tests / validation**
section says "E2E roundtrip" or when the issue label includes anything
encoder/decoder-related.

The canonical detail lives in @docs/testing.md — this workflow is the
short form an agent follows during `/amos:next` execution.

## When this workflow is mandatory

- Any change to `libs/vulkan-video/` or `libs/streamlib/src/linux/processors/{h264,h265}_{encoder,decoder}.rs`.
- Any change inside `vulkan/rhi/` that the codecs reach through.
- Any change to the H.264/H.265 validator, MP4 writer, or anything
  consuming `Encodedvideoframe`.

If any of those paths are in scope, this workflow runs as the test gate.

## Minimum scenario matrix

Run the following before calling the issue done. For each, collect the
standardized E2E template from @docs/testing.md.

| Scenario | Camera device | Codec | Purpose |
|---|---|---|---|
| vivid baseline | `/dev/video2` | h264 + h265 | smoke, zero-driver-quirks |
| v4l2loopback motion | `/dev/video10` | h264 | motion-sensitive; detects drops/duplicates |
| Cam Link (real hardware) | `/dev/video0` | h264 | validates MMAP + real-UVC driver path |
| Fixture PSNR | `BgraFileSource` | h264 + h265 | measures encode loss vs. reference |

Skip Cam Link only if the hardware isn't physically connected; say so
explicitly in the E2E report.

## The PNG read-tool visual check is mandatory

For every scenario, `STREAMLIB_DISPLAY_PNG_SAMPLE_DIR` must be set, the
harness must write samples, and **at least one PNG must be read with the
Read tool and described by content**. A one-liner of "looks fine" is
NOT acceptable — a reviewer should be able to tell from the description
alone whether the agent actually looked.

See the *"What was in the image(s)"* rule in @docs/testing.md — this is
the single test we've had the most trouble keeping honest, so the rule
is strict.

## PSNR thresholds

- Y PSNR ≥ 35 dB → good
- 30–35 dB → acceptable, flag as a caveat
- < 30 dB → regression; investigate color-matrix / range / plane layout
  before merging

Fixture rig: `libs/streamlib/tests/fixtures/e2e_fixture_psnr.sh`.

## Evidence required in the PR

Paste the filled E2E template from @docs/testing.md (one per scenario)
into the PR description. The standardized template is how reviewers grep
across PRs — verbatim structure only, don't paraphrase.

## Past gotchas to watch for

- NVIDIA DMA-BUF OOM after swapchain creation —
  @docs/learnings/nvidia-dma-buf-after-swapchain.md. Not a real OOM.
- `Validation Error` count must match the baseline on `main` for the same
  scenario. New validation errors = regression unless documented.
- vivid animates slowly; use v4l2loopback + testsrc2 when motion matters
  (see @docs/testing.md § v4l2loopback-motion-scenario).
