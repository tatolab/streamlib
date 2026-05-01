# Workflow: adapter-labeled issues

Applies to issues labeled `adapter` — anything that authors a new
surface adapter or extends an existing one (vulkan, opengl, skia,
cpu-readback, cuda, …).

## Background the agent must hold before starting

- Surface adapters are the single gateway from a host-allocated GPU
  resource to a customer's framework-native handle. The shape is
  deliberately uniform across every adapter — the **single-pattern
  principle**: generic over `D: VulkanRhiDevice`, host-side
  pre-registration via surface-share, subprocess import through
  `streamlib-consumer-rhi`, per-acquire as timeline wait + layout
  transition (plus a thin escalate-IPC trigger if and only if the
  host has per-acquire work).
- The full implementation contract — checklist, crate skeleton,
  capability markers, runtime wiring, conformance — lives in
  [`docs/architecture/adapter-authoring.md`](../../docs/architecture/adapter-authoring.md).
  Read it end-to-end before starting any adapter work.
- Adapter crates **must not** depend on `streamlib` at runtime —
  only as a dev-dep. Cdylibs that pull the adapter must satisfy
  `cargo tree -p streamlib-{python,deno}-native | grep -c
  "^streamlib v"` returning `0`. CI enforces this via `cargo xtask
  check-boundaries`.

## Things every adapter issue has to decide

1. **Is this a new adapter or an extension?** New adapters land on
   the canonical shape from day one (read
   [`adapter-authoring.md`](../../docs/architecture/adapter-authoring.md)
   end-to-end). Extensions to existing adapters (new capability
   marker, new format support) follow the existing crate's shape —
   don't reshape the crate while extending.
2. **Does the adapter need per-acquire host work?** Most don't
   (vulkan / opengl / skia ride surface-share-only). cpu-readback
   does (`vkCmdCopyImageToBuffer`). If yes, wire a bridge via
   `gpu.set_<name>_bridge(...)` in `install_setup_hook`. If no,
   the hook just allocates + registers + returns.
3. **What handle type does the framework require?** DMA-BUF for
   most GPU frameworks; OPAQUE_FD when the consumer's API takes a
   flat `void*` (cuda + DLPack). `RhiExternalHandle` covers both;
   pick the variant your framework demands.
4. **Polyglot coverage** — both Python AND Deno together (per
   [`polyglot.md`](polyglot.md)). The only legitimate split is
   schema-only / language-specific by construction.

## Rules specific to adapter issues

- **Don't deviate from the single-pattern shape.** The
  [trip-wires section](../../docs/architecture/adapter-authoring.md#trip-wires)
  in `adapter-authoring.md` lists the cases that look like they
  justify a parallel shape but don't. If your situation genuinely
  doesn't fit, surface the disagreement before building.
- **Don't add subprocess-side allocation.** The import-side carve-
  out is `vkImportMemoryFdInfoKHR` + `vkBindBufferMemory` /
  `vkBindImageMemory` + `vkMapMemory` + layout transitions on
  imported handles + sync wait/signal. Anything beyond that
  escalates to the host.
- **Don't author SPIR-V kernels in subprocess code.** Use
  `RegisterComputeKernel` + `RunComputeKernel` (#550) to dispatch
  through the host's `VulkanComputeKernel`.
- **Adapter crate runtime dep graph excludes `streamlib`.**
  Test-only deps go in `[dev-dependencies]`; test-helper bins
  that need `streamlib` go in a sibling `streamlib-adapter-<name>-helpers`
  crate (existing pattern — see
  `streamlib-adapter-vulkan-helpers/Cargo.toml`).
- **Conformance suite is non-negotiable.** Wire
  `tests/conformance.rs` from
  `streamlib_adapter_abi::conformance::run_conformance_suite` for
  every adapter.

## Testing adapter changes

Minimum coverage for a new adapter:

1. `tests/conformance.rs` — full suite green.
2. `tests/round_trip_*.rs` — host writes, subprocess reads (and
   write paths if the adapter is write-capable). Spawns the
   helpers-crate bin.
3. `tests/subprocess_crash_mid_*.rs` — kernel-watchdog release
   path under crash.
4. Polyglot E2E: an example under `examples/polyglot-<adapter>-<scenario>/`
   exercising both Python and Deno runtimes.

For changes to an existing adapter, run the existing test matrix
plus any new tests covering the change.

## PR body additions

```markdown
## Adapter coverage

- **Adapter crate**: `streamlib-adapter-<name>`
- **New adapter or extension?**: new | extension
- **Handle type**: DMA-BUF | OPAQUE_FD | n/a
- **Per-acquire host work?**: no (surface-share-only) | yes (bridge wired)
- **Capability markers impl'd**: VulkanWritable | VulkanImageInfoExt | GlWritable | CpuReadable | CpuWritable | <new>
- **Conformance suite**: ✅ | ❌ (explain)
- **Polyglot coverage**:
  - [ ] Python E2E: <scenario + result>
  - [ ] Deno E2E: <scenario + result>
- **Dep-graph invariant** (`cargo tree -p streamlib-{python,deno}-native | grep -c "^streamlib v"` returns 0): ✅ | n/a
```
