# zero-copy-data-plane

> **The rest of the zero-copy data plane**, decided by the owner on 2026-11-03
> (§Processor model & scheduling, the data-plane entry, `ARCHITECTURE.md:1323-1334`, and
> `docs/decisions/strict-port-families.md`). The first slice, `linux-pool-slot-import-once`,
> shipped in #3151. This change settles the fan-out OPEN (`ARCHITECTURE.md:1350-1355`): the
> owner chose a borrow quota per link on 2026-11-04. The five port families themselves are the
> next change, `strict-port-families`. This change builds the transport they ride on.

Today a frame crosses a link as copied bytes, and a GPU frame crosses as a name that costs a
socket round trip per frame. On the receive path, iceoryx2 lends one sample at a time
(`CHANNEL_SUBSCRIBER_MAX_BORROWED_SAMPLES = 1`, `iceoryx2/node.rs:47-49`). The mailbox copies it
into a `Vec<u8>` (`mailbox.rs`, `PortMailboxQueuedFrame::payload`), and Python receives a new
`bytes`. On the write path, Python hands over `bytes`, which are copied into a loaned slice
(`output.rs:471-482`). A GPU frame's `VideoFrame` names `<slot>#<generation>` inside msgpack. The
reader then makes a `check_out` and a `release_check_out` round trip per frame. On macOS the
service also sends a fresh IOSurface port per frame, which the helper releases without looking it
up (`macos/mod.rs:534`).

**After this change:**
- **A read borrows the sample in place.** Python receives a pyclass that owns the iceoryx2
  sample and offers the buffer protocol and `__dlpack__`. `bytes(payload)` is the one explicit
  copy. The sample returns to its publisher when the borrow and every export of it are gone.
- **A write loans its destination.** `with ctx.loan(port, byte_len) as out:` hands out a
  writable buffer over the loaned sample, which is published on exit. `write(port, payload)`
  stays as a door for a payload that already exists and is the one copy.
- **A GPU frame crosses as four integers.** Each Image or GPU Array sample carries
  `{pool, slot, generation, timeline_value}` in its user header. The reader claims the frame in
  a shared-memory claim table, with no socket and no port, and reads the slot through the import
  it made once.
- **A slow reader costs only itself.** Each link has its own borrow quota. Past it, that link
  drops and counts its own frames. The producer's pool is sized for its in-flight frames plus
  every link's mailbox depth and quota, so no reader ever starves another and no reader ever copies.
- **What a stream author sees:** reads that never copy, `ctx.loan`, and the same `VideoFrame`,
  with `__dlpack__` and `.cpu()` unchanged.

**Scale gate — this skill plus the existing ADR.** The IPC change is the one
`docs/decisions/strict-port-families.md` records. This change adds no new ADR; it amends that
ADR's Consequences with the claim table's ordering argument (§Claim table below).

## Current state (recon, 2026-11-05, `main` `cb13eed`)

- **The user header** is `DataChannelBagUserHeader { sequence_number, messages_lost_upstream_before_this_one }`
  (`streamlib-ipc-types/src/lib.rs:680-690`), `#[repr(C)]`, `ZeroCopySend`, with its type name
  pinned. It is one type for every channel data service (`iceoryx2/node.rs:236-240`, `:488-517`).
- **`FrameHeader`** (`streamlib-ipc-types/src/lib.rs:488`) sits in front of every payload:
  `[port_key: 64][timestamp_ns: 8][len: 4][flags: 4]`, 80 bytes. It is not `#[repr(C)]`. The
  live receive path routes by subscriber binding, not by `port_key` (`input.rs:1362-1370`);
  only the manual-injection `route` reads it.
- **The mailbox owns copies.** `PortMailboxQueuedFrame { payload: Vec<u8>, … }` sits in a
  crossbeam `ArrayQueue` per port (`mailbox.rs`). Eviction, loss attribution per link, the
  queued-frame measure (audio window) and the group gate (`message_group_loss_recovery_gate.rs`)
  all operate on that entry.
