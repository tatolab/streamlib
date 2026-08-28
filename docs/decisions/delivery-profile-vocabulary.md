# Delivery profile vocabulary

Rationale for the `[delivery-profile-vocabulary]` entries in `docs/plan/ARCHITECTURE.md`
§Processor model & scheduling, decided 2026-08-28.

## Trigger

Read this before adding a delivery profile, before adding any port-local knob for depth
or queueing, and before writing a doc, log line, or test name that says a link delivers
every bag.

## Decision

A delivery profile names a **read policy and nothing else**. There are two: `newest` —
the consumer drains to the most recent bag — and `ordered` — the consumer receives bags
in publication order. Both drop under sustained pressure, and every drop is counted by
the port that dropped it and readable in `graph`.

Ring depth and overflow policy are engine-chosen and unauthorable. No port declares a
depth, a leak policy, or a queue element, and no second surface tunes one.

`lossless` is retired. So is `every_sample`, for the same defect in weaker form.

## Why `lossless` could not stay

It did not work, and the shape of the failure is not a bug to fix at the margin.

`Lossless` resolved to `Overflow::Block`, which sets iceoryx2's service-level
`enable_safe_overflow = false` — leaving full-buffer handling to the publisher's
`unable_to_deliver_strategy`, an upstream default the engine never set, which may block
or may error (see §Producer blocking below). Even that never engages, because the
subscriber's shared-memory ring never fills. `InputMailboxesInner::receive_pending` drains it
**completely** into a host-side `PortMailbox` on every call, and the reactive runner calls
it between every `process()` invocation. `PortMailbox::push` then evicts its oldest entry
unconditionally, whatever profile the port declared.

So the producer never blocked, and the loss simply moved to a place nothing was counting.
Measured while building the audio playback path: a producer publishing about a thousand
blocks a second reached its consumer as **78 of 378**.

The deeper reason the word cannot be rescued by fixing the mailbox: the head of a
live-capture graph is a device — a camera, a microphone — that will not wait. Backpressure
there does not prevent loss, it relocates it to the device edge. `MicrophoneSource`
already drops and counts there deliberately, and that is the honest place for it. A
port-local declaration cannot promise delivery on a link it does not control the head of.

## Why `every_sample` went too

It carries the same defect in weaker form: the word says *every*, and under pressure it is
not every. Retiring one over-promising name while keeping its sibling would have left the
next author to rediscover the same gap. `newest` / `ordered` name the axis that actually
exists — which bag do I get next — and promise nothing about how many.

`latest` was renamed alongside it for a smaller reason: latest *what* — arrival or
timestamp? `newest` reads as arrival order, which is what it is.

## Why depth stays engine-chosen

The reachable alternative was GStreamer's `queue` element — `max-size-buffers` /
`max-size-bytes` / `max-size-time` plus `leaky=no|upstream|downstream` — and the owner
named it as what the retired profile was reaching for. It is rejected on usability:
"where do I need a queue?" is a well-known GStreamer wart, and the one-word port surface
was chosen deliberately against it. Depth becomes a number every author must reason about
and most will get wrong, in exchange for tuning that only a minority of links want.

Depth stopped being expressible when `read_mode` / `overflow` / `buffer_size` /
`max_queued_messages` were collapsed into one word (`c949feee`). That collapse stands;
this decision is the plan catching up to it rather than reversing it.

## Why loss must be counted

The 78-of-378 run and a healthy run were indistinguishable from outside the process. That
is the actual defect the vocabulary was hiding: not that bags were dropped — dropping is
correct on a realtime link — but that dropping was invisible.

So a drop is a normal, reportable event: counted at the port, surfaced in `graph`, never
an error and never silent. `ProcessorMetrics` already carries a `frames_dropped` field and
is never inserted on any node, so no processor reports anything today; wiring it is the
work this decision commissions.

An eviction counter alone was considered and deliberately not built ahead of this
decision: the cheap half survives any design, but the valuable half — surfacing the count
— had no path until this decision gave it one, and a counter built first would have
landed somewhere the redesign might not keep.

A port's dropped-bag count and a **tap's** `dropped_bags` are different subjects: the
former is data-plane loss on a link, the latter is what the tap's own reserved subscriber
slot missed while trading completeness for non-interference. They stay separate counters
with distinct names and are never summed.

## Producer blocking is deleted, not stranded

With no profile resolving to it, `Overflow::Block` is dead machinery, and the doctrine
bans that. But the stronger reason is that the capability was never engineered:

- The engine never sets iceoryx2's `unable_to_deliver_strategy` — `create_publisher` sets
  only slice length and allocation strategy. The blocking behaviour was whatever the
  upstream library defaulted to, and the engine's own test says so in as many words: the
  strategy "may yield either a `Block` (send blocks until consumer drains) or a non-`Ok`
  return". A capability whose core semantics the engine never chose is not a capability.
