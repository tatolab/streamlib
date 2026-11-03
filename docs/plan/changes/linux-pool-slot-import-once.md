# linux-pool-slot-import-once

> **The first slice of the zero-copy data plane**, which the owner decided on 2026-11-03
> (§Processor model, the data-plane entry, `ARCHITECTURE.md:1301-1312`): "The Linux
> import-once ships first and on its own." The shared-memory claim table, the
> `{pool, slot, generation, timeline_value}` descriptor and the borrowed and loaned doors
> are later changes of the same milestone and are not in this one.

Today a Python processor on Linux pays for every GPU frame it touches as if it had never
seen the slot before. Each frame is a `check_out` over the surface-share socket, the
service duplicating every stored fd, an `SCM_RIGHTS` hand-over, the helper duplicating
them again, a Vulkan memory import (and for a texture, an image rebuild and a timeline
pair joined), then a `release_check_out` round trip. A pool has a handful of slots, so
almost all of that work repeats an import the helper already did a few frames earlier.
macOS already caches one import per pool slot; Linux has no cache.

**After this change:**
- **A helper imports each pool slot once.** The first frame over a slot imports it as
  today; every later frame over the same slot reuses that import, and the service sends
  no fds for it. A frame still costs one `check_out` and one `release_check_out`, which
  is what keeps the checkout lease and the retired-frame refusal exactly as they are.
- **A slot the engine retires leaves the helper too.** When a pool slot's registration
  goes away, the helper drops its import of it, so a stream that rebuilds a pool (a
  resolution change) does not grow the helper's GPU memory.
- **Pixel buffers, textures and storage buffers alike** — every pooled surface a helper
  checks out, which is what an Image and a GPU Array will cross as. A surface that is
  not a pool slot (a foreign DMA-BUF the helper adopted, a one-off export) is imported
  per check-out as today and never cached.
- **What a stream author sees:** nothing but speed. No port, Protocol, config key or
  Python call changes.

**Scale gate — this skill, no new ADR.** The IPC change is the one
`docs/decisions/strict-port-families.md` records; this is its first step.

**Precondition.** The surface-id lifetime contract (`ARCHITECTURE.md:621-640`, DECIDED and
SHIPPED), the checkout lease and retired-frame refusal (`surface_share_wire_verbs.rs:110-153`)
and the macOS per-slot cache (`ARCHITECTURE.md:1399-1401`, SHIPPED) are in place.

## Current state (recon, 2026-11-03, `main` `9bb063e`)

- **The helper's door is one function.** `check_out_and_import`
  (`sdk/streamlib-python-wheel/src/python_helper_process_pixel_exchange/mod.rs:800-807`)
  sends `check_out` and hands the reply to the platform's `import_checked_out_surface`. Its
  callers are `resolve_surface` (`:770-776`, a published frame), the output
  `acquire_pixel_buffer` path (`:754`), foreign DMA-BUF adoption (`linux/foreign_dma_buf.rs:200`)
  and the export stagings (`linux/export_staging.rs:218`, `:273`, `:695`).
- **Linux imports every frame.** `import_checked_out_surface` (`linux/mod.rs:458-616`) splits
  off the timeline fds (`SurfaceShareCheckOutFds`), then per resource type: a pixel buffer is
  `duplicate_plane_fds_for_import` plus `ConsumerVulkanBuffer::from_dma_buf_fds`, or
  `duplicate_memory_fd` plus `from_opaque_fd_at_stated_memory_type_index`; a texture is
  `import_checked_out_texture` (`linux/texture.rs:420`), which rebuilds the image and joins
  the timeline pair; a storage buffer is `import_checked_out_storage_buffer`
  (`linux/storage_buffer.rs:102`). The result owns its `ConsumerVulkanBuffer` outright
  (`mod.rs:324-325`), so nothing outlives the frame.