- **Python reads `bytes`** (`link_data_access.rs:37`, `_node_context_protocols.py:285`), and
  `read(port, into=T)` calls `T.from_bytes`. `VideoFrame` is a `MsgpackCast` whose `surface_id`
  names the frame (`tatolab/stream/video_frame.py`). `ClaimedSurfacePixelAccess` takes the
  checkout lease at the cast (`ARCHITECTURE.md:642-662`).
- **Leases** live in `SurfaceCheckOutLeaseRegistry` (`core/context/surface_check_out_lease_registry.rs:113`),
  an in-process `Mutex` table that the surface-share service writes on each check-out. The ring
  rehands a slot only when `PoolSlotReuse::permits` and no in-process holder or platform use
  remains (`lease_aware_pool_slot_ring.rs:107-113`).
- **Pool depth** for a processor output pool is the helper's `rotation_depth`
  (`subprocess_escalate/handle_lifecycle/processor_output_pools_of_one_helper.rs`). Nothing
  sizes a pool by its readers.
- **Shared memory between processes** today is iceoryx2 (samples and the loss-count blackboard,
  `iceoryx2/helper_process_loss_count_board.rs`) plus the surface-share fds and ports. A
  blackboard has one writer per entry, so it cannot carry claims that many processes increment.

## ADDED

- **ADDED (§Processor model & scheduling, the data-plane entry, DECIDED):** The iceoryx2 user
  header is `DataChannelSampleUserHeader`, one `#[repr(C)]` type for every data service:
  `sequence_number: u64`, `messages_lost_upstream_before_this_one: u64`, `timestamp_ns: i64`,
  `payload_byte_len: u32`, `flags: u32` (`starts_group`), then
  `payload_descriptor: DataChannelPayloadDescriptor`. The payload starts at offset 0 of the
  sample, 64-byte aligned (iceoryx2 `payload_alignment(64)`), and carries nothing but payload.
  [zero-copy-data-plane]
