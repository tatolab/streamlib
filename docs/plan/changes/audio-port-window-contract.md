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

**Precondition.** The window-contract entry (`ARCHITECTURE.md:798-803`) is **DECIDED**
under `[audio-subsystem]`, unbuilt — three code sites say so in as many words
(`core/context/audio_device_backend.rs:266`, `linux/alsa_audio_device_backend.rs:117`,
`linux/pipewire_audio_shim.c:53-54`). §Processor model's port entries (`:222-244`) are
DECIDED too, and two of their absolutes contradict it head-on. That collision is
`[NEEDS DECISION] 1` below; this delta does not resolve it.

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
- `window_size` counts **per-channel** samples, the unit `AudioBlock.sample_count` already
  uses, so an emitted window carries `window_size × channels` scalars.
- `hop` defaults to `window_size`: contiguous, non-overlapping windows. A hop below
  `window_size` is a rolling window and is legal. A hop above it would silently discard
  samples between windows and is refused at declaration, naming both numbers.
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
  data only when its accumulator can yield one. Read off the plan, not chosen: a reactive
  `process()` that woke and found nothing would contradict the entry implementing it.
- **The order of operations is fixed**: dtype-decode → mixdown → resample → frame.
  Resampling after mixdown converts one channel's worth of samples rather than N, and
  framing last is the only order that can emit an exact window.
- **The stage derives a stamp; it never reads a clock.** An emitted window's
  `first_sample_timestamp_ns` is its source block's stamp plus the per-sample offset of
  the window's first sample, computed at the output rate. `ARCHITECTURE.md:745-752` — the
  device stamps the block and the engine never re-stamps it — survives intact, because
  deriving an offset from a device stamp is not re-stamping. Block-level A/V sync stays
  subtraction.
- **No sample is invented to bridge a gap.** The drop-at-the-edge entry (`:756-758`)
  states that nothing is silently interpolated, and a resampler is exactly the machinery
  that could quietly repeal it. A discontinuity — a timestamp gap wider than the samples
  in hand account for — **flushes the accumulator** and starts a new window at the next
  block's own stamp. The gap stays derivable from the stamps either side, as that entry
  requires.

**A bag the stage cannot read is refused by name at the read** — a `dtype` it does not
know, a payload whose length is not `sample_count × channels × itemsize`, a bag with no
`AudioBlock` keys at all — the way the Python cast already refuses
(`test_audio_block_cast.py`). Never reshaped into a plausible wrong answer, and the
refusal names the port.

**Counting is unchanged and the accumulator is not a second drop site.** A windowed
port's drops are still the mailbox's, counted per inbound link exactly as shipped in
#2023. The accumulator holds partial windows and never evicts. A discontinuity flush
discards a partial window, which is not a bag and is not counted as one — it is logged,
with the port named.

**The resampler is `rubato`** — pure Rust, MIT, adding no `DT_NEEDED` entry, which
`test_wheel_portability.py` enforces regardless of what anyone believes about a crate.
**Stated as an assumption rather than brought as a decision**: choosing a library inside a
ticket is pattern choice, not architecture (owner, 2026-08-13), so unless you say
otherwise the ticket takes `rubato` and proves the portability gate stays green. The
alternative is hand-rolling a polyphase resampler, which buys a maintenance burden and
no capability.

## MODIFIED: §Processor model — the port declares a fourth thing, and the read path reads one

Two DECIDED entries in §Processor model contradict the DECIDED window-contract entry in
§Media I/O. This is not ambiguity to interpret — the plan states both, and one must yield.

> `:222-224` — "A port declares three things and nothing else: name, description, and —
> on an input — delivery profile."
> `:238-240` — "Port rendering in the control plane is name, description, delivery
> profile, and direction."
> `:211-216` — "The engine has no type layer … no read path examines a tag."
> `:288-290` — "The engine's whole role is to count a drop at the port that dropped it and
> surface it; it never inspects a payload."

versus

> `:798-800` — "An audio input port may declare a window contract — rate, channels, dtype,
> window size, hop — beside its delivery profile … the engine resamples, mixes down, and
> frames natively."

A windower must decode `samples`, `sample_rate`, `channels` and `dtype` out of a bag. That
is inspecting a payload, at a read path, driven by a fourth thing the port declared.

[NEEDS DECISION] **Which entry yields, and how is the exception written?**

- **(a) §Processor model yields; the exception is named and bounded.** The port declares
  three things *plus an optional window contract on an audio input*; rendering becomes
  five; and the no-payload-inspection absolute gains one carve-out — declaring a window
  contract **is** the port's opt-in to the engine reading its bags as `AudioBlock`, and
  the engine inspects a payload on exactly the ports that asked it to and nowhere else.
  Links with no contract stay pure plumbing, `connect` still compares nothing, and the
  frame header still carries no schema ident.
  *Recommended.* It is what the DECIDED §Media I/O entry and the ADR both already
  describe, it keeps one implementation at the seam the app process and every helper
  child already share, and the carve-out is opt-in and legible at the declaration site
  rather than ambient.
