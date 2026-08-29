# audio-port-window-contract

The windower rung. An audio input port declares the rate, channels, dtype, window size
and hop it wants, and the engine resamples, mixes down and frames natively so `process()`
receives exact-size timestamped blocks — the thing that makes a Silero or WebRTC-VAD
consumer ordinary user code instead of a private buffering exercise. Delta against
`../ARCHITECTURE.md` §Media I/O, with consequential edits in §Processor model & scheduling
where two of its absolutes collide with the contract. Conditioning, barge-in and audio
plugins are later rungs; they are not in it.

**Scale gate — a changed contract on the Python API's public declaration surface and on
the processor model, so this skill plus an ADR.** The ADR trigger fires and is already
satisfied: `docs/decisions/audio-subsystem.md:67-74` names the window contract "the API
centerpiece", surveys what every other framework does instead (openWakeWord buffers
internally, Pipecat chunks in transports, none puts window/hop in the port contract) and
states the mechanism as "engine-inserted resample, mixdown, framing". Its
rejected-alternatives list already rules out solving an adjacent problem with a spliced
graph block (`:135-137`). Nothing below decides past it.

**Precondition.** The window-contract entry (`ARCHITECTURE.md:833-838`) is **DECIDED**
under `[audio-subsystem]`, unbuilt — three code sites say so in as many words
(`core/context/audio_device_backend.rs:266`, `linux/alsa_audio_device_backend.rs:117`,
`linux/pipewire_audio_shim.c:53-54`). §Processor model's port entries (`:219-296`) are
DECIDED too, and two of their absolutes contradict it head-on. That collision was
brought as a decision and the owner resolved it 2026-08-29 — see the RESOLVED block
below.

---

## MODIFIED: §Media I/O — the declaration, spelled

Five fields on an input port, beside `delivery_profile`. Worked spelling in both
languages, because the parity bar makes the Python one the contract:

    @input("audio", delivery_profile="ordered",
           audio_window=AudioWindowContract(sample_rate=16_000, channels=1,
                                            dtype="f32", window_size=512, hop=512))
    def audio_from_microphone(self) -> None: ...

    input("audio", delivery_profile = "ordered",
          audio_window(sample_rate = 16_000, channels = 1, dtype = "f32",
                       window_size = 512, hop = 512)),

It rides the same three carriers `delivery_profile` already rides, as an optional struct
rather than five loose fields: `ProcessorPortSchema` (`processor_schema.rs:374-388`),
`PortDescriptor`, and `PortInfo` (`core/graph/nodes/port_info.rs:10-26`). The Python
validator sits beside the profile check (`_processor_declaration.py:41-72`), the Rust one
beside the grammar's (`sdk/streamlib-macros/src/grammar.rs:317-386`), and the unknown-key
error there gains the new key by name.

Field semantics, resolved from the tree rather than invented:

- `sample_rate`, `channels` and `dtype` reuse the existing device vocabulary —
  `AudioStreamFormat` and `AudioSampleFormat` (`core/context/audio_device_backend.rs:19-57`)
  — never a parallel spelling. `dtype` takes the two `AudioBlock` already legalises,
  `f32` and `i16`.
- Channel conversion runs both directions, by fixed rule: N→1 averages, 1→N duplicates,
  and any other N→M is refused at the stage naming both counts. "Mixdown" alone would
  exclude the rung's own flagship case — a mono capture feeding a sink whose device
  resolved stereo is an up-conversion.
- A window contract requires `delivery_profile = "ordered"`, refused at declaration in
  both languages naming both knobs — the same shape as the hop refusal. `newest` resolves
  to skip-to-latest (`iceoryx2/read_mode.rs`, `mailbox.rs` `pop_latest`), which passes
  over bags by design; an accumulator needing contiguous samples would flush on nearly
  every read and, for a window wider than one device quantum, might never emit at all.
  The ADR already says `newest` is wrong for audio.
