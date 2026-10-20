# Research memo: why a reader at a relay loses whole audio groups, and how audio should ride MoQ

2026-10-20, for #2862. Read against the pinned crates — `moq-net` 0.3.15, `hang` 0.21.16,
`moq-relay` 0.17.7 (`xtask/src/stage_local_relay.rs:20`) — and `moq-dev/moq` at `eecb86c5`
(2026-10-20, `main`), the moq-lite editor's draft in that tree, draft-ietf-moq-transport-22,
draft-ietf-moq-loc-04, draft-ietf-moq-msf-01, RFC 7587 / 6716 / 4585 and WebRTC `main`.
Measured on this machine: an in-memory `moq-net` probe and a reader at an isolated test-root
runtime's local relay, both described in §Repro.

Evidence is tagged **[V] verified** (source line or page read), **[M] measured here** and
**[I] inferred**.

## Question

1. Why does a reader at the local relay lose whole groups the publisher wrote?
2. What causes the join transient (1→4→11)?
3. Is one track per audio port right?
4. One block per group, or several blocks per group so a stream orders them?
5. Is `mode: snapshot` right for the `AudioBlock` catalog entry?
6. What does a reader use to give up on a group?

## Answer

**The loss is the reader's own read policy. The publisher, the relay and the grouping are
not at fault.** Both of our readers subscribe with `Subscription::default()`, which is
`max_age = 0`, and read through `.ordered()`
(`runtime/streamlib-media-builtins/src/relay_track_source.rs:522-526`;
`runtime/tatolab-read-track` before its stopgap). Two independent mechanisms in that cursor
each lose a whole group:

- **The budget mechanism.** At `max_age = 0`, group N counts as stale the moment group N+1
  presents its first frame, whether or not N was complete or read. This is the
  "group 8 arriving means group 7 is abandoned" rule, applied to *arrival*, not to need.
  Our publisher's writes reach the relay with ordinary scheduling jitter: mostly 20 ms
  apart, but now and then a 40 ms gap followed by two groups in the same millisecond. A
  zero-budget reader keeps only the newer group of each such pair. Measured: 51 of 551
  `AudioBlock` groups lost (9 %), and 40 of 540 Opus groups. The losses stay in the reader
  even when the relay's aggregate budget is held at 5 s by another subscriber.
- **The seek mechanism.** `Ordered` never waits for a missing sequence. It hands over the
  lowest group *cached* above its cursor. If group N+1's stream lands before group N's, N is
  stepped over for good, **at any budget**, including the 5 s stopgap. Inversions are
  routine, not exceptional. moq-lite's group order is fixed newest-first, so when two groups
  are ready together the newer one's stream is sent first. 5–20 % of arrivals were inverted
  in steady state.

**The join transient is the stopgap's own doing, not the relay settling.** A subscription
with `max_age = 5 s` asks for every cached group the budget still considers fresh. The
relay hands that history over in one burst, up to ~250 groups (5 s) inside ~100 ms, on
parallel streams, newest first. `Ordered` then steps over whichever stragglers land after a
higher sequence (6177; 6740–6744 measured). At `max_age = 0` there is no burst, because
delivery starts at the latest group, but the steady loss above applies instead.

**Answers to the ticket's questions:**

- **Track per port: yes.**
- **One block (packet) per group: yes, keep it.** LOC, moq-dev's own publishers and our plan
  all agree, and more blocks per group would not fix either mechanism.
- **`mode: snapshot` is wrong for `AudioBlock`.** It declares the lossy latest-value
  semantics that caused this. But neither hang data mode fits a track of one block per
  group, which is a decision for `/align`.
- **The give-up signal is the gap at the centre of this.** There is **no transport signal
  that a missing group is really gone in every case**:
  - QUIC does not order streams.
  - A group reset before its header arrives leaves the reader no trace.
  - moq-lite-07 removed the one per-group "dropped" message.
  - MoQT says outright that a subscriber cannot infer a group exists until an object of it
    arrives.

  So every first-party reader gives up on a hole in one of three ways: the track ending, the
  group's own abort when its header was seen, or **newer content on the same track proving
  the hole could no longer be played in time**. That last test is measured in the track's
  own timestamps and fires on data arriving, never on a sleep. Its size is a budget someone
  has to choose. That choice is the owner's.

