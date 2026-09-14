# Counting the losses no port mailbox sees

Rationale for the `[loss-visibility]` entries in `docs/plan/ARCHITECTURE.md` §Processor model
and §Media I/O, decided 2026-09-14.

## Trigger

Read this before adding a loss counter anywhere on the data plane, before putting anything
new into the frame header, and before choosing how a helper process reports a number to its
parent.

## Decision

- **Ring overwrites.** A bag the iceoryx2 subscriber ring overwrites is counted by a
  per-channel 64-bit sequence number carried in an iceoryx2 user header. Each subscriber
  tracks the last number per producing publisher and adds the numbers a jump skips
  (`next − last − 1`) to its inbound link's dropped-bag count, on `ordered` ports. A new
  producing publisher starts a new baseline rather than a gap, and a 64-bit counter does not
  wrap within any run.
- **Helper counts.** A helper-placed destination's counts reach `graph` through one
  blackboard per helper spawn. The helper writes it and the parent reads it.
- **Windowed audio.** A windowed audio port's discontinuity flush counts the samples it
  discards. The ring in front of a windowed port is engine-sized to a fixed cap, and a live
  windowed connect onto a smaller channel is refused by name.

## Rejected alternatives

- **Reading iceoryx2's own delivery result.** A send counts a subscriber whose ring just
  overwrote a bag as reached: ten sends into a four-slot ring reported delivery every time
  while six bags vanished. Neither iceoryx2 0.9.3 nor its unreleased main reports an
  overwrite at all.
- **The sequence number in the frame header.** The 76-byte layout is an agent-documented tap
  contract pinned by a test. A user header sits outside the payload, so tap clients, which
  never open iceoryx2, are untouched.
- **Counting on `newest` ports too.** A `newest` port passing over bags is the profile
  working and is deliberately uncounted.
- **A periodic report op over the escalate socket.** Simpler, but it rides the one reader
  thread per helper, and counts since the last report die with a crashed helper. On a
  blackboard the parent reads, the last counts a crashed helper wrote stay readable, and the
  parent never waits on the child. Its one sharp edge is that a dead writer's entries can no
  longer be written by anyone, so each spawn gets a fresh board and the crashed spawn's final
  counts are read from the old one.
- **Leaving the flush uncounted.** It was a stated, bounded loss, but it is still loss a
  reader cannot see, and audio into slow inference is exactly where it happens.
- **Sizing the windowed ring to the contract's depth.** Ring slots hold whole bags. A
  windowed port's mailbox is sized for the smallest device quantum the stage accepts
  (125 µs), so a one-second window asks for about 8,000 bags. iceoryx2's bookkeeping memory
  scales with depth and is committed up front: at 8,000 slots a publisher takes 141 MiB of
  heap and every connection 17.7 MiB. A fixed cap of 64 bags costs 1.45 MiB. The cap is not
  meant to hold a whole window. It is headroom for a stalled `process()`: 640 ms at the common
  10 ms block. Anything that still overflows is counted.

## Consequences

- The iceoryx2 wire gains 8 bytes per sample and a user-header type every opener shares. The
  engine and the wheel change together.
- A loss after a link's last receive and before its disconnect stays uncounted.
- A remote link counts its hop losses the same way, by a gap in a sequence number. The
  sending runtime carries that number in each mesh message's own metadata, never in the bag,
  with the sending runtime and port as its identity and a fresh baseline whenever the remote
  link re-wires. Its exact encoding is the runtime-mesh change's to specify.
- A windowed port connected live onto a small channel is refused until overwrite counting has
  shipped, then revisited.
