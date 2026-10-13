# The shared pools are bounded by memory, a node's frames by its own count

Rationale for the `[shared-pools-bounded-by-memory-budget]` entries in
`docs/plan/ARCHITECTURE.md` §Graphics, decided by the owner 2026-10-12 after two independent
reviews. Change file: `docs/plan/changes/shared-pools-bounded-by-memory-budget.md`.

## Trigger

Read this before adding any count to a pool more than one node draws from, before letting an
acquisition wait for a frame or texture something else holds, before raising a pool cap to
make a refusal go away, and when someone asks why a node with a full output pool drops frames
while the GPU has memory free — or why one stream can still cost another its frames.

## The decision

1. **Count is per node.** A node's pooled frames are limited by its own output pool, and its
   one-off acquisitions by a count kept for that node. Native built-ins draw per-frame
   acquisitions from output pools of their own, on the same terms as a helper's.
2. **Memory is per runtime.** The runtime-wide texture and pixel-buffer pools reuse
   allocations by shape and cap no shape by count. They are bounded by the device's memory
   budget per heap, as the driver reports it through `VK_EXT_memory_budget` and the allocator
   reads it, and by the device's allocation-count limit. Past either, an allocation is
   refused by name and the node drops that frame. Where the driver lacks the extension, the
   budget is the allocator's estimate — 80% of the heap, also capped by the system's
   available memory for a heap that is not device-local — and the runtime says the bound is
   an estimate. On macOS the bound is Metal's recommended working set, measured against the
   runtime's own IOSurface bytes plus the Metal device's allocated size.
3. **Nothing waits on another holder.** An acquisition refuses at once rather than block for
   a frame some other node holds.
4. **Memory comes back.** An unheld entry is freed when its stream stops, when its node's
   output pool is torn down, or after an idle window — never while any holder, lease or
   in-flight submission still names it, and never so that an exportable memory flavour drops
   to zero live allocations.

## Rejected alternatives

- **Raise the shared per-shape cap.** Any count shared across nodes lets one node's held
  frames refuse another node's — the cap just moves the stream number at which it happens.
  At 16 textures of 1280×720 RGBA (≈59 MB) the cap bit long before memory did.
- **No shared bound beyond driver allocation failure.** NVIDIA returns a fake out-of-memory
  for exportable allocations in some states, so a driver failure cannot be read as
  exhaustion; NVIDIA on Linux has no system-memory fallback, so real exhaustion can take the
  desktop with it; and nothing would return an ended stream's memory.
- **Our own byte ledger beside the allocator.** The allocator already reads the driver's
  budget and its usage includes memory the pools never see — swapchains, codec buffers,
  pipelines. A second ledger would disagree with the driver.
- **90% of the reported budget.** The budget already excludes other processes, and some
  drivers already hold back a margin inside it; a percentage counts that margin twice. A
  small fixed headroom is enough.
- **Heap size as the fallback bound.** Some drivers report a GPU heap as a large share of
  system RAM; the kernel's out-of-memory killer would answer before the engine refused.

## Consequences

- Isolation between streams is by count, not by memory. A stream that grows into the budget
  can still cost others their frames. Per-stream memory budgets would close that and are not
  built.
- A helper process's GPU memory (torch, CUDA, MPS) is invisible to the runtime except as a
  smaller budget. A shrinking budget never takes back a held frame; freeing idle entries is
  the only recovery.
- Freeing needs a per-entry record of last use against queue completion, which the RHI did
  not keep, and must release the surface-share registration and parent caches with the
  memory, or a duplicated fd keeps the memory alive.
- macOS has no Vulkan-visible measure of IOSurface memory, so the bound there is Metal's
  recommended working set against the runtime's own IOSurface tally.
