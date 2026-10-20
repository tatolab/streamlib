# relay-track-reading

> **Approved by the owner, 2026-10-20**, as written, with its stated assumptions. Tickets derived
> 2026-10-20 (below).

The build of the §Networking align of 2026-10-20 (`ARCHITECTURE.md:3825-3843`, ADR
`docs/decisions/relay-track-reading.md`, evidence `docs/research/2026-10-20-audio-groups-over-moq.md`).

**After this change:**
- **One reader.** A reader at a relay loses no group that was published to it and arrived
  within its `max_age`. The relay source node and `tatolab-read-track` read through one
  relay-track reader.
- **Ordering.** The reader receives in arrival order, releases in sequence order, and holds
  at a missing sequence. It gives up a missing group only on its stream's reset (which
  moq-net surfaces as the group's abort), the track's end, or newer content past `max_age`,
  measured in the track's own timestamps. After a give-up it moves to the next sequence. It
  joins at the live edge.
- **The relay source node's config.** It takes `max_age` (100 ms by default).
  `tatolab-read-track` waits the track's declared retention.
- **Losses reach the graph.** Each group the relay source node gives up reaches its readers
  as a loss in `lost_since_last_read`, so a decoder conceals it.
- **The stopgaps are gone:** `tatolab-read-track`'s 5 s wait and the audio rig skipping a
  read's first 16 objects.
- **What a stream author sees:** only the new optional `max_age` key.

**Scale gate — this skill, plus the existing ADR.**
- **Contract changes:** a built-in's config contract (a new key on `RelayTrackSourceConfig`)
  and the processor model's loss accounting (a writer-declared loss).
- **ADR:** `relay-track-reading` carries the loss-accounting clause.
- **Not touched:** the RHI and the frame header.

**Precondition.** Every entry built is DECIDED:
- §Networking `:3794-3824` (the relay source node, its config list naming `max_age`) and
  `:3825-3843` (the reading rule, the loss sentence, the three `Rejected:` lines);
- §Processor model `:972-1046` (loss visibility). It is modified here as Decided 1 states.

## Current state (recon, 2026-10-20, `main` `acbc86f68`)

- **`RelayTrackSource`** (`runtime/streamlib-media-builtins/src/relay_track_source.rs`):
  - **Subscribe:** it subscribes `.subscribe(None).await?.ordered()` (`:522-526`) and reads
    with a biased select over the catalog, the session and `next_group()` (`:386-430`).
  - **Writes:** each object goes out through `write_bytes_with_timestamp` (`:464`), with
    `StartsGroup` on a group's first object (`:415-420`).
  - **Config:** `RelayTrackSourceConfig` is `deny_unknown_fields` with `relay_url`,
    `broadcast` and `track` (`:55-86`). `the_descriptor_publishes_the_config_and_the_one_output`
    pins the property keys to those three (`:1068-1083`).
  - **A name to rename:** its private session type is already named `RelayTrackReader`
    (`:320-326`).
- **`tatolab-read-track`** (`runtime/tatolab-read-track/src/main.rs`):
  - **Subscribe:** it subscribes with `A_GROUP_DELIVERED_LATE_IS_WAITED_FOR` (5 s, `:34-37`)
    and then `.ordered()` (`:183-191`).
  - **Read loop:** `next_group` (`:211-222`), skipping a group joined mid-way (`:224-233`).
  - **Dependencies:** moq-net, moq-tokio, moq-json and hang, with **no engine dependency**
    (`Cargo.toml`). Every other MoQ-using crate depends on the engine through `streamlib`, so
    there is no MoQ-only crate today.
