# Research memo: why a reader at a relay loses whole audio groups, and how audio should ride MoQ

2026-10-20, for #2862. Measured the streamlib tree at `main` `669f75953`. Read against the
pinned crates — `moq-net` 0.3.15, `hang` 0.21.16, `moq-tokio` 0.19.28, `moq-relay` 0.17.7
(`xtask/src/stage_local_relay.rs:20`) — and `moq-dev/moq` at `eecb86c5` (2026-10-20, `main`),
the moq-lite and moq-hang editor's drafts in that tree, draft-ietf-moq-transport-22,
draft-ietf-moq-loc-04 and draft-ietf-moq-msf-01 (the latest published of each), RFC 7587 /
6716, and WebRTC's `main` branch read on 2026-10-20, with no commit pinned. Measured on this
machine: an in-memory `moq-net` probe and a reader at an isolated test-root runtime's local
relay, both described in §Repro.

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
`runtime/tatolab-read-track` before its stopgap). The subscription's budget is applied
twice: once at the relay, which serves each downstream subscription through its own cursor
under that subscription's budget, and once in the reader's own cursor. Two independent
mechanisms each lose a whole group:

- **The budget mechanism.** At `max_age = 0`, group N counts as stale the moment group N+1
  presents its first frame, whether or not N was complete or read. This is the
  "group 8 arriving means group 7 is abandoned" rule, applied to *arrival*, not to need.
  - **What triggers it:** our publisher's writes reach the relay with ordinary scheduling
    jitter. They are mostly 20 ms apart, but now and then two groups land in the same
    millisecond. A zero-budget subscription keeps only the newer group of such a pair.
  - **Measured:** 51 of 551 `AudioBlock` groups lost (9 %), and 40 of 540 Opus groups.
  - **Where it happens:** the loss is unchanged when another subscriber holds the relay's
    *aggregate* upstream budget at 5 s. So it happens under this subscription's own budget:
    either at the relay's send side for this subscriber, or in the reader's cursor. These
    runs do not separate the two.
- **The seek mechanism.** `Ordered` never waits for a missing sequence. It hands over the
  lowest group *cached* above its cursor. If group N+1's stream lands before group N's, N is
  stepped over for good, **at any budget**, including the 5 s stopgap.
  - **Why inversions happen:** moq-lite's group order is fixed newest-first, so when two
    groups are ready together the newer one's stream is sent first.
  - **How often:** in steady state, 0.4–5 % of arrivals were inverted, almost all adjacent pairs.
    Inside a join's history burst the share is far higher.
  - **What was observed:** at the relay, this mechanism lost groups only inside join bursts.
    A 5 s `ordered` read lost nothing over ~240 steady-state groups. A steady-state loss
    needs the reader to poll between the two arrivals of a pair. That rests on the
    in-memory probe and the source, and was not seen at the relay.

**The join transient measured here is the stopgap's own doing, not the relay settling.** A
subscription with `max_age = 5 s` asks for every cached group the budget still considers
fresh.
- **The burst:** the relay hands that history over in one burst, up to ~250 groups (5 s)
  inside ~100 ms, on parallel streams, newest first.
- **The seek loss:** `Ordered` steps over whichever stragglers land after a higher sequence
  (6177 and 6740–6744 measured).
- **The budget edge:** the oldest group of the burst sits at the edge of the budget. Once
  newer content arrives it can expire after being handed out, and its read ends with
  `Error::Old` (6176, 6739 measured).
- **At `max_age = 0`:** there is no burst, because delivery starts at the latest group, but
  the steady loss above applies instead.
- **The ticket's own case is inferred, not measured.** Its 1→4→11 was a join a few groups
  into a fresh track, with no 5 s of history to replay. These runs joined long-running
  tracks only. [I] The same newest-first burst of whatever the origin had cached, read
  through `Ordered`, would produce it, but that is not measured here.

**Answers to the ticket's questions:**

- **Track per port: yes.**
- **One block (packet) per group: yes, keep it.** LOC, moq-dev's own publishers and our plan
  all agree, and more blocks per group would not fix either mechanism.
- **`mode: snapshot` misdescribes `AudioBlock`.** It declares the lossy latest-value
  contract to every reader, though no reader we run consults it, so it did not cause these
  gaps. Neither hang data mode fits a track of one block per group. This reopens an owner
  ruling, so it goes to `/align`.