- **(b) §Media I/O yields; the contract moves off the port onto the processor's config.**
  The absolutes survive untouched and the stage becomes a built-in's own business. Cost:
  it reverses a DECIDED entry and the ADR's centrepiece claim, it gives Rust and Python
  two implementations, and a *user's* Python processor — the case the contract exists for
  — gets nothing, because a user processor has no built-in to configure.
- **(c) Neither yields; the framing runs in a compiler-spliced native node on the link.**
  The engine's read path still inspects nothing, because a processor does the inspecting
  and processors are allowed to. Cost: no compiler op inserts nodes today
  (`core/compiler/compiler_ops/` has five, none of them this), it adds an IPC hop per
  windowed link, it becomes visible in `graph` and `tap` topology, and the ADR already
  rejected a spliced block for the adjacent conditioning case on alignment grounds
  (`audio-subsystem.md:135-137`).

I cannot resolve this one — it retires an absolute either way, and which absolute the
plan keeps is yours.

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

[NEEDS DECISION] **May a native built-in set its input port's window contract at
`setup()`?**

- **(a) Static declaration only.** The contract is a compile-time declaration and nothing
  else. `SpeakerSink` keeps refusing, and a user whose mic and speaker disagree has no
  fix — there is no user-authorable resampler, and the plan says there must not be one.
  The rung ships without solving its most obvious case.
- **(b) A built-in may resolve its contract at `setup()`, where the typestate is Full.**
  The declaration surface gains a runtime setter reachable only from `setup()` — the same
  phase in which a processor requests a window (`:589-591`) — and `SpeakerSink` sets it
  from the format it just opened. `refuse_a_block_the_device_cannot_play` then deletes:
  the stage converts, and the sink plays. *Recommended*, because (a) leaves the plan's own
  motivating case unsolved, and because `setup()`-time resource resolution is a shape the
  processor model already has rather than a new one.
  Cost, stated plainly: a port's rendered contract in `graph` becomes machine-dependent
  for such a port, and this is a new capability on the declaration surface — which is
  precisely why it is here and not assumed.

## REMOVED:

- REMOVED: refuse_a_block_the_device_cannot_play

**This bullet is provisional on `[NEEDS DECISION] 2` resolving to (b)** and must be struck
from this file before `/derive-tickets` if it resolves to (a) — a bullet whose artifact is
meant to survive would fail the ship gate forever. Nothing else in this delta is a
deletion: the rung is additive. The three "no resampler on this rung" comments
(`audio_device_backend.rs:266`, `alsa_audio_device_backend.rs:117`,
`pipewire_audio_shim.c:53-54`) become false the moment the stage lands and are corrected
in the PRs that falsify them — ordinary doc hygiene inside work already being done, not
gate bullets, since the gate's grammar wants artifacts rather than prose.

## Not in scope

- **Conditioning, AEC, AGC, noise suppression and barge-in** — the next rung, and its own
  `/propose-change`. It shares no code with this one: conditioning sits between device and
  published block inside a built-in, the windower sits at a consuming port.
- **Feature extraction** — mel, MFCC and friends stay out, permanently. The contract ends
  at windowed raw samples (`ARCHITECTURE.md:802-803`, `audio-subsystem.md:133`).
- **Output-side window contracts** — an output port declares no contract; a producer
  publishes what it has. Only a consumer states what it needs.
- **Video** — no framing, no rate conversion, no contract on a video port.
- **`packages/` and `examples/`** — lag by design.

## Validation

- **The declaration round-trips**: a contract declared in Python and one declared in Rust
  produce the same `ProcessorPortSchema`, and a hop above `window_size` is refused at
  declaration in both languages naming both numbers.
- **The stage is exact**: a 48 kHz stereo `f32` source into a port declaring
  16 kHz / 1 / `f32` / 512 / 512 yields blocks of exactly 512 per-channel samples, at
  16 kHz, mono, with stamps advancing by exactly 32 ms — asserted on a known signal, not
  on block counts.
- **A rolling window overlaps correctly**: hop 160 against window 512 yields windows whose
  contents overlap by 352 samples, proven by comparing sample values across consecutive
  windows rather than by counting.
- **Readiness is a full window**: a reactive processor on a windowed port is never
  dispatched with nothing to read — the accumulator holding a partial window reports no
  data.
- **A discontinuity flushes rather than interpolates**: a gap in the input stream produces
  no window spanning it, and the window after the gap carries its own first sample's stamp.
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