- **Loss accounting** (`docs/decisions/loss-visibility.md`):
  - **The sequence number:** each output port's `ChannelEgress` stamps a per-publisher `u64`
    sequence number into `DataChannelBagSequenceNumberUserHeader` (`#[repr(C)]`, layout test
    `runtime/streamlib-ipc-types/src/lib.rs:678-685`, `:917-935`; stamp
    `runtime/streamlib-engine/src/iceoryx2/output.rs:473-484`).
  - **How a reader counts a gap:** it adds `new − last − 1` to the link's dropped count on
    `ordered` ports
    (`runtime/streamlib-engine/src/iceoryx2/bags_a_gap_in_the_numbering_says_were_lost.rs:42-55`,
    `input.rs:980-995`), surfaced as `lost_since_last_read` (`input.rs:1088-1097`).
  - **Who that covers:** helper readers, the exposed-port publisher and `audio_window` ports
    all count it. No writer-declared loss exists.
- **The stopgaps:** `OBJECTS_THE_JOIN_TAKES=16` and `--from-object`
  (`runtime/streamlib-engine/tests/fixtures/verify_audio_channel.sh:129-142`,
  `audio_track_dump_check.py:253-270`), with its test
  `test_the_join_to_the_relay_is_left_out_of_the_measurement`
  (`test_audio_track_dump_check.py:352-372`).
- **The test-side reader:** the loopback relay test reads with `.ordered()` too
  (`runtime/tatolabd/tests/tatolabd_publishes_exposed_ports_at_the_local_relay.rs:441`).

## Decided 1 — a given-up group is carried in its own header field (owner, 2026-10-20)

The plan says a given-up group reaches readers in `lost_since_last_read`, "the way a link
counts one" (`:3836-3838`). It also says "A bag dropped at a port is counted by the port that
dropped it" (`:972-974`) and that the first sample after wiring is the baseline (`:999-1000`).
The ways to carry it disagree with those sentences differently.

- Rejected: (a) advance the existing sequence number — `graph` would show the loss as the
  downstream link's drop, and it vanishes on a link's first sample (owner, 2026-10-20).
  **What (a) was:** The relay source node bumps its output's
  `next_sequence_number` by the count before its next write. There is no wire change, and
  every `ordered` reader counts the jump today.
  - **Cost:** the loss lands in the *downstream link's* `dropped_bags_by_link`, as if that
    link had dropped it, which contradicts `:972-974`.
  - **Cost:** it is lost uncounted when it rides a link's first sample, the baseline.
  - **Cost:** it cannot be told apart from a ring overwrite.
  - **Plan wording:** `:999-1003` gains "or a writer declared lost".
- **(b) A second field in the user header — chosen.**
  `DataChannelBagSequenceNumberUserHeader` gains `messages_lost_upstream_before_this_one: u64`,
  with its layout test, written by a new engine-internal writer call.
  - **Readers:** a reader adds it to `lost_since_last_read` on `ordered` ports, read straight
    off the header with no baseline involved. It never adds it to the link's dropped count.
  - **The producer's own count:** the relay source node counts its give-ups on the producer,
    `given_up_groups_by_output_port`, beside `refused_bags_by_output_port`. This follows the
    precedent of `:1036-1045`.
  - **Result:** `graph` attributes the loss where it happened, a decoder still sees it, and
    the baseline residual does not apply.
  - **Cost:** a `#[repr(C)]` wire type changes. The header's iceoryx2 type name stays pinned,
    and opener and builder must agree, which is already gated
    (`iceoryx2::node::tests::a_channel_data_service_and_an_opener_disagreeing_on_the_user_header_are_refused_by_name`).

(b) is the only option that keeps both `:972-974` and the loss sentence true. The call is
engine-internal Rust: Python's `write` gains nothing.

## Decided 2 — one given-up group counts as one (owner, 2026-10-20)

`lost_since_last_read` counts messages, and the relay source node writes one message per
object. A given-up group's object count is unknown: it may never have been seen at all.

- **(a) One per group given up — chosen.** For audio, a group is one object, so the
  count is exact. For video, the count is a lower bound, and a decoder's reaction is the same
  whatever the count: discard to the next `starts_group` (`:3135`). The plan states the lower
  bound.
- Rejected: (b) the objects seen of it, plus one — more bookkeeping for a number that is still
  a guess when a group's header never arrived (owner, 2026-10-20).

