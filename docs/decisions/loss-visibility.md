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
  tracks the last number per producing publisher and adds any jump to its inbound link's
  dropped-bag count, on `ordered` ports.
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
  thread per helper, and counts since the last report die with a crashed helper. A blackboard
  the parent reads survives the crash and never waits on the child. Its one sharp edge is that
  a dead writer's entries stay locked, so each spawn gets a fresh board.
- **Leaving the flush uncounted.** It was a stated, bounded loss, but it is still loss a
  reader cannot see, and audio into slow inference is exactly where it happens.
- **Sizing the windowed ring to the contract's depth.** iceoryx2's bookkeeping memory scales
  with depth and is committed up front. At a one-second window's depth of about 8,000 slots a
  publisher takes 141 MiB of heap and every connection 17.7 MiB. At a fixed cap of 64 it is
  1.45 MiB, with 640 ms of headroom at 10 ms blocks.

## Consequences

- The iceoryx2 wire gains 8 bytes per sample and a user-header type every opener shares. The
  engine and the wheel change together.
- A loss after a link's last receive and before its disconnect stays uncounted.
- The same sequence number is what a remote link across the runtime mesh counts its hop
  losses by.
- A windowed port connected live onto a small channel is refused until overwrite counting has
  shipped, then revisited.