## Evidence

### 1. The read policy in `moq-net` 0.3.15

- [V] `Subscription::default()` has `max_age: Duration::ZERO` (`src/model/subscription.rs:95`).
  Its doc says "[`Duration::ZERO`] (the default) skips immediately: group 8 arriving means
  group 7 is abandoned. A larger budget tolerates that much reordering before giving up"
  (`:19-23`). hang repeats it: a subscriber's max_age "defaults to ZERO (skip the moment a
  newer group arrives)" (`hang-0.21.16/src/container/frame.rs:29-31`).
- [V] Staleness is measured on presentation timestamps, never a clock. Group N is stale when
  the newest frame on the track is `>= budget` past N's *reach*, the first timestamp of its
  immediate successor (`src/model/track.rs:553-567` `reach`, `:610-638` `is_stale`). "Only
  timestamps drive expiry; wall-clock reclamation of idle content is the cache's own policy"
  (`:596-598`). At zero, N+1 presenting its first frame already convicts N.
- [V] `Ordered`: "Every group it returns has a higher sequence than the last, so a late
  arrival … is skipped rather than delivered out of turn". "A gap costs nothing either way,
  since the cursor seeks to the lowest cached group in range rather than waiting for the
  sequence it would have read next" (`src/model/track.rs:4380-4404`). The seek is
  `poll_next_in_range`, the first non-aborted group in `next..` (`:425-460`).
- [V] Group order on moq-lite is fixed newest-first. The priority queue ranks "Newest group
  first, which is what makes a congested track shed its backlog rather than its live edge"
  (`src/lite/priority.rs:19-61`). The subscriber's group-order byte "is ignored: group order
  is fixed, so a peer that still sets it gets the same newest-first delivery"
  (`src/lite/subscribe.rs:110-112`).
- [V] Join: "Delivery starts at the oldest group at or above the floor that the budget still
  considers fresh" (`src/model/subscription.rs:59-66`). The moq-lite editor's draft says a
  Max Age of 0 "wants only the latest group" (`drafts/draft-lcurley-moq-lite.md:1173` in
  `moq-dev/moq`).
- [V] Publisher retention is not the limit. Our exposed tracks are `Info::default()` with a
  nanosecond timescale (`runtime/tatolabd/src/exposed_port_publisher.rs:669-671`), which keeps
  `DEFAULT_MAX_AGE` = 5 s (`src/model/track.rs:36`, `:117-122`).
- [V] Our publisher does what the plan says: one `append_group` per `StartsGroup`, one
  `write_frame` per message (`runtime/tatolabd/src/exposed_port_publisher.rs:574-618`).

### 2. The in-memory probe [M]

The probe writes one-frame groups 20 ms apart on a `track::Producer` and drains
`subscribe(…).ordered()` without blocking:

| Case | `max_age = 0` | `max_age = 5 s` |
|---|---|---|
| 4 complete groups arrive, then the reader polls | `[3]` | `[0, 1, 2, 3]` |
| reader polled after every group | `[0, 1, 2, 3]` | `[0, 1, 2, 3]` |
| seq 1 lands, reader polls, then seq 0 and seq 2 land | `[1]`, then `[2]` | `[1]`, then `[2]` |
| seq 0's stream opens empty, seq 1 lands complete, then seq 0's frame | `[1]` | `[0, 1]` |

Rows 1 and 4 are the budget mechanism. Row 3 is the seek mechanism: group 0 arrives inside
any budget and is still never delivered.

### 3. At a real relay [M]

Setup: an isolated test-root runtime (`cargo xtask build-runtime --release
--machine-directories-under-a-test-root`, local relay on loopback `:14443`). It ran
`opus_roundtrip_stream.py` with `STREAMLIB_KNOWN_SIGNAL_REPEATS=1`, both audio ports exposed
private. A probe reader dialled the relay with `moq-tokio` 0.19.28, as `tatolab-read-track`
does, and logged each group's sequence, stamp and arrival. Each policy read alone unless
noted.

**`opusdecoder/audio` (`AudioBlock` msgpack), 500 groups per read:**