## ADDED: the relay-track reader — `runtime/streamlib-relay-track-reader`

- **The crate.** A new crate depending on `moq-net` only, never the engine, so
  `tatolab-read-track` stays engine-free. It holds
  `RelayTrackGroupsReleasedInSequenceOrder`, built from a `moq_net::track::Consumer` and a
  `max_age`:
  - **Subscribe and join:** it subscribes with `Subscription::default().with_max_age(max_age)`.
    Once the feed is live it floors its local cursor at the latest group, the way
    `moq-dev/moq` `rs/moq-audio/src/decode/consumer.rs:182-199` does, so it replays no history.
  - **Ordering:** it reads `recv_group()` (arrival order) into a hold keyed by sequence, and
    releases the lowest held group whose sequence is next.
  - **Giving up:** a missing sequence is given up on one of three events:
    - its stream is reset after its header arrived, which moq-net surfaces as the group's
      abort;
    - the track ends;
    - the newest frame of a later group is `max_age` past the hole's reach, the first
      timestamp of the held group immediately after it. This is the reach rule moq-net's own
      staleness check uses (`moq-net` `src/model/track.rs:553-567`, `:610-638`), so a later
      group whose timestamps step back neither convicts the hole nor shrinks its reach.
  - **Timestamps that never advance:** such a track never ages a hole, so its hole is given
    up only on a reset or the track's end. That is the decided no-clock rule applied to a
    track that carries no time.
  - **After a give-up:** the cursor moves to the next sequence. A given-up group that arrives
    later is discarded, because its loss is already counted.
  - **A group served part-way through:** a group served from a later object than its first
    (`index() > 0`) is skipped to the next group's start, as `tatolab-read-track` does today
    (`runtime/tatolab-read-track/src/main.rs:224-233`). A group served whole is released
    whole.
  - **What it yields:** `ReleasedRelayTrackGroup::InSequence(group)` or
    `ReleasedRelayTrackGroup::GivenUp { groups: u64 }`.
- **The relay source node** reads through it. On `GivenUp` it counts the loss on itself at
  once, as `given_up_groups_by_output_port`, and declares it to its readers on its next write,
  through the header field of Decided 1. A give-up after its last write reaches no reader.
  That is the residual §Processor model already states for a ring gap after a link's last
  receive (`ARCHITECTURE.md:1022-1024`). Its private `RelayTrackReader` is renamed `RelayTrackSourceSession`.
  - **Config:** `RelayTrackSourceConfig` gains `max_age`, a whole number of milliseconds, 100
    by default. Its doc comment names the unit, and the generated
    `_built_in_nodes.py` TypedDict follows through
    `cargo xtask generate-built-in-node-classes`.
- **`tatolab-read-track`** reads through it with `max_age = track.info().max_age`, the track's
  declared retention.
- **The tatolabd loopback test's reader** reads through it too, so no test reads a relay with
  `.ordered()`.

## MODIFIED: §Processor model — loss visibility (`:972-1046`)

- **A new DECIDED clause after `:1036-1045`.** "A message a writer declares lost upstream is
  carried in the user header beside the sequence number and added to an `ordered` reader's
  `lost_since_last_read`, never to its link's dropped count; the writer counts it on itself,
  per output port. Only engine-internal writers declare one. A loss declared after the
  writer's last write reaches no reader, the producer's count excepted."
- **The two residuals at `:1022-1024` stay as written.** They are about ring gaps.

## MODIFIED: §Networking — verify lines for `:3794-3824` and `:3825-3843`

The entries' text stands. Each gains `<!-- verify: -->` lines naming the tests below.

## Tests — the seams