- A windowed port accepts exactly one inbound link, refused at wire time naming the
  port and both links. Fan-in legally interleaves N producers' blocks in one mailbox
  today (`input.rs:270-288`); two sample streams interleaved into one accumulator is
  plausible-looking wrong audio, the outcome this file already names as the worst one.
- `window_size` counts **per-channel** samples, the unit `AudioBlock.sample_count` already
  uses, so an emitted window carries `window_size × channels` scalars.
- `hop` defaults to `window_size`: contiguous, non-overlapping windows. A hop below
  `window_size` is a rolling window and is legal. A hop above it would silently discard
  samples between windows and is refused at declaration, naming both numbers.
- Every numeric field is strictly positive, refused at declaration naming the field and
  the value — a zero `hop` makes no framing progress, a zero `sample_rate` resamples to
  nothing, and Python's declaration path would otherwise carry either straight to the
  engine.
- The contract is all-or-nothing. There is no partial form, because a half-declared
  contract leaves the stage guessing at exactly the values a model asserts on — and a
  guess that is usually right is the worst outcome available.
- A port with no `audio_window` is unchanged in every respect. This is opt-in; nothing
  about a video port, a control port, or an audio port that wants raw device blocks moves.

## MODIFIED: §Media I/O — the stage, and where it runs

**One stage, at the one read seam every reader already shares.**
`InputMailboxesInner::read_raw_bounded` (`iceoryx2/input.rs:433-482`) is read by an
app-process Rust processor through the parent's mailboxes and by a helper-placed Python
processor through its own, which the child opens for itself
(`sdk/streamlib-python-wheel/src/python_processor_link_data_access.rs:41,125,264`). A stage
there is one implementation serving both, with no new IPC hop and no parent↔child
contract to design. That matters more than it sounds: every Python processor is
helper-placed, and a Python consumer is who this contract exists for.

**The stage is not a pure transform.** Windowing is N-in → M-out — one 1024-sample
PipeWire quantum satisfies two 512-sample windows, and a one-second rolling window needs
about forty-seven of them. So the stage owns a per-port accumulator sitting between the
mailbox and the reader, and four consequences follow:

- **Readiness means a full window, not an arrived bag.** `has_data` and
  `any_port_has_data` (`input.rs:499-520`) drive the reactive scheduler, and the DECIDED
  entry promises `process()` receives exact-size blocks. A windowed port therefore reports
  data only when a full window can be emitted. Read off the plan, not chosen: a reactive
  `process()` that woke and found nothing would contradict the entry implementing it.
  The gate lands in two dispatch sites, not one: the helper loop already gates every
  dispatch on readiness (`_helper.py:615`), but the app-process reactive runner calls
  `process()` unconditionally on every listener wake and consults `any_port_has_data`
  only to continue draining (`core/execution/thread_runner.rs:283-307`) — it gains the
  same gate. The drain loop then dispatches once per ready window, so one 1024-sample
  quantum against a 512/512 contract dispatches twice and a ready window never sits
  latent waiting for the next bag.
- **The order of operations is fixed**: decode to f32 → channel-convert → resample →
  frame → encode to the declared dtype. Internal arithmetic is f32 always — the
  resampler speaks nothing else — with an `i16` contract encoded back saturating, never
  wrapping. Resampling after channel conversion converts one channel's worth of samples
  rather than N, and framing before the encode is the only order that can emit an exact
  window.
- **The stage derives a stamp; it never reads a clock.** One device stamp anchors each
  contiguous run — taken from the first block after start or after a flush — and every
  window's `first_sample_timestamp_ns` is that anchor plus the emitted-sample offset in
  integer rational arithmetic (`anchor + emitted × 1_000_000_000 / out_rate`, widened),
  minus the resampler's reported group delay. Never an accumulated per-sample delta,
  which drifts at 44.1 kHz-family rates; never re-anchored per block, whose
  status-derived stamps jitter below sample exactness. `ARCHITECTURE.md:736` — the
  device stamps the block and the engine never re-stamps it — survives intact, because
  deriving offsets from a device stamp is not re-stamping. Block-level A/V sync stays
  subtraction, and the exactly-32-ms validation below holds within a contiguous run,
  which is what its test asserts.