- **The give-up signal is the gap at the centre of this.** There is **no transport signal
  that a missing group is really gone in every case**:
  - QUIC does not order streams.
  - A group reset before its header arrives leaves the reader no trace.
  - The one per-group "dropped" message (`SUBSCRIBE_DROP`, on the lite-06 wire we negotiate)
    is never sent by the pinned `moq-net`.
  - MoQT says outright that a subscriber cannot infer a group exists until an object of it
    arrives.

  moq-dev's own consumer gives up on a hole on any of five conditions:
  - the track ending;
  - the hole being outside the subscriber's range;
  - the group's own abort, when its header was seen;
  - **newer content on the same track proving the hole could no longer be played in time**,
    measured in the track's own timestamps;
  - **the same budget elapsing in monotonic time while blocked on the hole**, so content
    buffered behind it is still delivered when the publisher pauses.

  The content-time test fires only on data arriving. The monotonic test is a clock. Any
  reader that drops the clock holds what it has buffered behind a hole for as long as the
  track is paused. How large the budget is, and whether the clock arm is kept, are the
  owner's to decide.

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
- [V] The budget applies at the relay too, per downstream subscription. The relay's lite
  publisher serves each SUBSCRIBE through its own `track::Subscriber` carrying that
  subscription's `max_age` (`src/lite/publisher.rs:1222-1233`). Each group it hands out
  carries an expiry built from that subscription (`src/model/track.rs:3773-3781`). A group
  that expires before its stream opens is never opened (`src/lite/publisher.rs:2671-2676`).
  That publisher reads in arrival order, for the reason this memo's seek mechanism names:
  "a sequence cursor would then permanently skip the older group even though it is cached
  and in demand" (`src/lite/publisher.rs:2080-2083`).
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

**`opusdecoder/audio` (`AudioBlock` msgpack).** "Missing" counts sequences never handed out.
"`Error::Old`" counts groups handed out whose frame read then failed:

| Read policy | Groups read | Span | Missing | `Error::Old` | Arrivals inverted (whole run) |
|---|---|---|---|---|---|
| `ordered`, `max_age = 0` | 500 | 551 | **51** | 0 | n/a |
| `ordered`, `max_age = 0`, beside a 5 s reader | 500 | 552 | **52** | 0 | n/a |
| `arrival`, `max_age = 0` | 500 | 550 | **50** | 0 | 0 (older one shed) |
| `ordered`, `max_age = 5 s`, 4 fresh joins | 500 / 150 / 150 / 150 | 505 / 151 / 150 / 155 | 5 / 1 / 0 / 5 | 0 / 1 / 0 / 1 | n/a |
| `arrival`, `max_age = 5 s`, 4 fresh joins | 500 / 400 / 400 / 400 | 504 / 400 / 401 / 400 | 4 / 0 / 1 / 0 | 0 | 38 / 145 / 9 / 11 |

**`opusencoder/encoded_audio` (Opus):** `ordered` at zero lost **40 of 540**. `arrival` at
5 s lost **0 of 500**, with 70 arrivals inverted over the whole run.

- **The 5 s `ordered` joins.** Every group they lost, missing or `Error::Old`, was inside
  the join burst. Three of the four read only 150 groups and ended ~160 ms after subscribing,
  so they never left the burst. The first, 500 groups long, lost nothing over its ~240
  steady-state groups.
- **Arrival timing**, in `arrival` reads more than 200 ms after the first delivery (past the
  join burst):
  - **Inversions:** 0.4 % (Opus) to 5.0 % of arrivals (`dec-arrival5`, the first join, 7 of 238; the
    other three joins, 4 of 141, 6 of 377 and 3 of 374; Opus, 1 of 234; the read beside a 0-budget
    reader, 22 of 442).
  - **Gaps:** in `dec-arrival5`, 158 of 238 gaps were 20 ms and 21 were 0 ms (two groups in
    one burst). The rest scatter around 20 and 40 ms.
  - **Lateness:** the number of groups a delivery sat behind the highest yet seen was almost
    always 0 or 1. So the inversions are adjacent pairs, which is the newest-first send of
    two ready groups.
- **A join at 5 s** began up to ~250 groups behind live and caught up within ~100 ms. The
  `ordered` cursor lost the groups whose streams lost that race. The burst's oldest group sat
  at the edge of the budget and expired after being handed out, which is the Ordered doc's
  "a read still waiting on it ends with [`Error::Old`]" (`src/model/track.rs:4398-4400`).
- **Where the zero-budget loss happens.** With a 5 s reader holding the relay's aggregate
  upstream budget open, the zero-budget reader's loss was unchanged (52). The aggregate is
  the loosest subscriber's (`src/model/subscription.rs:158-166`), so the loss is not
  upstream of the relay. Two places remain, and these runs do not separate them:
  - the relay's send side for this subscription, under its own 0 budget (§1);
  - the reader's own cursor.

### 4. A missing group has no "gone" signal

- [V] moq-net: "QUIC does not order streams … a grace expires for a group whose stream was
  reset before its header arrived. A reset that keeps the header (reliable reset) would make
  the grace unnecessary" (`src/tail.rs:1-11`). That grace covers only a subscription's end.
