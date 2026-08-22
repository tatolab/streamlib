# Raw handles export the allocation, gated at Full

Rationale for the `[raw-handle-export-contract]` entry in `docs/plan/ARCHITECTURE.md`
§Packages, decided by the owner 2026-08-21 — resolving the recorded reservation on the
handle-export contract's reach by reaffirmation with a stated gate, not narrowing —
and refined the same day by a six-lens expert audit (Vulkan external-memory spec, CUDA
interop, engine-architecture practice, robotics-platform practice, Python API shape,
adversarial lifetime) whose findings tightened the statements without moving any of
the five decisions.

## Trigger

Read this before widening any raw-handle export surface, before moving an export onto
the Limited capability surface, before letting any per-frame path — escalate ops
included — reach or mint a raw fd, before adding a field to the OPAQUE_FD export
object, and when someone asks why an exported fd can outlive its frame or why there
is no per-frame raw export.

## The decision

1. The reach stands: a third-party native package — its own Vulkan or CUDA stack, or
   a whole other rendering engine — may take a raw memory fd for an engine surface
   and run against it. The recorded reservation resolves as reaffirmation with a
   gate.
2. The gate is the Full capability surface: raw handles mint only through
   `GpuContextFullAccess`, on every minting path — escalate ops included. Per-frame
   reach from `process()` is the engine-ordered device-tensor scope, never a raw fd —
   and the gate bounds use as well as minting: per-frame data-plane reach, read or
   write, through a held raw fd is out of contract. A foreign engine runs against an
   exported allocation as its sole producer or sole consumer, or accepts
   pool-recycling semantics. Stated honestly: the typestate gates which object, not
   when — a Full reference stashed past `setup()` still mints; the phase bound is
   contract, not a runtime check.
3. A raw handle names the allocation, never the frame. The surface-id lifetime
   guarantees end at export: an fd held past checkout release reads whatever the
   pool hands the slot next — the pool bucket is keyed by width, height and format
   and shared across processors, so possibly another processor's frames.
   Memory-safe — each exported fd holds a spec-mandated reference to the payload,
   and the allocation is dedicated, so a leaked fd pins its whole VRAM block —
   semantically undefined. A raw fd is also write-capable into the allocation: from
   a pooled allocation's first export onward, the immutable-frame guarantee for
   every frame it backs rests on the importer honouring the use bound; the engine
   cannot enforce it against an fd holder. Holders that need frame semantics use
   surface ids and the device-tensor scope.
4. The OPAQUE_FD spelling is `export_opaque_fd` on the Full capability surface,
   returning a typed export object, `OpaqueFdTextureExport` — deliberately outside
   the `GpuSurface*` family prefix, because the object names an allocation, not a
   frame-bearing surface. Metadata is allocation-stable only, and the audit fixed
   the enumeration: whole-`VkDeviceMemory` byte size at offset zero (field spelling
   `allocation_byte_size`, never a tight width×height×bpp figure — an OPTIMAL-tiled
   dedicated image differs), extent, format, the image-creation recipe (tiling,
   usage set, mip/layer/sample counts) a conforming re-import must reproduce,
   dedicated-allocation status — always true for this flavour; a Vulkan importer
   chains `VkMemoryDedicatedAllocateInfo`, a CUDA importer sets
   `cudaExternalMemoryDedicated`, and omitting either is undefined behaviour, not
   leniency — the exporter's memory type index, and the exporting device UUID (an
   OPAQUE_FD is device-bound; importing on the wrong GPU of a multi-GPU rig
   corrupts silently). The texture is consumed as an image — CUDA maps the
   mipmapped array with flags derived from the usage set; a linear buffer mapping
   over OPTIMAL-tiled memory yields block-linear bytes, never pixels. The fd is
   caller-owned and freshly dup'd, one ownership rule shared with the DMA-BUF
   flavour: the fd is the caller's until a foreign import adopts it — Vulkan and
   CUDA imports both adopt on success and leave it with the caller on failure, so
   never close after a successful import, always close after a failed one.
   `export_dma_buf`'s refusal of the flavour points at the new name.

