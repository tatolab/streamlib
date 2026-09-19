# loss-visibility

Every bag lost on a local link is counted where `graph` can read it. After this change:
- a bag the iceoryx2 ring overwrites on an `ordered` port is counted on its inbound link;
- a `newest` port counts nothing, as the plan already says;
- a Python processor's node renders `metrics` like a native one's, and those counts survive a
  crashed helper;
- a windowed audio port counts the samples its flush discards, sits behind a 64-bag ring, and
  refuses a live connect onto a shallower channel by name.

The change implements the `[loss-visibility]` entries in `docs/plan/ARCHITECTURE.md`:
- §Processor model `:532-543` (ring overwrites) and `:544-552` (helper counts), which make
  `:511-527`'s "no loss is silent" true in the tree;
- §Media I/O `:1500-1518`: the flush count, the ring cap and the live refusal.

The OPEN at `:1523-1526`, wiring a windowed port at the channel's depth once overwrite counting
ships, is left untouched; this change is what makes it answerable. Owner align 2026-09-14
(PR #2258). Rationale lives in `docs/decisions/loss-visibility.md`, which this proposal amends.

**Scale gate: this skill, plus an ADR.**
- **IPC wire format:** every data sample gains an 8-byte user header, a helper gets a blackboard
  service, and the wiring envelope gains a slot per inbound link.
- **Processor model:** what counts as a drop, and where.
- **Python API public contract:** none. `graph` gains keys, and no Python signature or stub
  changes.

**Precondition.** Every entry above is DECIDED. The change builds on `local-transport-hardening`,
already ticketed:
- #2263 (M2) makes one creation-depth function that this change raises for a windowed
  destination, gives each subscriber its own port's ring and holds the service factories;
- #2265 (M3) gives `wire_link` a reply;
- #2264 (S1) detects a helper's death by the process;
- #2262 (M4) refuses a helper built from another engine, which is what makes a wire change safe
  to land in one PR.

**Verified against the tree 2026-09-14 (HEAD f9f8e3bf8).** Three read-only recon sweeps re-checked
the audit's findings (`~/Documents/streamlib-iceoryx2-deep-dive/`, GAP-1, GAP-2, BUG-6, BUG-8,
HYG-5). Paths are under `runtime/streamlib-engine/src/` unless rooted.

**Ring overwrites** (iceoryx2 0.9.3 from crates.io, `Cargo.lock:1644-1647`)
- A send reports an overwritten subscriber as reached, and no iceoryx2 API reports an overwrite.
  The audit reproduced this: ten sends into a four-slot ring, `send()` returned 1 each time, and
  the subscriber received bags 6 to 9.
- User headers are supported: `.user_header::<M: Debug + ZeroCopySend + Default>()`
  (`iceoryx2-0.9.3/src/service/builder/publish_subscribe.rs:334`),
  `SampleMutUninit::user_header_mut()` (`sample_mut_uninit.rs:208`) and `Sample::user_header()`
  (`sample.rs:127`).
  - An opener with another header type fails `IncompatibleTypes` (`publish_subscribe.rs:315-325`).
  - The type name defaults to `core::any::type_name`, module path included
    (`zero_copy_send.rs:40-42`).
- Every received sample carries `origin()`, a `UniquePublisherId` that is `Eq + Hash + Copy`
  (`sample.rs:137`, `identifiers.rs:26-39`).
- There is one publisher per channel (`streamlib-ipc-types/src/lib.rs:249`, applied at
  `iceoryx2/node.rs:123`).
- Data services are opened by 4 library callers, all through `Iceoryx2Node::open_or_create_service`
  (`iceoryx2/node.rs:120-128`): `open_iceoryx2_service_op.rs:144`, `core/runtime/tap.rs:236`, and
  `sdk/streamlib-python-wheel/src/python_processor_link_data_access.rs:288` and `:393`.
  - 19 test and bench sites use the wrapper.
  - 10 bypass it with a raw `.publish_subscribe::<[u8]>()`: `benches/output_writer_ffi_hop.rs:85`,
    `iceoryx2/channel_sizing_tests.rs:51`, `iceoryx2/input.rs:1505,3028`, and
    `iceoryx2/output.rs:561,634,773,864,1002,1128`.