| Read policy | Span | Missing | Arrivals inverted |
|---|---|---|---|
| `ordered`, `max_age = 0` | 551 | **51** | n/a |
| `ordered`, `max_age = 0`, beside a 5 s reader | 552 | **52** | n/a |
| `arrival`, `max_age = 0` | 550 | **50** | 0 (older one shed) |
| `ordered`, `max_age = 5 s`, 4 fresh joins | 505 / 151 / 150 / 155 | 5 / 1 / 0 / 5, **all at join** | n/a |
| `arrival`, `max_age = 5 s`, 4 fresh joins | 504 / 400 / 401 / 400 | 4 / 0 / 1 / 0 | 38 / 145 / 9 / 11 |

**`opusencoder/encoded_audio` (Opus):** `ordered` at zero lost **40 of 540**; `arrival` at
5 s lost **0 of 500**, with 70 arrivals inverted.

- **Arrival timing.** In the steady state of an arrival read, 230 of 350 gaps were 20 ms and
  32 were 0 ms (two groups in one burst), each 0 ms gap after a ~40 ms one. Lateness, the
  number of groups a delivery sat behind the highest yet seen, was 0 or 1 only. So the
  inversions are adjacent pairs, which is the newest-first send of two ready groups.
- **A join at 5 s** began up to ~250 groups behind live and caught up within ~100 ms. The
  `ordered` cursor lost the groups whose streams lost that race.
- **Where the zero-budget loss happens.** With a 5 s reader holding the relay's aggregate
  budget open, the zero-budget reader's loss was unchanged (52). The relay's aggregate is the
  loosest subscriber's (`src/model/subscription.rs:158-166`), so this loss happens in the
  reader's own cursor.

### 4. A missing group has no "gone" signal

- [V] moq-net: "QUIC does not order streams … a grace expires for a group whose stream was
  reset before its header arrived. A reset that keeps the header (reliable reset) would make
  the grace unnecessary" (`src/tail.rs:1-11`). That grace covers only a subscription's end.
- [V] moq-lite-07 replaced `SUBSCRIBE_DROP` with a stream count in `SUBSCRIBE_END`, sent
  once at the subscription's end (`src/lite/version.rs:31-40`, `src/lite/subscribe.rs:494-497`).
  So mid-stream no message names a dropped group.
- [V] What does reach the subscriber: a group the publisher gives up on *after* opening its
  stream is reset with the reason (`Old`, …), "so the subscriber can tell a truncated group
  from a routine cancel" (`src/lite/publisher.rs:2642-2645`). A group it gives up on
  *before* opening (`GroupState::Open` + `poll_expired`, `:2671-2676`) gets no stream at all.
- [V] MoQT-22 §2.3.1: "Objects from different Groups can always be received out of order. As
  a result, subscribers cannot infer the existence of a Group until an object in the Group is
  received." MoQT does have publisher-declared holes: §10.8 Prior Group ID Gap (groups that
  "do not, and will never, exist") and the End of Group / End of Track object status
  (§11.1.1). moq-lite has no equivalent.
- [V] moq-dev's own consumer (`rs/moq-mux/src/container/consumer.rs` at `eecb86c5`) reads in
  arrival order (`poll_recv_group`, `:635`), buffers groups by sequence and holds its cursor
  at a missing one. Its rationale: "Groups race on independent QUIC streams (newer ones at
  higher priority), so a buffered higher sequence proves nothing: the missing one may be
  merely late, and there is no way to tell that from an eviction" (`:503-512`).
  - **What makes it give up:** the track finished, the hole excluded by the subscriber's
    range, newer content's timestamps a full budget past the next group's start, or the
    budget's own monotonic time elapsed "so a marker behind a hole is delivered even when
    the track is paused" (`:36-46`, `:515-523`). Then it yields `Read::Gap`.
  - **Not available to us:** this consumer is not in our pinned set. hang 0.21.16 ships only
    `container/frame.rs` and `mod.rs`.
- [V] WebRTC draws the same line, measured against playout. NetEQ conceals (`kExpand`) when
  no packet is due (`modules/audio_coding/neteq/decision_logic.cc:247-261`), discards
  packets older than playout (`neteq_impl.cc:1892-1897`), and audio NACK asks only for
  packets "which have expected time-to-play larger than the given round-trip-time"
  (`audio/nack_tracker.h:66-68`). RFC 6716 §4.4: PLC "SHOULD be included when receiving from
  an unreliable channel". The playout budget decides lateness, never arrival order.

