---
whoami: amos
name: '@github:tatolab/streamlib#282'
adapters:
  github: builtin
description: 'Migrate vulkan-video to Vulkan 1.4 sync2 / SubmitInfo2 — Migrate vulkan-video''s ported nvpro-samples code to Vulkan 1.4 sync2 (SubmitInfo2, cmd_pipeline_barrier_2) and delete submit_to_queue_legacy. Finishes the modernization #261 started on the streamlib side.'
github_issue: 282
blocks:
- '@github:tatolab/streamlib#279'
---

@github:tatolab/streamlib#282

## Branch

Create `refactor/vulkan-video-sync2` from `main` (after #279 validates the per-queue mutex + device-lock fixes work).

## Steps

1. Migrate all `cmd_pipeline_barrier` calls in `libs/vulkan-video/src` to `cmd_pipeline_barrier_2` with `DependencyInfo` + `ImageMemoryBarrier2` / `BufferMemoryBarrier2`
2. Migrate all 6 `vk::SubmitInfo` submit sites in vulkan-video to `vk::SubmitInfo2` (encode/submit.rs, encode/staging.rs, decode/mod.rs, vk_video_decoder.rs, nv12_to_rgb.rs, rgb_to_nv12.rs)
3. Rename `RhiQueueSubmitter::submit_to_queue_legacy` → `submit_to_queue` taking `&[vk::SubmitInfo2]`
4. Update `VulkanDevice` impl to call `queue_submit_2`; update `RawQueueSubmitter` likewise
5. Delete the standalone `VulkanDevice::submit_to_queue_legacy` inherent method
6. Grep-verify zero remaining `queue_submit(`, `cmd_pipeline_barrier(`, `submit_to_queue_legacy` in non-test code
7. Run encoder + decoder unit tests
8. Re-run the full roundtrip from #279 (release build) to confirm no regression

## Testing goals

- Positive: encode/decode unit tests pass; full roundtrip passes in release build
- Negative: grep confirms no Vulkan 1.0 submit/barrier patterns remain in production code
- Regression: no new flakiness; no perf regression (expect modest improvement from precise stage masks)

## Scope notes

This is mechanical but touch-heavy. Keep it as its own PR — no behavioral changes beyond the API migration. Any logic changes discovered along the way become separate follow-ups.