- **One send seam**, `OutputWriterInner::write_raw` (`iceoryx2/output.rs:281-361`), which the
  helper reuses. Its order: the ceiling check (`:300-321`), the port-key header (`:325-328`), the
  loan (`:330-333`), then `send()` (`:342-344`).
- `SendError` (`iceoryx2-0.9.3/src/port/mod.rs:234-247`):
  - `ConnectionBrokenSinceSenderNoLongerExists` and `ConnectionError` fail before any delivery
    (`port/publisher.rs:310-316`);
  - `UnableToDeliver`, `ConnectionCorrupted` and `InternalError` can follow a partial delivery
    (`port/details/sender.rs:369-409`).
- **One receive seam**, `InputMailboxesInner::receive_pending` (`iceoryx2/input.rs:838-888`),
  driven lazily from reads and `has_data`, with no receive thread. The helper reuses it.
  - `PortBoundSubscriber` (`:85-102`) already holds `link_id`, `inbound_link_name` and the
    link's `InboundLinkDroppedBagCounter`.
  - The port's read mode is in hand at `:852-853`.
- **BUG-6 is live.** `push_frame` records every eviction whatever the read mode
  (`iceoryx2/mailbox.rs:192-215`). The verify test `passing_over_bags_to_reach_the_newest_is_not_a_drop_at_the_port`
  (`:428-446`) pushes 4 bags into a depth-4 mailbox, so it never reaches the eviction branch.
- **Two receive-side losses warn and count nothing:** an undersized frame (`input.rs:844-851`) and
  a frame for a vanished mailbox (`:872-877`).
- A count resets on disconnect: `remove_channel_link` calls `forget_inbound_link`
  (`input.rs:762-773`).
- The frame header is 76 bytes (`streamlib-ipc-types/src/lib.rs:282`). Tap forwards
  `sample.payload()` alone (`core/runtime/tap.rs:279`), and no client outside the engine opens
  iceoryx2.

**Helper counts**
- `graph` renders `{"frames_dropped": n, "dropped_bags_by_link": {link_id: n}}` under
  `metrics` (`core/graph/components/processor_metrics.rs:49-60`).
  - It is attached only by `wire_rust_dest` (`open_iceoryx2_service_op.rs:854`, `:873-891`).
  - `wire_subprocess_dest` (`:974-1047`) attaches nothing, and the test at `:1307-1343` locks
    that in.
- The helper builds its own `InputMailboxesInner` (`python_processor_link_data_access.rs:237-248`),
  so it already counts evictions. Nothing reads them.
- A helper's over-ceiling write logs at `debug!` and returns `Ok` (`:562-570`). The helper
  installs no tracing subscriber, so the line reaches nothing, and the docstring's
  "refused-and-counted" (`:547-548`) is false.
  - The ceiling is per channel, not per link (`iceoryx2/output.rs:85-94`).
  - `refused_over_ceiling_count` (`:208`) has only test callers.
- **Blackboard keys are fixed at creation**, and an unknown key returns `EntryDoesNotExist`
  (`iceoryx2-0.9.3/src/service/builder/blackboard.rs:528-566,681-684`;
  `port/writer.rs:326-330`).
  - A board has one writer (`static_config/blackboard.rs:56`).
  - A dead writer's entries return `HandleAlreadyExists` forever (`port/writer.rs:407-415`).
  - `Reader` is not `Send` for `ipc::Service` (`service/ipc.rs:58-59`). Nothing in the tree uses a
    blackboard yet.