### 5. How audio is carried

- **One object per group.**
  - [V] LOC-04 §4.1: "each encoded audio chunk, say 10ms, represents a MOQT Object. In this
    setup, there is one MOQT Object per MOQT Group … each such unitary group is sent over an
    individual unidirectional QUIC stream". RFC 7587 §4.2: an RTP payload "MUST contain
    exactly one Opus packet".
  - [V] moq-dev publishes with a *minimum* group duration of 20 ms
    (`js/publish/src/audio/encoder.ts:111`, `:650-656`: "Every audio frame decodes on its
    own, so any of them can open a group … A dropped group leaves a gap for the codec's
    PLC"). The Rust side is the same (`rs/moq-audio/src/encode/producer.rs:41-62`), whose
    doc notes the trade: "Longer groups … coarser loss: a viewer that falls behind skips a
    whole group, and a lost packet stalls the rest of its group until retransmitted".
  - [V] The 20 ms default came from cost, not correctness. moq-dev #4784 measured one stream
    per Opus frame as "the first scaling limit (47-50 groups per second per track per
    subscriber)". 20 ms Opus at a 20 ms minimum is still one packet per group.
  - [I] Packing several blocks per group makes one stream order them, but it does not fix
    either mechanism: a zero-budget reader then sheds a whole multi-block group per
    superseding arrival, and inversions still happen between groups.
- **Track per port.**
  - [V] MSF-01 §5.6.1 gives audio its own `"role":"audio"` track, and hang's catalog has one
    `audio` entry per rendition (`hang-0.21.16/src/catalog/audio/mod.rs`).
  - [V] Audio outranks video in hang's priorities, 80 against 60
    (`src/catalog/priority.rs:21-26`).
- **How players subscribe.**
  - [V] moq-dev's watch player subscribes audio with at least `AUTO_MAX_DELAY = 2000 ms` in
    auto mode (`js/watch/src/audio/decoder.ts:186-196`, `js/watch/src/audio/latency.ts:34`).
    Its stated reason: "a subscription cut to the target hides every frame later than the
    target" (`doc/concept/audio-jitter.md:51-56`).
  - [V] On join it moves the *local* cursor to the live edge rather than replaying the
    budget's history (`rs/moq-audio/src/decode/consumer.rs:182-199`).
  - [V] Upstream has since renamed `max_age` to `max_delay`.
- **The catalog `mode`.**
  - [V] hang's `snapshot` is "Lossy latest-value: each group carries one complete payload and
    supersedes the previous group, so a consumer reads only the newest". `stream` is
    "Lossless append log: a single group that is never rolled"
    (`hang-0.21.16/src/catalog/mode.rs:16-28`). The moq-hang draft says a `stream` consumer
    must fail the read on a second group.
  - [V] hang's `audio` section has a `pcm` codec, "Uncompressed interleaved little-endian
    IEEE-754 binary32 PCM" (`src/catalog/audio/codec.rs:18-20`).
  - [I] `AudioBlock` is msgpack, not raw f32le, so `pcm` would not describe it as published.
    No reader we run reads `mode` to choose a policy, and `moq-relay` reads no catalog. So
    the entry is a misdescription to third-party readers, not a cause of these gaps.

## Recommendation, for `/align`

These are recommendations, not decisions. Items 2–4 change what the plan says.

1. **Keep the publisher as decided:** one track per port, one message per group
   (ARCHITECTURE.md §group boundary). The evidence puts none of the loss there, and both LOC
   and moq-dev group the same way.
2. **Read through one relay-track reader, in arrival order with a sequence hold, never
   through `.ordered()`.**
   - **Who uses it:** one core system, owned by the relay-reading concern, used by
     `RelayTrackSource` and `tatolab-read-track` alike. Where it lives is for `/align`.
   - **The cursor:** receive groups in arrival order, release them in sequence order, and
     hold the cursor at a missing sequence.
   - **Giving up on a hole:** only on (a) the group's own abort, when its header arrived;
     (b) the track ending; or (c) newer content on the track running the budget past where
     the hole would end, measured in the track's timestamps.
   - **The budget on the wire:** subscribe with the same budget, so the relay does not shed
     what the reader would still take.
   - **Joining:** at the live edge, by flooring the local cursor at the latest group once
     live, not by replaying the budget's history.
   - **Counting:** every give-up is counted into `lost_since_last_read`.