## Rejected alternatives

- **Narrowing to views only** (DLPack / CUDA Array Interface as the only export) —
  walks back a shipped public surface and deletes the native-import capability the
  "texture reach, not names-only" rejection in `python-kernel-api.md` deliberately
  bought.
- **Reaffirming without the gate** — leaves the reservation a concession note; a
  raw export on the Limited surface could later land without tripping any plan text.
- **A lifetime-scoped export** (a `with` block revoking the handle at exit) — an
  exported fd is a dup and cannot be revoked; the scope would be theater. Any real
  tightening is about who may mint and how a holder may use, never how long they
  hold.
- **Restricting raw export to unpooled surfaces** — forecloses the setup-phase
  hand-off of pooled and self-acquired surfaces, the shipped export pattern; the
  per-frame pooled hand-off it would mainly prevent is already out of contract
  under the use bound, so the restriction buys enforcement of a rule the contract
  already states, at the price of the legitimate one-shot case.
- **A widened tuple or a plain dict** for the OPAQUE_FD return — positionally
  fragile or untyped; the wheel promises typed surfaces under the stub gate, and
  ungrowable returns are why the extra metadata could not overload `export_dma_buf`.
- **An fd-owning export object with a detach step** — leak-safer for the hold case,
  but ceremony on the dominant hand-off case (a foreign importer adopts the fd), it
  would split the ownership rule between the two flavours, and a close-on-exit
  middle shape collapses into the same detach ceremony the moment an import
  succeeds.
- **A full checkout snapshot as metadata** — image layout and timeline edges are
  per-frame state, stale or meaningless by the time they are read; an
  allocation-lifetime handle must not promise per-frame truths. Scope note the
  audit added: DRM plane layout — fourcc, format modifier, per-plane offset and
  stride — is allocation-stable, not per-frame, and belongs to the DMA-BUF
  flavour's metadata when its typed object lands.
- **Timeline edges in the export object** — known future pressure, not a permanent
  refusal: when a foreign importer needs GPU-overlap rather than CPU-serialized
  hand-off, timeline-semaphore export rides the same Full gate (the engine already
  mints OPAQUE_FD timeline fds for its own checkout protocol). What stays rejected
  is per-frame edges inside an allocation-lifetime object.

## Consequences

- A texture meant for raw export is acquired in `setup()`; a texture acquired
  per-frame in `process()` cannot be raw-exported, by design. An export set covers
  only allocations that exist when it is taken — the pool grows past it by design,
  and late-grown slots are outside every export set.
- Synchronisation is stated in both directions. Dispatch is synchronous, so
  exported memory is defined whenever Python holds control *and the export's
  backing is still held* — past a checkout release the engine writes the slot
  asynchronously. Symmetrically, an importer completes its own device work before
  returning control: GPU launches are asynchronous, and the engine waits on no
  foreign stream and holds no fence it could wait on. Spec-defined cross-instance
  hand-off needs a queue-family release recorded on the engine's own queue — that
  edge lives in the checkout protocol, which is exactly why frame semantics live
  there; a raw-fd Vulkan consumer's content-definedness rests on NVIDIA's
  empirical preservation across the UNDEFINED-layout bridge, and a CUDA consumer's
  on NVIDIA's Vulkan↔CUDA coherence model.
- A raw fd bypasses the surface-id lifetime contract deliberately; the contract's
  guarantees are stated as ending at export rather than pretended to extend past
  it, and the fd-pins-the-payload claim is proven on the rig alongside the
  implementation: an exported fd imported by foreign code must outlive engine
  teardown of its source allocation.
- The export object can grow fields without breaking callers — the reason it is an
  object rather than a tuple — and the metadata enumeration grows the moment any
  listed constant of the flavour stops being constant.
- `export_dma_buf`'s tuple is grandfathered: the first metadata field it needs —
  the allocation-stable DRM layout every EGL / V4L2 importer requires — moves it to
  a typed export object under the same ownership rule, a clean pre-1.0 rename,
  never a widened tuple.