- `graph` serialises under the graph write lock on a tokio worker
  (`operations_runtime.rs:481-483`, `compiler.rs:58`), so a component must be `Send + Sync`. The
  engine already wraps a `!Send` port at `iceoryx2/input.rs:115` and `output.rs:149`.
- Links reach a helper in the setup envelope and live through fire-and-forget `wire_link` and
  `unwire_link` (`sdk/streamlib-python-wheel/src/python_helper_process_spawn_host.rs:628-671`).
  There is no slot index. The cap of 8, becoming 256 under M2, counts every inbound link of a
  processor (`open_iceoryx2_service_op.rs:474`). No respawn path exists.

**Windowed audio**
- The one flush is `flush(why)` (`iceoryx2/audio_window/audio_window_accumulator.rs:627-645`).
  - It logs `info!` with `port`, `discarded_output_frames` and `discarded_source_frames`, and
    counts nothing ("not counted as one", `:624-626`).
  - It is called on a format change (`:306-311`) and on a timestamp gap (`:316-323`).
  - The accumulator holds only `port_name` (`:203`). `PortMailboxDeliveredBag` keeps
    `inbound_link_name` and drops the counter (`mailbox.rs:62-76`).
- The mailbox is sized `min(ceil(window/125 µs)+4, 8192)`, floored at 16
  (`iceoryx2/audio_window/resolved_audio_window_contract.rs:71-87,217-227`). The ring stays at the
  profile's 16 (`open_iceoryx2_service_op.rs:430-441`; `node.rs:185-193`; wheel `:381-402`).
- The op cannot see an existing channel's creation depth. `Iceoryx2Service::max_queued_messages()`
  returns the requested depth (`node.rs:144-147`). M2's held factory is where it becomes readable.
- The neighbouring refusal for a second inbound link into a windowed port is at
  `open_iceoryx2_service_op.rs:609-647`.

---

## MODIFIED: §Processor model `:532-543` — the readings the tree will build

The entry is DECIDED. Each point reads it where an implementer would otherwise choose inline;
none adds a mechanism.

1. **"Per-channel" is per producing publisher.** Each publisher numbers its own sends from zero,
   and each subscriber keeps its last number keyed by `UniquePublisherId`. A publisher recreated
   when a source port's last link goes starts a new baseline, and so will a future respawned
   helper.
2. **"Fails before delivering to anyone" names two variants.** `ConnectionBrokenSinceSenderNoLongerExists`
   and `ConnectionError` consume no number. The other three consume one, because iceoryx2
   does not report partial delivery. A subscriber the failed send never reached therefore reads a
   gap, which is a real loss to it.
3. **The header is one engine type.** It is `#[repr(C)] DataChannelBagSequenceNumberUserHeader {
   sequence_number: u64 }` in `streamlib-ipc-types`, with its iceoryx2 type name pinned so a move
   never changes its identity.
   - Every data service opens through the wrapper, and the 10 raw test sites migrate.
   - A source-walking gate refuses a data `publish_subscribe` builder outside `iceoryx2/node.rs`,
     registered in `ALL_SOURCE_WALKING_GATES`.
4. **Gaps and evictions never double-count.** A ring gap is a bag never received. A mailbox
   eviction is a bag received and then displaced. Each lands on the link's one
   `InboundLinkDroppedBagCounter`.
5. **`newest` counts neither.** BUG-6 is fixed here: a `SkipToLatest` mailbox's eviction is not a
   drop, and the verify test pushes past capacity. This is the tree catching up to `:519-521`.
6. **A bag dropped at the receive seam is counted on its link.** An undersized frame and a frame
   for a vanished mailbox each consume a number and reach no reader, so no gap can show them.
   `:511`'s "counted by the port that dropped it" covers them.
7. **Counts land on the next receive, for a native and a Python consumer alike.** A consumer inside a long `process()`
   receives nothing, so its overwrites reach `graph` when it next reads. This is the stated
   residual's neighbour, and no timer is added.