- **The reader crate, in-memory.** Each test builds a `moq_net::track::Producer` with
  one-frame groups at 20 ms stamps. This is the probe in the memo's §2.
  - `a_burst_of_complete_groups_is_released_whole_and_in_sequence`
  - `a_group_whose_stream_lands_after_its_successors_is_released_before_it`
  - `a_missing_group_is_held_while_newer_content_stays_within_max_age`
  - `a_missing_group_is_given_up_once_newer_content_runs_max_age_past_it`
  - `a_missing_group_is_never_given_up_while_nothing_newer_arrives`. This proves no clock.
  - `a_reset_group_is_given_up_at_once`
  - `a_given_up_group_arriving_late_is_discarded`
  - `a_group_served_after_its_first_object_is_skipped_to_the_next_groups_start`
  - `a_track_whose_timestamps_never_advance_gives_up_a_hole_only_on_its_reset_or_end`
  - `a_track_that_ends_gives_up_its_missing_groups`
  - `a_reader_joining_a_track_with_cached_history_starts_at_the_live_edge`
- **The relay source node:**
  - `the_descriptor_publishes_the_config_and_the_one_output` now names `max_age`.
  - `a_config_without_max_age_reads_with_one_hundred_milliseconds`
  - `a_given_up_group_reaches_the_reader_of_the_output_as_lost_since_last_read`. This runs
    the engine's input and output pair.
  - `a_given_up_group_is_counted_on_the_relay_source_and_not_on_the_downstream_link`.
- **The engine:**
  - a layout regression test for the widened `DataChannelBagSequenceNumberUserHeader`;
  - `a_declared_upstream_loss_reaches_lost_since_last_read_on_an_ordered_port`;
  - `a_declared_upstream_loss_on_a_links_first_sample_is_still_counted`;
  - `a_newest_port_does_not_count_a_declared_upstream_loss`.
- **Through the local relay**, in
  `runtime/tatolabd/tests/tatolabd_publishes_exposed_ports_at_the_local_relay.rs`:
  `a_relay_track_source_reading_paired_writes_keeps_every_group`. A writer emits groups two
  at a time, the case that lost 9 % at budget zero.
- **The audio rig:** `verify_opus_roundtrip.sh` and `verify_audio_loopback.sh` pass with
  `OBJECTS_THE_JOIN_TAKES` gone and the check measuring from the first object.

## Assumptions stated, not asked

- **The crate's name and home:** `runtime/streamlib-relay-track-reader`. It is new because
  no MoQ-only crate exists, and `tatolab-read-track` must not depend on the engine.
- **`max_age`'s value:** a whole number of milliseconds, MoQ's own wire unit for Max Age.
- **Joining:** floor the local cursor at the latest group, never a wire-level start. The
  wire-level start is aggregated across a relay's subscribers and can rewind for everyone.
- **`tatolab-read-track`'s dump** gains nothing for a given-up group. The gap in its
  `group_sequence` column is the record.
- **`exposed_port_publisher.rs:1017`** keeps `.ordered()`. It is an in-memory unit test on
  one producer, where no stream can land out of order.

## Expected slices

1. **The reader and both readers.** The crate, `RelayTrackSource` and `tatolab-read-track`
   read through it, `max_age` config, the loopback test, and the stopgaps removed. It is
   proven end to end on the audio rig.
2. **The declared loss.** The header field, wired from the relay source node to
   `lost_since_last_read` and to `given_up_groups_by_output_port`, with the §Processor model
   text.

## Left to later

The adaptive jitter buffer and decoded audio as `audio`/`pcm` are both OPEN
(`:3565-3568`, `:3761-3764`). Nothing here builds toward them.

## Tickets

Derived 2026-10-20; milestone *Streams shared through MoQ relays*.

1. #2895: a reader at a relay keeps every group that arrived within its `max_age`, through
   one relay-track reader (slice 1). Independent; needs the audio rig. It carries the REMOVED
   bullets.
2. #2896: a group a relay source gives up reaches its readers as `lost_since_last_read` and
   is counted on the relay source (slice 2). Blocked by #2895; needs no rig.

## REMOVED

- REMOVED: A_GROUP_DELIVERED_LATE_IS_WAITED_FOR
- REMOVED: OBJECTS_THE_JOIN_TAKES
- REMOVED: from_object
- REMOVED: test_the_join_to_the_relay_is_left_out_of_the_measurement