- **No sample is invented to bridge a gap.** The drop-at-the-edge entry (`:749-751`)
  states that nothing is silently interpolated, and a resampler is exactly the machinery
  that could quietly repeal it. A discontinuity — a block's stamp missing its expected
  position (previous stamp + `sample_count / rate`) by more than half a source quantum,
  a tolerance because status-derived device stamps jitter below sample exactness —
  **flushes the accumulator and the resampler's own filter state**, then re-anchors on
  the next block's stamp. The filter reset is load-bearing, not hygiene: a polyphase
  resampler holds a filter's length of pre-gap samples, and emitting through it after
  the gap blends audio across the loss — the interpolation the entry bans. The same
  doctrine settles the priming question at stream start and after every flush: filter
  output produced before the filter has filled is zero-padding, not audio, so it is
  discarded — an emitted sample always derives from real input — and the group-delay
  subtraction then aligns the first emitted stamp with the real input sample it derives
  from. The gap stays derivable from the stamps either side, as that entry requires.

**Three wiring facts the tickets should not have to rediscover.** The `AudioBlock`
cast lives in `streamlib-media-builtins`, which depends on the engine — so the stage
owns its own decode of the six wire keys and re-encodes each emitted window as an
ordinary `AudioBlock` bag, which is also what keeps `read(into=AudioBlock)` and Rust's
`read::<AudioBlock>` working unchanged. The contract reaches a helper child over the
same parent→child wiring envelope that already carries `read_mode`
(`python_processor_link_data_access.rs:115-128`), or the child's stage windows nothing.
And a stream that simply stops leaves under one window of samples parked in the
accumulator, delivered to nothing — designed, not a defect: an exact-size contract has
no partial form to hand over.

**A bag the stage cannot read is refused by name at the read** — a `dtype` it does not
know, a payload whose length is not `sample_count × channels × itemsize`, a bag with no
`AudioBlock` keys at all — the way the Python cast already refuses
(`test_audio_block_cast.py`). Never reshaped into a plausible wrong answer, and the
refusal names the port.

**Counting is unchanged and the accumulator is not a second drop site — which pins the
ingestion discipline.** Bags stay in the counted mailbox until `read` consumes them into
the stage; the accumulator holds only the already-consumed resampled remainder, under one
window's worth, and never evicts. Readiness is computed jointly — queued bags' sample
counts plus the remainder — not by draining the mailbox at `has_data`, because an eager
drain would starve the #2023 per-link counters (`mailbox.rs:79-87`) exactly where loss
happens and grow the accumulator unboundedly under a stalled consumer. What forces the
depth question into the open: `ORDERED_DEPTH` is 16 (`delivery_profile.rs:57`) and a
one-second rolling window at a 1024-sample quantum needs ~47 queued blocks, so the
engine sizes a windowed port's mailbox from its contract
(`ceil(window / quantum) + margin`) — still engine-chosen, still not authorable; the
contract is a declaration, not a depth dial. Overflow past that depth is a counted
mailbox eviction, same counter, same `graph` surface. A discontinuity flush discards
the remainder — under one window of samples, not a bag, not counted as one — logged
with the port and the sample count; a bag evicted at a windowed port therefore costs
its own samples plus the flush of the remainder behind it, which belongs beside the
plan's OPEN no-loss-is-silent entry (`:267-277`) as a stated, bounded loss shape.

**The resampler is `rubato`** — a new dependency (no resampler crate exists anywhere in
`Cargo.lock`), pure Rust, MIT, adding no `DT_NEEDED` entry, which
`test_wheel_portability.py` enforces regardless of what anyone believes about a crate.
Three adapter obligations ride it: fixed input-chunk sizes (`input_frames_next`), planar
rather than interleaved buffers (de-interleave after the channel convert), and the
group-delay / reset seams the stamp and flush bullets above already bind.
**Stated as an assumption rather than brought as a decision**: choosing a library inside a
ticket is pattern choice, not architecture (owner, 2026-08-13), so unless you say
otherwise the ticket takes `rubato` and proves the portability gate stays green. The
alternative is hand-rolling a polyphase resampler, which buys a maintenance burden and
no capability.