## MODIFIED: §Processor model `:544-552` — the readings the tree will build

1. **An entry is a slot carrying its wiring.** Board keys are fixed at spawn and links arrive
   live, so the board declares `MAX_INBOUND_LINKS_PER_DESTINATION` slots.
   - The parent assigns each inbound link a slot and a wiring generation, carried in its setup
     envelope entry or its `wire_link`.
   - Each value is `#[repr(C)] { wiring_generation: u64, dropped_bags: u64, discarded_samples: u64 }`.
   - The parent renders a slot only while its generation matches the link it assigned, so a late
     write for an unwired link is ignored and a reused slot starts from zero. That is `:514-517`'s
     "a count is cumulative for the life of one wiring".
   - Size: 256 slots cost about 14 KiB of payload and about 100 KiB of management per helper
     (iceoryx2-0.9.3 `UnrestrictedAtomic` layout, estimated rather than measured).
2. **The board is named per spawn, never per processor.** The parent creates it before the child
   starts and holds its creator factory and reader. It drops them at processor removal, never at
   helper death, so a crashed helper's last counts render until the node goes. No respawn exists
   today; the name keeps the first one free of any board change.
3. **The helper writes at the seam that moves the count.** A count is mirrored into its slot as
   the counter increments, on the helper's own receive, with no loop flush and no timer. Entry
   handles are `Send + Sync` (`port/writer.rs:382-395`).
4. **The reader is wrapped for `graph`**, the `input.rs:115` pattern, and read lock-free under
   the graph lock. `graph` never waits on the child.
5. **The flush count rides the same slot** (`discarded_samples`). One board serves both entries,
   because a windowed port on a helper is where both losses happen together.

**DECIDED (owner, 2026-09-14) — a write refused at the ceiling is counted on the producer, per
output port.** The ceiling is per channel, and the refusal happens before the bag reaches any link,
so it consumes no sequence number and no destination can see it.
- One counter at `write_raw` serves native and Python producers alike, so a Rust author's `Err`
  is counted too.
- `graph` renders `refused_bags_by_output_port: {port: n}` on the producing node.
- A helper's count reaches `graph` through a per-output-port section of its board, fixed at spawn
  from the descriptor.
- This counts the loss where it happens. The cost is stated: a reader looking only at a
  destination's link does not see it.
- Rejected: adding the refusals into each outbound link's `dropped_bags_by_link` at its
  destination. That needs per-link baselines at the producer and a merge of two processes'
  counters at render.

## MODIFIED: §Media I/O `:1500-1518` — the readings the tree will build

1. **The ring cap is 64 bags.** M2's creation-depth function returns
   `WINDOWED_PORT_SUBSCRIBER_RING_DEPTH` (64) when any destination of the channel being created
   is windowed, and 16 otherwise.
   - A windowed port's subscriber ring is 64, carried as the envelope's port depth.
   - Its mailbox depth stays sized from the contract, and neither is fed to the other.
   - Cost, from the ADR: 1.45 MiB of publisher heap against 141 MiB at contract depth.
2. **The live refusal reads the held factory.** A windowed destination wired onto a channel whose
   held factory states a creation depth below 64 is refused at wire time. The refusal names the
   port, the link, both depths, and the fix of connecting the windowed consumer before the channel's
   other links. It sits beside the second-link refusal (`open_iceoryx2_service_op.rs:609-647`).
   Wiring at that depth instead stays the OPEN at `:1523`.
3. **A flush counts what it discards, in the samples the consumer would have received.** The unit
   is per-channel samples at the port's declared rate, `AudioBlock.sample_count`'s unit. It is the
   output remainder plus the staged source frames scaled by the rate ratio, rounded down. A windowed
   port has exactly one inbound link, so the count lands on that link.
   - `graph` renders `discarded_samples_by_link: {link_id: n}` beside `dropped_bags_by_link`, only
     for links into windowed ports. A link into an unwindowed port carries no sample count rather
     than a zero, as a port that declared nothing renders no `audio_window` key.
   - `frames_dropped` stays a bag total.
   - The log names the port, the link and the count at `warn`.
   - Both flush callers count: the format change and the gap.