- A producer parked in `send()` cannot observe shutdown — the thread runner checks its
  shutdown channel only between `process()` calls. A parked publisher holds posix-shm
  connection state that wedges every other test in the suite.
- Keeping the word alive already cost two standing workarounds: `MicrophoneSource`'s
  two-second detach-and-leak grace, whose comment names the cause exactly ("a consumer
  whose port declares `lossless` can hold that thread inside `write` for as long as it
  likes"), and the tap's always-drain/never-park forwarder, which exists because a parked
  tap on such a channel would back-pressure the source.

Deleting it also retires a class of ambiguity: `enable_safe_overflow` is service-level, so
a channel feeding both a dropping and a blocking destination was genuinely undecidable.
With one overflow policy engine-wide, multi-destination conflicts reduce to depth and
drain order.

**Sequencing.** Counted drops land before or with the deletion. Removing blocking while
consumer-side eviction is still silent would remove the only alternative to silent loss
while the loss is still silent. `SpeakerSink` and both audio rings redesign in the same
motion — their own docs say a dropping ring "would make `lossless` a lie", which stops
being a defect the moment the word is gone.

## Where loss belongs

Deleting blocking moves loss placement from the engine to the endpoints, and that is the
whole of the design. The cost is real and named: on camera → encoder → writer, blocking
used to propagate a stall past the encoder so loss landed on raw frames and everything
reaching the file stayed decodable. Dropping at the writer's input instead drops encoded
frames, and a lost reference frame corrupts the stream to the next sync point.

Every serious system solves this the same way, and none of them solve it by dropping
encoded frames blindly. WebRTC lands loss pre-encode through several layers — rate
feedback that keeps the encoder from sustained overproduction, a frame dropper that skips
raw frames when it overshoots, replacement of a pending raw frame by its successor, then
resolution and framerate adaptation — and only past all of that does it accept loss and
recover with a keyframe request. GStreamer sends lateness upstream as QoS events and its
video encoder drops late raw frames before encoding them. MoQ forbids a relay dropping
inside a group; publishers pre-declare grouping and priority and the relay enforces them
without understanding the media. CMAF's atomic unit is the fragment, which is the group.

The transferable set is therefore: producer-declared drop semantics, a consumer→producer
resync signal, and closed-loop pressure feedback. Explicitly not transferable — NACK/RTX,
FEC, RED, jitter buffers. Those compensate for a lossy, reordering network; an in-node
link has neither, and importing them would be insuring against our own queue.

Two shapes were considered for carrying the producer's knowledge and one was rejected:

- **Bag fields the producer writes and the consumer casts** (`is_keyframe`, a group index,
  a sequence number) — chosen. The encoder already has all of it for free; the engine
  stays blind; and the fields map onto MoQ's group and object ids at the wire, where a
  processor does the mapping and is entitled to read content. Reading them is not
  optional on the consuming side: a consumer of an encoded stream that sees a gap must
  discard to the producer's next sync point — no system drops or forwards encoded frames
  blindly, and neither does any StreamLib consumer.
- **Bits in the frame header the engine acts on** — rejected. It fails mechanically before
  doctrine is even reached: the ring is 16 deep and a group is 30–120 frames, so
  group-aware eviction inside the ring degenerates to evicting everything, and repairing
  that would require the authorable depth this decision just refused. It would also couple
  the frame header to a transport whose plan section is still OPEN.

## Rejected alternatives

- **Keep `lossless` and make it true** — a bounded mailbox that blocks rather than evicts.
  Rejected: it lets a slow Python helper stall a camera's capture thread, and makes a
  graph with a cycle deadlockable. Isolation is the optimised axis; no processor may
  block another.
- **Keep the word, redefine it** as in-order + deepest ring + counted, with no blocking.
  Rejected: zero churn, but the word still tells every reader who has not read the source
  that nothing is lost.
- **A third profile for the muxer / file-writer case** — a genuinely blocking profile for
  sinks that can legitimately wait. Rejected as premature: no consumer demands it today,
  and when one does we will know whether it wants blocking or a journal spill.

## Consequences

- `Overflow::Block` and the blocking publish path are deleted, along with the
  overflow-disabled back-pressure test that locked their contract. The tap's mirrored
  `enable_safe_overflow` field and the hazard it defended against retire with them.
- Three engine declaration sites move: `speaker_sink.rs` (`lossless` → `ordered`),
  `python_test_harness_endpoints.rs` (`every_sample` → `ordered`), and `display_window.rs`
  plus the `test_support.rs` / macro-doc sites (`latest` → `newest`).
- The Python `_DELIVERY_PROFILES` constant, `DELIVERY_PROFILE_DECLARATION_VALUES` in the
  processor-schema crate, and the error text listing legal values all move with them.
- Pre-1.0: this is a clean rename, not a deprecation. No aliases, no back-compat shims.
- `packages/` and `examples/` lag by design and are not in scope.
