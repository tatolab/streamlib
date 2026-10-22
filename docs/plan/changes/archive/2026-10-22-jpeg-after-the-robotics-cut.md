# jpeg-after-the-robotics-cut

> **Approved by the owner, 2026-10-05.** Cut to removal only by the owner on 2026-10-21: MJPEG
> capture is not built, and no JPEG decoder survives anywhere.

The removal half of the JPEG align (owner, 2026-10-04). After this change the tree holds no
JPEG block, no JPEG decoder, no nvJPEG backend, no `libnvjpeg` probe, no
`ThirdPartyGpuCapabilities`, no `sdk/vulkan-jpeg`, and none of the held JPEG consumers or their
fixture. Nothing a stream author writes or configures changes.

**Scale gate — this skill, plus the existing ADR** (`docs/decisions/jpeg-after-the-robotics-cut.md`).
Deletion only: no new behaviour, no IPC wire, no Python API contract.

**Precondition.** Every entry touched is DECIDED: §Media I/O, the codec blocks (`JpegDecoder`
retired, the nvJPEG tree and probe deleted) and the held codec consumers; §Consumers (the JPEG
pair deletes outright).

**Verified against the tree 2026-10-05 (HEAD 62395e617).** `E` means
`runtime/streamlib-engine/src` and `J` means `sdk/vulkan-jpeg`.

**The removals**
- `E/vulkan/_nvjpeg_impl_pending_/`: 8 files, 2,713 lines, never compiled. Its
  `tests_pending/gpu_decode.rs:65-333` holds a GPU-against-CPU-reference PSNR test worth carrying
  forward (Y ≥ 50 dB; its reference hardcodes 4:2:0 at `:194-199`).
- `ThirdPartyGpuCapabilities` (`E/vulkan/rhi/vulkan_device.rs:59-141`): `nvjpeg` is its only
  field. Also the device field (`:160-164`), the probe call (`:682-688`, `:1588`), the getter
  (`:3009-3025`), three tests (`:4457-4506`) and the re-exports (`E/host_rhi.rs:64`,
  `E/vulkan/rhi/mod.rs:32`).
- `packages/jpeg` (472 lines) builds against `streamlib-plugin-sdk`, `streamlib-plugin-abi` and
  `streamlib-macros` 0.16.0, plugin crates the tree no longer has. `examples/jpeg-psnr` (253 lines)
  and `E/../tests/fixtures/e2e_fixture_psnr_jpeg.sh` (328 lines) run through it.
- Residue that names them:
  - the `verify-live` skill, whose `SKILL.md:66-71` counts "three fixture rigs";
  - `.claude/scripts/tests/rig-brake.test.sh:221`, `:243`, `:246` and `:285`, which use
    `jpeg-psnr` paths as hook inputs;
  - comments at `E/core/context/texture_ring.rs:123-124` and
    `xtask/src/lint_logging.rs:376`, `:411` (`_apple_impl_pending_` stays as the example
    there);
  - `docs/architecture/third-party-gpu-backends.md`, whose only shipped instance is nvJPEG
    and `J`'s backend trait, and `texture-ring.md:30`, `:206`, `:282`, `:330`;
  - `Cargo.toml:5`, `deny.toml:184-187`, and the comment in `release.yml`'s version-pin sync step.
- `J` itself: a workspace member (`Cargo.toml:5`, `deny.toml`) whose only consumers are
  `packages/jpeg` and the nvJPEG tree's pending tests. With no decoder surviving, it is
  deleted whole.

## MODIFIED: §Media I/O — the codec blocks, and the held consumers

At ship, the codec-block entry and the held-codec-consumers entry drop their pointers. They
state, as shipped, that the nvJPEG tree, its probe, `packages/jpeg`, `examples/jpeg-psnr` and
`e2e_fixture_psnr_jpeg.sh` are gone. Nothing is mined. `ThirdPartyGpuCapabilities` goes with
its only field.

## MODIFIED: §Consumers (`:660`, `:711-727`)

- "fourteen converted beside two held" becomes "beside one held" (`examples/screen-recorder`).
- The held list loses its codec-blocks clause and keeps audio plugins and screen capture.

## Factual records in the same PR

- `docs/architecture/third-party-gpu-backends.md` is deleted: it describes a pattern with no
  shipped instance left.
- `texture-ring.md` loses its JPEG decoder lines and its `J` citation.

## Tickets

Milestone #63.

1. #2639 — the JPEG leftovers are gone, `sdk/vulkan-jpeg` with them. Needs no rig. It carries
   every bullet below.

## REMOVED

- REMOVED: sdk/vulkan-jpeg
- REMOVED: vulkan-jpeg
- REMOVED: vulkan_jpeg
- REMOVED: SimpleJpegDecoder
- REMOVED: JpegDecodeBackend
- REMOVED: VulkanComputeBackend
- REMOVED: JpegDecoder
- REMOVED: runtime/streamlib-engine/src/vulkan/_nvjpeg_impl_pending_
- REMOVED: _nvjpeg_impl_pending_
- REMOVED: nvjpeg
- REMOVED: nvJPEG
- REMOVED: ThirdPartyGpuCapabilities
- REMOVED: third_party_gpu_capabilities
- REMOVED: probe_nvjpeg_loadable
- REMOVED: docs/architecture/third-party-gpu-backends.md
- REMOVED: third-party-gpu-backends
- REMOVED: packages/jpeg
- REMOVED: examples/jpeg-psnr
- REMOVED: jpeg-psnr
- REMOVED: runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_jpeg.sh
- REMOVED: e2e_fixture_psnr_jpeg
- REMOVED: JpegBytesSource
- REMOVED: worst_case_coefficient_buffer_bytes_420
- REMOVED: CHROMA_SAMPLING_420
