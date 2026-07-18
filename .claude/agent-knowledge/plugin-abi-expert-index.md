# plugin-abi-expert — symptom index

Knowledge lives in `docs/`; this file is only routing. Update in the same PR that adds a learning (see `.claude/rules/docs-policy.md`).

Match your symptom, read the doc, then verify its claims against current code — a learning is the best-known state when it was written, not ground truth.

| symptom / trigger | read |
|---|---|
| A cdylib pipeline runs end-to-end with zero errors / panics / validation complaints but produces all-zero (black) output — a host-side `make_*_borrow` reconstructed a PluginAbiObject with zeroed cached POD fields (`.width()`/`.byte_size()` return 0) | `docs/learnings/cdylib-make-borrow-cached-fields.md` |
| A GPU package works in-process / as a workspace plugin but corrupts the driver as a separately-built `.slpkg` (NVIDIA double-free in `vkCreatePipelineLayout`) — it hand-rolled RHI on the transited raw host device instead of the cdylib-safe FullAccess primitives. The historical why; #1270 RESOLVED it by deleting the raw-`Arc` transit slots entirely (no non-`#[repr(C)]` slot transits; a package cannot name the host device) | `docs/learnings/slpkg-raw-device-rhi-construction.md` |