- **ADDED (same entry, DECIDED):** `DataChannelPayloadDescriptor` is a fixed-size
  `#[repr(C)]` value: `kind: u32` and a 60-byte body, for 64 bytes in all. In this change the
  kinds are `Bytes` (the payload is the sample's bytes, and the body is zero) and
  `GpuPoolSlotFrame`, whose body is `pool: [u8; 16]` (the pool's UUID),
  `slot: u32`, `generation: u64` and `timeline_value: u64`, with the rest reserved and zero.
  `strict-port-families` adds a kind per family and gives each family's layout its body. A
  descriptor a reader does not know is refused by kind, never guessed at. [zero-copy-data-plane]
- **ADDED (same entry, DECIDED):** A read borrows. `ctx.read(port)` returns a
  `PortPayloadBorrow` that owns the iceoryx2 sample. It offers the buffer protocol (read-only,
  `memoryview(payload)` without a copy) and `__dlpack__` / `__dlpack_device__` for a `Bytes`
  payload as a 1-D `uint8` CPU tensor, and `bytes(payload)` is its one explicit copy. A borrow
  stays counted while any `memoryview` or DLPack capsule over it lives. `read(port, into=T)`
  hands `T.from_bytes` the borrow, which `msgpack.unpack` and every `MsgpackCast` accept as a
  buffer. [zero-copy-data-plane]
- **ADDED (same entry, DECIDED):** A write loans. `with ctx.loan(port, byte_len,
  timestamp_ns=None, starts_group=True) as out:` hands out a writable `PortPayloadLoan` over a
  loaned sample, with the buffer protocol and `__dlpack__`. The sample is published on a clean
  exit and given back unpublished when the block raises. `write(port, payload)` copies an
  existing buffer, and accepts any buffer-protocol object, not only `bytes`. [zero-copy-data-plane]
  A loan is refused at its edges (owner, 2026-11-05). Leaving the block while an export of the
  loan (`memoryview`, DLPack capsule) lives raises `BufferError` and gives the sample back
  unpublished, because a published sample never changes under its readers. A `write` or a
  second `ctx.loan` on a port whose loan is open is refused, naming the open loan, so a port's
  order is its write order and the publisher keeps one loaned sample
  (`CHANNEL_PUBLISHER_MAX_LOANED_SAMPLES = 1`). Rejected: publishing with a live export, and
  several loans per port.
- **ADDED (same entry, DECIDED):** The mailbox queues samples, not copies. Each queued entry
  holds the iceoryx2 `Sample` plus the per-link attribution it carries now. A link's subscriber
  borrows up to `mailbox depth + the link's borrow quota + 1` samples
  (`subscriber_max_borrowed_samples`): the one is the frame being admitted, so a full mailbox
  and a reader at quota can still receive it to drop and count it. Eviction drops the oldest
  sample and counts it against its link, as it does today. A relay's
  network bytes (`RelayTrackSource`) are written into a loaned sample of the input's local
  channel, so every read is a borrow, whatever its origin. [zero-copy-data-plane]
- **ADDED (same entry, DECIDED; settles the fan-out OPEN):** Each link has its own borrow quota:
  the samples, and for a GPU frame the claims, its reader may hold beyond its mailbox. The
  default is 2. A link's quota is stated on the input port (`borrow_quota=`), never on the
  output. Past its quota, a link drops the newest frame and counts it in that link's
  `messages_dropped`, and no other link is affected. Wiring a link grows the pool by
  that link's mailbox depth and quota before the link's first frame, and unwiring it shrinks the pool lazily.
  A queued `GpuPoolSlotFrame` holds no claim until its cast, so the pool also counts each
  link's mailbox: `in-flight frames + Σ (mailbox depth + quota)` over the links on its output,
  and a frame the mailbox accepted is never outwaited by its slot (owner, 2026-11-05).
  Rejected: publish-to-claim transit riding pool depth, refused at the cast as a drop. No
  reader ever copies to escape the quota. Memory grows with fan-out, and that is accepted
  (owner, 2026-11-04). [zero-copy-data-plane]
  Rejected: one quota per output — a slow reader holding slots starves a timely one; and a copy
  past quota — a hidden slow path, which the ADR forbids (owner, 2026-11-04).
- **ADDED (same entry, DECIDED):** A pool's slot set crosses once per processor interpreter.
  The first time a helper reads or writes a frame of a pool, the surface-share service hands it
  the pool's slot set: each slot's memory, its timeline pair for a texture, and the pool's
  claim table. A descriptor naming a slot the reader has not imported makes one slot-set
  request, which is how a slot added by growth crosses; a retired slot is listed
  under `retired_pool_slots` (the Linux rule of `linux-pool-slot-import-once`). macOS adopts the
  same notice and stops sending a port per frame. [zero-copy-data-plane]
- **ADDED (same entry, DECIDED):** Each GPU pool has a claim table in shared memory: a memfd on
  Linux, and on macOS a Mach memory entry, crossing with the slot set. Each slot is one
  64-byte-aligned row: `published_generation: AtomicU64` and one `AtomicU32` claim count per
  claimant column, up to 64 columns. The service assigns a column to each interpreter at
  hand-over and clears it as §failure isolation below states. The protocol is in §Claim
  table, and it replaces the per-frame `check_out` and `release_check_out` for a
  `GpuPoolSlotFrame`. The retired-frame refusal becomes the generation re-check. The checkout
  socket path stays for non-pool surfaces (foreign adoptions, export stagings).
  [zero-copy-data-plane]
- **ADDED (§Processor model, failure isolation, DECIDED):** A dead interpreter's claims die
  with it. When an interpreter's process is reaped, including a helper restart under
  `helper-process-restart`, the engine zeroes its column in every claim table it was handed,
  then re-runs the ring's availability test for those pools. [zero-copy-data-plane]
  A column clears only once its interpreter's process is reaped, never on a connection close
  alone (owner, 2026-11-05). Every connection the service drops — an oversized or malformed
  message included — is a helper fail-stop: the engine kills that helper, reaps it, and then
  zeroes its column, so a column is never reused while its holder can still run. Rejected:
  clearing on connection close, which leaves a live reader on a reused slot.
- **ADDED (§Processor model, DECIDED):** Every `#[repr(C)]` value this change puts in shared
  memory has a layout regression test (size, alignment and every field offset): the user header,
  the descriptor and each kind's body, and the claim table row. Python mirrors none of them. It
  reads them through the wheel. [zero-copy-data-plane]

## Claim table

The protocol, written for the ADR's Consequences. All operations are `SeqCst`. One row:
`G` = `published_generation`, and `C[i]` = claimant `i`'s count. `G`'s top bit is `RETIRING`.

- **Claim frame `g` (reader, column `i`):** `C[i].fetch_add(1)`, then load `G`. If
  `G == g`, the claim holds. Otherwise `C[i].fetch_sub(1)` and refuse it as recycled
  (`G != g`, `RETIRING` included, because a retiring slot's frame is already being replaced).
- **Release:** `C[i].fetch_sub(1)`.
- **Rehand the slot (engine, ring lock held):** read every `C[*]`. If any is non-zero, the slot
  is held. Otherwise store `G = g | RETIRING`, then read every `C[*]` again. If one is non-zero,
  a reader's claim landed between the two reads: store `G = g` back and treat the slot as held.
  That reader saw `G == g` and holds a valid claim, and no reader that saw `RETIRING` keeps one.
  If all are zero, the slot is free. Write the slot, then publish with `G = g + 1`.
- **Why this is sound:** under `SeqCst` there is one total order. If the reader's `fetch_add`
  precedes the engine's second read, the engine sees the claim and backs off. If it follows,
  the reader's load of `G` follows the engine's `RETIRING` store, so the reader refuses. A
  reader that refuses during a back-off loses a frame it was in time for. That is a count, never
  wrong pixels, and it costs one frame only when a claim races a rehand of the same slot.
- **Crash:** a claimant that dies between `fetch_add` and `fetch_sub` leaves `C[i] > 0`. Its
  column is zeroed when its holder is gone (see §failure isolation), which is the only time a
  column is reused.
- **GPU ordering:** a claim says nothing about GPU work. The reader waits on the slot's
  `produce_done` timeline at `timeline_value` before it reads. It releases its claim only once
  its own GPU reads of the slot have completed, signalling `consume_done` host-side at that
  release as `HelperCheckedOutTextureSurface` does now (`linux/texture.rs`). A zero claim count
  therefore means no reader's GPU work still reads the slot, and the rehand needs no wait on a
  value a peer signals (the macOS timeline entry's rule).

## MODIFIED

- **MODIFIED (§Processor model, the data-plane entry, `:1323-1334`):** the entry gains its
  change tag, `[zero-copy-data-plane]`, and "borrows bounded per port" becomes "borrows bounded
  per link".
- **MODIFIED (§Processor model, the fan-out OPEN, `:1350-1355`):** replaced by the DECIDED
  quota entry above.
- **MODIFIED (§Processor model, the Mach transport entry, `:1409-1445`):** the per-frame port
  hand-off ends. The helper's per-slot IOSurface cache is filled by the slot-set hand-over, and
  `HelperIOSurfaceUseCountClaim` stays as the platform-use half of a claim. A macOS reader both
  claims in the table and raises the use count, because the ring also asks the kernel
  (`is_in_use_per_the_platform`). The cache follows the Linux rule: it drops a slot listed in
  `retired_pool_slots` and empties when its connection is replaced. That closes two gaps in
  today's one-time clear on service death: a failed death watch leaves the cache in place, and
  an import inserted after the clear survives it.
- **MODIFIED (§Processor model, the surface-id entry, `:642-662`):** a published frame is still
  `<slot>#<generation>` by name, for logs, previews and the module host. On the data plane it
  travels as the descriptor, and the claim is taken where the cast is taken now, at
  `ClaimedSurfacePixelAccess`. Publish-to-claim transit no longer rides pool depth alone: the pool
  counts every link's mailbox depth, so a queued frame is not refused as recycled.
- **MODIFIED (engine code):**
  - `streamlib-ipc-types`: `DataChannelBagUserHeader` becomes `DataChannelSampleUserHeader`,
    with `DataChannelPayloadDescriptor` and its bodies, and the layout tests.
  - `iceoryx2/node.rs`: `payload_alignment(64)`, and `subscriber_max_borrowed_samples` per link
    from its mailbox depth and quota.
  - `iceoryx2/output.rs`: a loan door that the Rust `LinkOutputDataWriter` and Python's
    `ctx.loan` both call. The copy-in becomes the `write` convenience over it.
  - `iceoryx2/mailbox.rs`, `input.rs`: queue `Sample`s, and hand a read the sample.
  - `core/context`: `GpuPoolClaimTable` beside `SurfaceCheckOutLeaseRegistry`. `PoolSlotReuse`
    consults both, and the ring sizes a pool from its links' mailbox depths and quotas.
  - `linux/surface_share`, `apple/surface_share`: the slot-set hand-over, the claim table's
    handle, column assignment and clearing, and the macOS `retired_pool_slots` notice.
- **MODIFIED (helper code, `sdk/streamlib-python-wheel/`):** `PortPayloadBorrow` and
  `PortPayloadLoan` pyclasses. `read*` returns a borrow, `write` takes any buffer, and
  `ctx.loan` is added. The GPU path claims in the table and reads through the per-slot import.
  `.pyi` entries and Protocols are added for both pyclasses (`_node_context_protocols.py`), and
  the conformance gate covers them.
- **MODIFIED (`sdk/tatolab-stream`):** `read` is typed `PortPayloadBorrow | None`, and
  `MsgpackCast.from_bytes` accepts a buffer. `VideoFrame` and `NodeOutputTextureRing` read and
  write the descriptor instead of a `surface_id` key for a pooled frame.

## REMOVED

- REMOVED: FRAME_HEADER_SIZE
- REMOVED: DataChannelBagUserHeader
- REMOVED: CHANNEL_SUBSCRIBER_MAX_BORROWED_SAMPLES
- REMOVED: push_frame_without_inbound_link_attribution
  The manual-injection path writes a loaned sample like every other producer.

## Decided — `read` stops returning `bytes` in this change (owner, 2026-11-04)

Reads borrow in this change, so `read`'s public type goes from `bytes` to `PortPayloadBorrow`
here, and `strict-port-families` then narrows it per family. A caller that hands the result to
`msgpack`, `numpy.frombuffer`, `memoryview` or `torch.frombuffer` keeps working. A caller that
relies on `bytes` itself breaks once. A converted consumer that breaks gets backlog filed.
Rejected: keeping `bytes` until the families by copying out at the door, which would build a
copy-out door only to delete it one change later (owner, 2026-11-04).

## Proof

- Layout regression tests for `DataChannelSampleUserHeader`, `DataChannelPayloadDescriptor`, each
  body and the claim table row: size, alignment and offsets.
- Claim table: a loom model of the claim and rehand protocol (one reader, one engine; two
  readers, one engine) that proves no interleaving leaves a reader holding a claim on a slot
  the engine rehanded. A crash-reclaim test kills a helper mid-claim and shows the slot is
  rehanded once its column clears.
- Engine: a Rust reader holds a borrowed sample while the publisher keeps sending, and the
  borrow's bytes stay unchanged. A link past its quota drops only its own frames while a second
  link on the same output receives every frame.
- Wheel: `memoryview(ctx.read(port))` shares memory with the sample (the sample's address is
  visible through the buffer); `ctx.loan` publishes on exit and gives the sample back on raise.
  A `VideoFrame` read on Linux and macOS makes no surface-share request after the slot set has
  crossed.
- Rig (booked through the coordinator): per-hop copy count and time to first byte for a 1080p
  frame and a 4 MB encoded payload, Python → Python, before and after.

## Tickets (expected)

Three tracer bullets, in order:
1. **Header and borrowed reads:** the user header, the descriptor, 64-byte alignment,
   sample-queuing mailbox, `PortPayloadBorrow`, loaned writes, per-link quota and drop counting.
   CPU payloads only.
2. **Claim table on Linux:** slot-set hand-over, `GpuPoolClaimTable`, the descriptor for pooled
   GPU frames, pool sizing by quota, crash reclaim, and the loom model.
3. **macOS:** the claim table over a Mach memory entry, slot-set hand-over, the end of the
   per-frame port, and `retired_pool_slots` on Mach, with the helper cache emptied on a
   replaced connection rather than by the death watch.

## Notes (not in scope)

- The OPEN "Zero-copy per-frame consumption by a foreign GPU stack" (`:637-641`) is the same
  slot set and claim table offered to a foreign importer. It stays OPEN.
- Previews (`ARCHITECTURE.md:3495`) read through a borrow and so cost nothing extra. They are
  built with the families.