4. **Three discards stay uncounted.**
   - Resampler priming output is filter delay, not input.
   - The remainder parked when a port's last link disconnects is `:1464-1467`'s designed stop.
   - A bag `accept` refuses already fails the read by name.
5. **A helper's flush log reaches no log** until the audit's unfiled helper-diagnostics bug (B6)
   routes a helper's Rust `tracing`. The board count is what keeps that loss visible.

## Assumptions stated, not asked

- **Mesh hop losses** are the runtime-mesh change's. The ADR's sequence-in-mesh-metadata sentence
  is untouched.
- **Tap is unchanged.** Its subscriber inherits the header type through the wrapper, and its
  `dropped_bags` is its own.
- **No new Python surface.** A Python processor reads no count, and pyright and stubtest see no
  change.
- **The frame header does not move.** `frame_header_size_matches_constant` and
  `frame_header_fields_sit_at_their_documented_wire_offsets` stay green untouched.

## Expected slices

`/derive-tickets` decides the breakdown. The shape the recon supports:

| # | Slice | Blocked by | Proof |
|---|---|---|---|
| L1 | Sequence user header on every data service; per-publisher gaps on `ordered` ports; send-error reading; BUG-6; receive-seam drops counted; raw-site migration + gate; the ceiling count per output port | #2263 | CI: ten sends into a depth-4 `ordered` ring count 6 on the link, and the same overrun on a `newest` port counts 0 (ring and mailbox, past capacity); a replacement publisher on a live subscriber reads no gap; a ceiling refusal consumes no number and renders under `refused_bags_by_output_port`; an opener without the header is refused naming the type; frame-header tests untouched; gate green |
| L2 | Ring cap 64 through the creation-depth function; flush counted and rendered; live refusal | #2263 | CI: a channel created for a windowed destination states depth 64; a gap flush renders its discarded samples on the link and logs them; a windowed connect onto a running 16-deep channel is refused naming both depths; 64+ stalled blocks count ring gaps once L1 is in |
| L3 | Per-spawn board, slots + generations in the envelope and `wire_link`, parent render, helper `metrics` | L1, L2, #2265, #2264 | CI (GPU-free pytest): an overrun helper `ordered` destination renders its drops; SIGKILL leaves the last counts in `graph`; unwire then reconnect reuses a slot from zero and ignores a stale write; a helper windowed flush renders its samples; a helper's over-ceiling write renders on its output port |

Every new engine test is named in `.github/workflows/test.yml`'s slice and in the
`run_local_ci_gates` mirror (`xtask/src/main.rs:204`), or it runs nowhere.

## Records the implementation owes, in the same PRs

These are statements the change makes false; each is a record, not a question:
- The flush doc "not a bag, and not counted as one" (`audio_window_accumulator.rs:624-626`).
- `publish_dropped_bag_counts_on_destination_node`'s doc (`open_iceoryx2_service_op.rs:867-871`).
- The wheel's over-ceiling arm and docstring (`python_processor_link_data_access.rs:547-548`,
  `:564-570`): "refused-and-counted" becomes true, and the arm stops being silent.
- The renamed test in `test.yml:274` and `xtask/src/main.rs:369`.
- At `/ship-change`:
  - `ARCHITECTURE.md:521-527`'s "states the intent, not yet the tree";
  - the verify tag at `:531`, which proves nothing about eviction;
  - `system.mmd:50`'s "(decided, not yet built)".

## REMOVED

Helper counts (L3):
- REMOVED: a_helper_placed_destinations_node_carries_no_metrics_rather_than_a_zero
  It locks the behaviour L3 reverses. Its replacement asserts the rendered counts.