3. **The budget is a live owner fork.** The only ways to avoid choosing one are publisher-
   declared holes, which moq-lite lacks and MoQT has (Prior Group ID Gap), or a reliable
   reset. Neither is available on the wire we speak. The candidates:
   - **The track's retention** (publisher `Info::max_age`, 5 s). Nothing is ever given up
     that could still arrive, but a stalled hole can hold the cursor up to 5 s of content
     behind live. This fits `tatolab-read-track`, a recorder that wants everything.
   - **A per-reader latency budget** on `RelayTrackSource`. Latency is bounded, and a late
     group becomes a counted loss the decoder conceals. moq-dev uses ≥ 2 s for audio, and
     WebRTC's equivalent is the jitter buffer's playout target.

   Item 2's content-time rule is not a sleep: with nothing newer arriving, a hole is never
   given up, because there is nothing to skip to.
4. **The `AudioBlock` catalog entry:** `snapshot` declares the wrong contract and `stream`
   forbids more than one group. The options:
   - (a) A `binary` entry with no `mode`. Whether hang accepts an absent `mode` is not
     checked.
   - (b) Publish the exposed decoder output as `audio`/`pcm` raw f32le. That changes the
     wire of an exposed port.
   - (c) Leave it and document that our readers ignore it.

   The same question stands for every msgpack media built-in declaring
   `BinaryEntry(mode="snapshot")` (`_built_in_nodes.py:78`, `:130`, `:244`, `:346`, `:397`,
   `:582`).
5. **Remove both stopgaps once item 2 lands:** `tatolab-read-track`'s
   `A_GROUP_DELIVERED_LATE_IS_WAITED_FOR` (`runtime/tatolab-read-track/src/main.rs:185-190`),
   and `OBJECTS_THE_JOIN_TAKES=16` (`runtime/streamlib-engine/tests/fixtures/verify_audio_channel.sh:129-131`,
   echoed in `audio_track_dump_check.py:258`).

## Repro

- **In memory, deterministic.** This is the regression seam for the fix. Build one
  `track::Producer` (`broadcast::Info::new().produce().create_track(…, Info::default())`)
  and write one-frame groups with `append_group` / `create_group` at 20 ms stamps. Read with
  `.subscribe(sub).ordered()` and `next_group().now_or_never()`. Write a burst before reading
  (budget mechanism), and create seq 1 before seq 0 (seek mechanism). The table in §2 is the
  expected and observed output for today's reader.
- **At a relay:**
  1. Build an isolated runtime: `cargo xtask build-runtime --release
     --machine-directories-under-a-test-root`.
  2. Start it per `.claude/skills/drive-running-stream/SKILL.md`, with
     `STREAMLIB_KNOWN_SIGNAL_REPEATS=1` and `STREAMLIB_CAPTURED_WAVEFORM` set on `tatolabd`.
  3. Load and expose the stream:
     `tatolab run -d --dir runtime/streamlib-engine/tests/fixtures --name opus_rt opus_roundtrip_stream.py`,
     then `tatolab expose opus_rt opusdecoder audio`.
  4. Read `<machine>/opus_rt.hang` / `opusdecoder/audio` at `http://127.0.0.1:14443` with a
     `moq-tokio` client under each policy, and compare sequence spans.

## What remains unknown

- Whether the pair bursts come from the graph (the source's `interval_ms=1` pacing, the
  window stage) or from the iceoryx2 → publisher hop. Either way it is the jitter a reader
  must absorb, not a defect to remove, and loss at budget zero would only grow on a real
  network.
- How the loss rates change over a WAN relay. Everything here ran on loopback.
- Whether `hang` accepts a `binary` entry with no `mode` (recommendation 4a).
- Which published crate versions correspond to `moq-dev/moq` HEAD: HEAD's manifests carry
  older version numbers than ours, so upstream file anchors are to `eecb86c5`, not to our
  pins.