- [V] We negotiate lite-06. `Versions::all()` leaves lite-07 out ("its wire format is still
  work-in-progress", `src/version.rs:299-310`). `moq-tokio` defaults to all versions
  (`connect.rs:741-744`), and nothing in `runtime/` or `xtask/` opts in to lite-07.
- [V] lite-06 has a `SUBSCRIBE_DROP` naming a dropped range (`src/lite/subscribe.rs:437-454`),
  but the pinned `moq-net` never sends one. Its only receive arm is the subscriber's
  (`src/lite/subscriber.rs:4285`), and no publisher path builds one. lite-07 removes it for a
  stream count in `SUBSCRIBE_END`, sent once at the subscription's end
  (`src/lite/version.rs:31-40`, `src/lite/subscribe.rs:494-497`). So mid-stream no message
  that reaches our readers names a dropped group.
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
    (`hang-0.21.16/src/catalog/mode.rs:16-28`). A `stream` consumer "MUST fail the read if
    the track carries a second" group (`drafts/draft-lcurley-moq-hang.md:559` in
    `moq-dev/moq`).
  - [V] `mode` is mandatory. `BinaryConfig.mode` is "Always stated", with no serde default
    (`hang-0.21.16/src/catalog/binary.rs:75-78`). "There is no safe default … a consumer
    that doesn't recognize the value (Unknown) must ignore the track rather than guess"
    (`mode.rs:6-12`).
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
     hold the cursor at a missing sequence. This is the order the relay's own publisher
     already serves in, for the same reason (`moq-net` `src/lite/publisher.rs:2080-2083`).
   - **Giving up on a hole:** on (a) the group's own abort, when its header arrived;
     (b) the track ending; or (c) newer content on the track running the budget past where
     the hole would end, measured in the track's timestamps. Whether to add upstream's
     fourth arm, (d) the budget elapsing in monotonic time while blocked, is item 3's fork.
   - **The budget on the wire:** subscribe with the same budget, so the relay does not shed
     what the reader would still take.
   - **Joining:** at the live edge, by flooring the local cursor at the latest group once
     live, not by replaying the budget's history.
   - **Counting:** every give-up is counted into `lost_since_last_read`.
3. **The budget, and whether it may run on a clock, is a live owner fork.**
   - **Why a budget can't be avoided:** the only ways around one are publisher-declared
     holes or a reliable reset. MoQT has the former (Prior Group ID Gap). lite-06's
     `SUBSCRIBE_DROP` exists on the wire, but the pinned `moq-net` never sends it.
   - **Its size, two candidates:**
     - **The track's retention** (publisher `Info::max_age`, 5 s). Nothing is given up that
       could still arrive, but a stalled hole can hold the cursor up to 5 s of content
       behind live. This fits `tatolab-read-track`, a recorder that wants everything.
     - **A per-reader latency budget** on `RelayTrackSource`. Latency is bounded, and a late
       group becomes a counted loss the decoder conceals. moq-dev uses ≥ 2 s for audio, and
       WebRTC's equivalent is the jitter buffer's playout target.
   - **The clock arm (d):** the content-time rule (c) fires only on newer data arriving, so
     it never sleeps. Without (d), groups already buffered above a hole are held for as long
     as the publisher is paused, and are released only when content resumes or the track
     ends. moq-dev keeps (d) for exactly that case. If the ticket's "waits and sleeps are never
     fixed in realtime streaming" covers giving up on a hole, it means (c) alone, at the cost
     of that held tail. That reading is the owner's to make.
4. **The `AudioBlock` catalog entry.** This reopens the DECIDED catalog entry
   (ARCHITECTURE.md §The catalog is assembled from port descriptions, `:3735`): "a built-in
   whose payload is a msgpack record declares `binary`, `mode: snapshot`, `mime:
   application/msgpack`". `snapshot` declares a lossy latest-value contract, and `stream`
   forbids more than one group. The options:
   - (a) A `binary` entry with no `mode` **is not viable**: hang requires it and fails to
     parse an entry without it (`binary.rs:75-78`). A mode string hang does not know makes a
     conforming reader ignore the track (`mode.rs:6-12`).
   - (b) Publish the exposed decoder output as `audio`/`pcm` raw f32le. That changes the
     wire of an exposed port.
   - (c) Keep `snapshot` and document that our readers ignore it.

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
- Whether the zero-budget loss happens at the relay's send side for that subscriber or in the
  reader's cursor. Both apply the same budget, and the runs here do not separate them.
- The ticket's own join case (1→4→11, a join a few groups into a fresh track). Every join
  here was to a long-running track, so its explanation in §Answer is inferred.
- Which published crate versions correspond to `moq-dev/moq` HEAD: HEAD's manifests carry
  older version numbers than ours, so upstream file anchors are to `eecb86c5`, not to our
  pins.