- **macOS caches per slot.** `iosurface_pool_slot_import_for` (`macos/mod.rs:534-575`) keys
  `iosurface_imports_by_pool_slot` by `pool_slot_key_of_surface_id` and releases the fresh
  port unlooked-up on a hit; each frame raises the IOSurface use count
  (`HelperIOSurfaceUseCountClaim`) while held, and the cache empties when the service goes
  away (`macos/mod.rs:316-375`). The service still sends a port every frame. Pixel buffers
  only; a macOS texture imports per frame. `pool_slot_key_of_surface_id` returns a
  non-pool id whole (`core/rhi/pixel_buffer_pool.rs:123-126`), so a foreign surface is
  cached too, and never evicted.
- **The service dups and sends on every check-out.** `handle_check_out`
  (`runtime/streamlib-engine/src/linux/surface_share/unix_socket_service.rs:635-662`) is
  `handle_lookup` (`:675-810`) — the retired-frame refusal, then an `F_DUPFD_CLOEXEC` of every
  plane fd and of `produce_done` and `consume_done`, then the registration's metadata,
  `current_image_layout` included — plus `record_check_out_lease_or_refusal`. Leases are held
  per connection and released when it closes.
- **A cached import cannot pin a Linux slot.** The ring rehands a slot when no lease holds it
  and no holder in the engine has it (`core/context/lease_aware_pool_slot_ring.rs:107-113`);
  `is_in_use_per_the_platform` is overridden only on macOS (`core/context/gpu_context.rs:105`).
  A helper's imported memory is invisible to the ring, as the plan wants: the lease is the claim.
- **Slot keys are never reused.** A pool slot is registered under a fresh UUID
  (`core/context/gpu_context.rs:3080`, `:8247`, `:8320`), and a frame id is `<slot>#<generation>`
  (`pixel_buffer_pool.rs:104-121`), so a cached slot can be retired but never aliased by a
  different allocation.
- **Nothing tells a helper a slot is gone.** `release_surface` (`linux/surface_share/state.rs:466`)
  closes the table's fds; a helper holding an import keeps that memory alive until it exits.

## ADDED

- **ADDED (§Processor model & scheduling, the data-plane entry, DECIDED):** On Linux a helper
  imports each pool slot once. It keeps one import per pool slot key — the memory, and for a
  texture the rebuilt image and its joined timeline pair — and a `check_out` of a published
  frame id whose slot it holds says so (`holds_pool_slot_import: true`); the service then
  records the lease and answers with the frame's metadata, `current_image_layout` included,
  and no fds. A frame still takes and releases its checkout lease exactly as today, and the
  retired-frame refusal answers before anything else, so a recycled frame is refused by name
  whether or not the slot is cached. Only a published frame id (`<slot>#<generation>`) is
  cached; any other surface is imported per check-out and dropped with it.
  [linux-pool-slot-import-once]
- **ADDED (§Processor model & scheduling, the data-plane entry, DECIDED):** A helper drops a
  slot's import when the engine unregisters the slot, and drops every import when its
  connection to the service closes. How it learns of the unregistration is the decision below.
  [linux-pool-slot-import-once]
- **ADDED (§Processor model, failure isolation):** a cached import is never a claim. It pins
  nothing on the engine's side, a dead helper's imports die with its process, and the engine
  rehands a slot by its leases alone, as it does now. [linux-pool-slot-import-once]

## MODIFIED

- **MODIFIED (§Processor model, the Mach transport entry, `:1399-1401`):** "the helper caches
  one import per pool slot" is the macOS half of a rule both platforms now follow; the Linux
  half is the entry above. The macOS cache keeps its shape in this change.
- **MODIFIED (§Processor model, the data-plane entry, `:1301-1312`):** "The Linux import-once
  ships first and on its own" gains its change tag, `[linux-pool-slot-import-once]`.