## MODIFIED: §Processor model — the port declares a fourth thing, and the read path reads one

Two DECIDED entries in §Processor model contradict the DECIDED window-contract entry in
§Media I/O. This is not ambiguity to interpret — the plan states both, and one must yield.

> `:219-221` — "A port declares three things and nothing else: name, description, and —
> on an input — delivery profile."
> `:320-322` — "Port rendering in the control plane is name, description, delivery
> profile, and direction."
> `:211-216` — "The engine has no type layer … no read path examines a tag."
> `:292-294` — "The engine's whole role is to count a drop at the port that dropped it and
> surface it; it never inspects a payload."

versus

> `:833-835` — "An audio input port may declare a window contract — rate, channels, dtype,
> window size, hop — beside its delivery profile … the engine resamples, mixes down, and
> frames natively."

A windower must decode `samples`, `sample_rate`, `channels` and `dtype` out of a bag. That
is inspecting a payload, at a read path, driven by a fourth thing the port declared.

**RESOLVED (owner, 2026-08-29): (a) — §Processor model yields; the exception is named
and bounded.** The port declares three things *plus an optional window contract on an
audio input*; rendering becomes five; and the no-payload-inspection absolute gains one
carve-out — declaring a window contract **is** the port's opt-in to the engine reading
its bags as `AudioBlock`, and the engine inspects a payload on exactly the ports that
asked it to and nowhere else. Links with no contract stay pure plumbing, `connect` still
compares nothing, and the frame header still carries no schema ident. The ship fold
amends the four §Processor model absolutes (`:219-221`, `:320-322`, `:211-216`, `:292-294`)
with this carve-out, in those words.

Rejected, for the record: moving the contract onto processor config reverses the DECIDED
§Media I/O entry and strands the user-Python case (a user processor has no built-in to
configure); a compiler-spliced native node on the link needs a node-insertion capability
no compiler op has, adds an IPC hop per windowed link, surfaces in `graph`/`tap`
topology, is nearly incompatible with the `match_device` resolution below (a `setup()`
re-resolution would mean re-splicing topology at runtime), and the ADR already rejected
a spliced block for the adjacent conditioning case on alignment grounds
(`audio-subsystem.md:135-137`).

## MODIFIED: §Media I/O — SpeakerSink's refusal becomes a conversion

`SpeakerSink` today refuses any block whose rate, channels or dtype the device cannot play
(`speaker_sink.rs:299-322`), and the refusal text says why: *"There is no resampler on
this rung, so the block is refused rather than adapted."* This rung is that resampler,
and the mic-to-speaker rate mismatch is the plainest case the contract exists to fix —
`PREFERRED_CAPTURE_CHANNELS` is 1 and `PREFERRED_PLAYBACK_CHANNELS` is 2
(`alsa_audio_device_backend.rs:112-118`), so the two built-ins disagree by design today.

The obstacle is that `SpeakerSink`'s target format is not knowable when its port is
declared: it comes from `playback_stream.stream_format()` after `setup()` opens the device
(`speaker_sink.rs:130`), and it varies by machine.

**RESOLVED (owner, 2026-08-29): (c) — `setup()`-time resolution, spelled as a
declaration sentinel.** The port's declaration itself carries
`audio_window = match_device`; the contract resolves at `setup()`, where the typestate
is Full — the same phase in which a processor requests a window (`:591`) — from the
format the device stream just opened. Only a processor that opens a device stream can
satisfy the sentinel; the `setup()` setter is the engine-internal mechanism, never
public surface, and it is deliberately not exported to Python — the parity disposition,
named: a Python processor's window is its model's compile-time knowledge, and it holds
no machine-varying device format to resolve. `graph` renders the resolved values —
machine-dependent because the device format is, which is truer than a static lie.
`SpeakerSink` declares `audio_window = match_device` with window = hop = one device
period — it wants format conversion, not framing, and under all-or-nothing that is how
a converter is spelled. `refuse_a_block_the_device_cannot_play` then deletes: the stage
converts, and the sink plays.

