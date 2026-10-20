# Reading a relay track without losing what was published

Rationale for the `[relay-track-reading]` entries in `docs/plan/ARCHITECTURE.md` §Networking,
decided 2026-10-20. Evidence: `docs/research/2026-10-20-audio-groups-over-moq.md`.

## Trigger

Read this before changing how anything subscribes to a relay track, before choosing a
subscription's `max_age`, and before adding a playout or jitter buffer.

## Decision

- **One reader.** The relay source node and `tatolab-read-track` read through one relay-track
  reader.
- **Ordering.** It receives groups in arrival order, releases them in sequence order, and holds
  at a missing sequence.
- **Giving up.** A missing group is given up on one of three signals:
  - its stream is reset after its header arrived;
  - the track ends;
  - newer content on the same track runs the reader's `max_age` past where the missing group
    would end, measured in the track's own timestamps.
- **The budget is the reader's.** It is 100 ms by default on the relay source node, and the
  track's declared retention for `tatolab-read-track`. The reader subscribes with that same
  budget and joins at the live edge.
- **Losses are visible.** The relay source node writes each group it gives up as a loss its
  readers count in `lost_since_last_read`, so a decoder downstream conceals it as it would a
  loss on a link.
- **How a given-up group is carried.** A given-up group counts as one. It travels in its own
  user-header field beside the sequence number, "messages lost upstream before this one". An
  `ordered` reader adds that field to `lost_since_last_read`, never to its link's dropped
  count. The relay source node counts its give-ups on itself, per output port.
- **The catalog entry stays.** `AudioBlock` keeps `binary` / `mode: snapshot`, and no reader in
  the runtime consults a data track's `mode`. Publishing decoded audio as `audio` / `pcm` is
  OPEN.
- **Not a playout buffer.** The budget is a transport budget. A playout or jitter buffer is a
  separate, later concern at the playing sink.

## Rejected alternatives

- **moq-net's sequence cursor (`.ordered()`).** It hands over the lowest *cached* group above its
  position and never waits for a missing one. moq-lite sends the newest of two ready groups
  first, so a group whose stream lands after its successor's is stepped over for good, at any
  budget.
- **The default budget of zero.** At zero a group is stale as soon as its successor shows a
  frame. Two groups landing in the same millisecond, which is ordinary scheduling jitter, keep
  only the newer one: 9 % of per-block audio groups were lost on loopback.
- **Giving up on a clock.** moq-dev's own consumer also gives up once the budget elapses in
  monotonic time. On a live track, content time and real time advance together, so the clock
  adds nothing during normal running. It differs only when a publisher pauses with a group
  missing, and then the content-time rule holds the groups behind the gap until content resumes
  or the track ends. Nothing is lost, only delayed. Waits and sleeps never fix realtime
  streaming.
- **One budget for every reader, the track's 5 s retention.** One group that really goes missing
  would become 5 s of silence and then a 5 s burst, which a playing sink either plays late from
  then on or discards. How late a group may be and still be worth using depends on the reader:
  a recorder wants everything, a speaker wants it on time.
- **A transport signal that a group is gone.** None reaches a reader in every case:
  - QUIC does not order streams.
  - A stream reset before its header arrives leaves no trace.
  - lite-06's `SUBSCRIBE_DROP` is never sent by the pinned `moq-net`.
  - MoQT says a subscriber cannot infer a group exists until an object of it arrives.

  So a budget cannot be avoided, only owned by the right party.

- **Giving up a group silently.** A decoder after the relay source then sees the next group
  arrive as if nothing were missing, never conceals the gap, and plays a step in the audio.
- **Skipping numbers in the existing sequence to declare the loss.** There is no wire change,
  but the loss would land on the downstream link as if that link had dropped it. It would read
  the same as a ring overwrite, and it would vanish when it rode a link's first sample, which is
  the baseline gaps are measured from.
- **Counting a given-up group as the objects seen of it, plus one.** The bookkeeping buys a
  number that is still a guess when a group's header never arrived. One per group is exact for
  audio and a lower bound for video, and a decoder reacts the same to any nonzero count.
- **Re-describing `AudioBlock` now.** hang has no data mode for one record per group: `snapshot`
  is lossy latest-value, and `stream` forbids a second group. `mode` is mandatory, and an
  unknown value makes a conforming reader ignore the track. A player plays the Opus track,
  which is already an `audio` entry. Publishing raw PCM instead changes an exposed port's wire,
  and nothing in the MVP needs it.

## Consequences

- **Latency.** The hold adds no latency while groups arrive in sequence. A group that really
  goes missing costs its reader at most `max_age` of delay, once.
- **The default.** 100 ms is several times the worst lateness measured on loopback: groups
  arrive one late, two at most, within the same millisecond. A reader across a WAN, where
  reordering grows with round-trip time, raises it.
- **Clocks.** The rule compares a track's timestamps only with that track's own, so a common
  clock across machines leaves it unchanged.
- **A pause.** A publisher that pauses while a group is missing holds up to `max_age` of
  content behind the gap until it resumes.