- **MODIFIED (engine code):**
  - `unix_socket_service.rs` `handle_check_out` reads `holds_pool_slot_import`; when true and
    the id is a published frame id, it takes the lease and answers the metadata without
    duplicating or sending an fd. `handle_lookup` splits into the metadata answer and the fd
    duplication, so the two paths share one metadata builder.
  - The connection loop (`handle_client_connection`, `:220`) keeps the pool slot keys it has
    handed memory for, for the retirement notice below.
- **MODIFIED (helper code, `sdk/streamlib-python-wheel/src/python_helper_process_pixel_exchange/`):**
  - `HelperProcessGpuExchangeClient` gains a Linux per-slot import cache beside the macOS
    one, keyed by `pool_slot_key_of_surface_id`, holding each slot's import behind an `Arc`.
  - `check_out_surface` takes the slot's import out of the cache (a clone of its `Arc`)
    before it sends `holds_pool_slot_import: true`, so an eviction during the request cannot
    leave a fd-free reply with nothing to read: the frame uses the import it took, and the
    eviction frees it when that frame drops. No path retries a check-out, so a frame never
    holds two leases.
  - `HelperCheckedOutPixelSurface` (`mod.rs:324-325`) holds its Linux buffer as the slot's
    shared import, as the macOS arm holds `iosurface_pool_slot_import`; the texture and
    storage-buffer arms do the same.
  - The cache empties when the surface-share connection closes, as the macOS cache does when
    the service goes away.

## REMOVED

Nothing; the per-frame import path stays as the first touch of a slot and for every surface
that is not a pool slot.

## [NEEDS DECISION] How a helper learns a slot was retired

A pool is rebuilt when its frames change size, so a long stream retires slots; a helper that
never hears of it keeps their memory. The Linux socket is request-and-reply, with no push.

- **A. The service tells it on the next reply (recommended).** The connection remembers which
  slot keys it has handed memory for; when one is unregistered, the next reply on that
  connection carries it in `retired_pool_slots`, and the helper drops those imports. Exact,
  costs nothing per frame, and a helper that has stopped asking holds nothing new while it is
  idle. A notice is a hint: one for a slot the helper no longer holds is ignored.
- **B. The helper bounds its own cache.** Keep at most a fixed number of slots and evict the
  least recently used. No wire change for retirement, but the bound is a guess: too small and a
  deep pool re-imports every frame, too large and a rebuilt pool's memory lingers.
- **C. Never evict, as macOS does today.** The smallest change, and it leaks one pool's memory
  per rebuild for the life of the helper.

Recommendation: **A**. If it is chosen, macOS adopts the same notice when its cache next
changes, so the two platforms keep one rule; that is not in this change.

## Proof

- An engine test against the socket service: two check-outs of two generations of one slot on
  one connection, the second with `holds_pool_slot_import`, take two leases and receive fds once;
  a recycled generation is refused with and without the flag; unregistering the slot puts it in
  the next reply's `retired_pool_slots` (if A is chosen).
- A helper test beside `linux/opaque_fd_pixel_buffer_import_tests.rs`, which runs on NVIDIA
  and on lavapipe and skips where no device comes up: five frames over a two-slot pool import
  twice, every frame's pixels read back right, and the cache is empty after the connection
  closes.
- A before-and-after number on the Linux rig: a Python processor's per-frame
  check-out-to-release time at 1080p, recorded in the PR. The rig run is booked through the
  coordinator, never started unasked.

## Tickets (expected)

One tracer bullet: the service flag and metadata split, the helper cache across the three
resource types, the retirement path the decision picks, and the proofs above. The claim table
that removes the per-frame round trip altogether is the next change of the milestone.

## Notes (not in scope)

- macOS still sends a fresh IOSurface port every frame and releases it unlooked-up; the same
  request flag would end that. It belongs to the macOS change of this milestone.
- macOS caches a non-pool surface under its whole id and never evicts it; this change's
  "only a published frame id is cached" rule would fix that there too.
