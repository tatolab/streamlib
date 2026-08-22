# Raw handles export the allocation, gated at Full

Rationale for the `[raw-handle-export-contract]` entry in `docs/plan/ARCHITECTURE.md`
§Packages, decided by the owner 2026-08-21 — resolving the recorded reservation on the
handle-export contract's reach by reaffirmation with a stated gate, not narrowing.

## Trigger

Read this before widening any raw-handle export surface, before moving an export onto
the Limited capability surface, before adding a field to the OPAQUE_FD export object,
and when someone asks why an exported fd can outlive its frame or why there is no
per-frame raw export.

## The decision

1. The reach stands: a third-party native package — its own Vulkan or CUDA stack, or
   a whole other rendering engine — may take a raw memory fd for an engine surface
   and run against it. The reservation resolves as reaffirmation with a gate.
2. The gate is the capability typestate: raw handles mint only where the typestate is
   Full (setup / teardown). Per-frame reach from `process()` is the engine-ordered
   device-tensor scope, never a raw fd. This states as plan invariant what was
   already the shipped shape — `export_dma_buf` lives on the Full surface only.
3. A raw handle names the allocation, never the frame. The surface-id lifetime
   guarantees end at export: an fd held past checkout release reads whatever the pool
   writes into the slot next — memory-safe (the fd pins the payload), semantically
   the producer's next frame. Holders that need frame semantics use surface ids and
   the device-tensor scope.
4. The OPAQUE_FD spelling is `export_opaque_fd` on the Full capability surface,
   returning a typed export object (`OpaqueFdTextureExport`): allocation-stable
   metadata only — byte size, extent, format, what is true for the allocation's whole
   lifetime — and a caller-owned freshly-dup'd fd, one ownership rule shared with the
   DMA-BUF flavour. `export_dma_buf`'s refusal of the flavour points at the new name.

## Rejected alternatives

- **Narrowing to views only** (DLPack / CUDA Array Interface as the only export) —
  walks back a shipped public surface and deletes the native-import capability the
  "texture reach, not names-only" rejection in `python-kernel-api.md` deliberately
  bought.
- **Reaffirming without the gate** — leaves the reservation a concession note; a
  raw export on the Limited surface could later land without tripping any plan text.
- **A lifetime-scoped export** (a `with` block revoking the handle at exit) — an
  exported fd is a dup and cannot be revoked; the scope would be theater. Any real
  tightening is about who may mint, never how long they hold.
- **Restricting raw export to unpooled surfaces** — closes the allocation-vs-frame
  gap by construction, but breaks the pooled DMA-BUF hand-off (camera frames to
  EGL / V4L2 consumers), the DMA-BUF flavour's primary use.
- **A widened tuple or a plain dict** for the OPAQUE_FD return — positionally
  fragile or untyped; the wheel promises typed surfaces under the stub gate, and
  ungrowable returns are why the extra metadata could not overload `export_dma_buf`.
- **An fd-owning export object with a detach step** — leak-safer for the hold case,
  but ceremony on the dominant hand-off case (a foreign importer adopts the fd), and
  it would split the ownership rule between the two flavours.
- **A full checkout snapshot as metadata** — exports per-frame state (image layout,
  plane layout, timeline edges) that is stale or meaningless by the time it is read;
  an allocation-lifetime handle must not promise per-frame truths.

## Consequences

- A texture meant for raw export is acquired in `setup()`; a texture acquired
  per-frame in `process()` cannot be raw-exported, by design.
- A foreign importer does its own synchronisation. Dispatch is synchronous, so
  exported memory is defined whenever Python holds control; nothing more is
  promised, and no timeline edge crosses the export.
- A raw fd bypasses the surface-id lifetime contract deliberately; the contract's
  guarantees are stated as ending at export rather than pretended to extend past it.
- The export object can grow fields without breaking callers — the reason it is an
  object rather than a tuple.
