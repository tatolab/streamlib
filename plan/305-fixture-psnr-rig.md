---
whoami: amos
name: '@github:tatolab/streamlib#305'
adapters:
  github: builtin
description: Fixture-based PSNR rig for encoder/decoder roundtrips — Build a frame-aligned PSNR rig (checked-in reference PNGs fed through BgraFileSource → encoder → decoder) so encoder/decoder PSNR actually measures encode loss instead of timecode drift. Today every scenario ends up as "n/a" per docs/testing.md.
github_issue: 305
---

@github:tatolab/streamlib#305

## Branch

Create `test/fixture-psnr` from `main`.

## Steps

1. Check in a reference PNG set under `libs/streamlib/tests/fixtures/`:
   - Solid colors at known BT.601 / BT.709 values (catches color-matrix bugs).
   - Horizontal / vertical gradients (catches plane-stride, chroma-subsample bugs).
   - A natural photograph (realistic encode quality).
2. Build a `vulkan-video-psnr` example (or extend `vulkan-video-roundtrip` with a `--fixture <path>` flag) that feeds reference PNGs via `BgraFileSource` deterministically.
3. Make the display PNG sampler key its outputs by input-frame index (not wall clock) so reference ↔ decoded pairing is exact.
4. Add a shell / cargo-test harness that runs each reference through encode+decode and computes PSNR via ffmpeg:
   ```bash
   ffmpeg -i <input_ref>.png -i <decoded>.png -lavfi "psnr=stats_file=psnr.log" -f null -
   ```
5. Enforce the [`docs/testing.md`](../docs/testing.md#psnr--how-to-compute) thresholds: Y ≥ 35 dB pass, 30–35 dB warn, < 30 dB fail.
6. Update the **PSNR** section of `docs/testing.md` to promote this rig as the primary encoder/decoder PSNR path and demote the camera-based discussion to a best-effort note.

## Verification

- Running the rig on `main` + this branch produces numeric Y/U/V PSNR for every reference frame (no `n/a`).
- Synthetic bug-injection test: a deliberate BT.601 ↔ BT.709 color-matrix swap drops Y PSNR below the fail threshold and the harness reports `FAIL`.
- File the standardized [test-report](../docs/testing.md#standardized-test-output-template) including PSNR values at each reference image.

## References

- PR #301 retest — measured 18 dB on vivid-vs-vivid, illustrating why ad-hoc PSNR is useless today: https://github.com/tatolab/streamlib/pull/301#issuecomment-4274680105
- [`docs/testing.md`](../docs/testing.md)