Rejected, for the record: static-only ships the rung with its flagship case unsolved
(mono-preferring mic into a stereo-preferring speaker fails on a stock machine, with no
user-authorable fix by design); a bare public setter puts a dynamic-contract API on the
declaration surface where any processor could reach it, and leaves the declaration site
silent about a resolution the reader needs to know happens.

## REMOVED:

- REMOVED: refuse_a_block_the_device_cannot_play

**The bullet stands**: decision 2 resolved to the sentinel, whose mechanism deletes the
refusal — the stage converts, so a block the device cannot play as it stands stops being
refusable and starts being playable. Nothing else in this delta is a deletion: the rung
is additive. The three "no resampler on this rung" comments
(`audio_device_backend.rs:266`, `alsa_audio_device_backend.rs:117`,
`pipewire_audio_shim.c:53-54`) become false the moment the stage lands and are corrected
in the PRs that falsify them — ordinary doc hygiene inside work already being done, not
gate bullets, since the gate's grammar wants artifacts rather than prose.

## Not in scope

- **Conditioning, AEC, AGC, noise suppression and barge-in** — the next rung, and its own
  `/propose-change`. It shares no code with this one: conditioning sits between device and
  published block inside a built-in, the windower sits at a consuming port.
- **Feature extraction** — mel, MFCC and friends stay out, permanently. The contract ends
  at windowed raw samples (`ARCHITECTURE.md:837-838`, `audio-subsystem.md:133`).
- **Output-side window contracts** — an output port declares no contract; a producer
  publishes what it has. Only a consumer states what it needs.
- **Video** — no framing, no rate conversion, no contract on a video port.
- **`packages/` and `examples/`** — lag by design.

## Validation

- **The declaration round-trips**: a contract declared in Python and one declared in Rust
  produce the same `ProcessorPortSchema`; a hop above `window_size`, a zero or negative
  value in any numeric field, and a contract beside `delivery_profile = "newest"` are
  each refused at declaration in both languages naming the numbers or knobs. An N→M
  channel pair with neither side 1 is refused at the stage naming both counts — the
  source count arrives with the bags, so declaration cannot see it; a second inbound
  link into a windowed port is refused at wire time.
- **The stage is exact**: a 48 kHz stereo `f32` source into a port declaring
  16 kHz / 1 / `f32` / 512 / 512 yields blocks of exactly 512 per-channel samples, at
  16 kHz, mono, with stamps advancing by exactly 32 ms — asserted on a known signal, not
  on block counts.
- **A rolling window overlaps correctly**: hop 160 against window 512 yields windows whose
  contents overlap by 352 samples, proven by comparing sample values across consecutive
  windows rather than by counting.
- **Readiness is a full window**: a reactive processor on a windowed port is never
  dispatched with nothing to read — in the app-process runner and the helper loop both —
  and one 1024-sample quantum against a 512/512 contract dispatches `process()` exactly
  twice.
- **A discontinuity flushes rather than interpolates**: a gap in the input stream produces
  no window spanning it, the window after the gap carries its own first sample's stamp,
  and that first post-gap window contains no pre-gap energy — proven on a known signal
  against the pinned `rubato`, which is the assertion that catches a missed filter reset.
- **Nothing else moved**: a port with no contract reads byte-identical bags before and
  after, and the per-link drop counts on a windowed port match an unwindowed one under the
  same overrun.
- **The wheel still links nothing new**:
  `test_wheel_portability.py::test_the_native_extension_links_nothing_the_host_may_not_supply`
  stays green with the resampler in.
- **Audio still proves itself on the rig**: `/verify-audio` passes after the rename of
  nothing — the loopback fixture asserts content, and it must assert it through a windowed
  port too (wheel rebuilt first).
- Mechanical: `cargo xtask check-all-source-gates` green; wheel pytest suite green;
  `cargo check --target aarch64-apple-darwin`.
