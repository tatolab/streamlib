# delivery-profile-vocabulary

The vocabulary align made true in the tree. Two read-policy words replace three, the
producer-blocking machinery is deleted whole, and the silent mailbox eviction becomes a
per-link counted event visible in `graph` — landing the alternative to silent loss in the
same motion that removes blocking, exactly as the plan sequences it. Delta against
`../ARCHITECTURE.md` §Processor model & scheduling, with consequential edits in the audio
built-ins that declared the retired words. The windower rung follows this change; it is
not in it.

**Scale gate — a changed contract, so this skill plus an ADR.** The delivery-profile
strings are the Python API's public declaration surface and their resolution feeds the
iceoryx2 service settings, so the ADR trigger fires — and is already satisfied:
`docs/decisions/delivery-profile-vocabulary.md` was written by the align that decided
these entries (PR #2021), over two research passes whose load-bearing claims were
re-verified in-tree. Nothing below decides past it.

**Precondition.** The six vocabulary entries in §Processor model & scheduling
(`ARCHITECTURE.md:228-270`) are DECIDED, merged 2026-08-28. The reflection entry
(`:271-278`) is **OPEN** and stays untouched: nothing here reflects a count to a
producer, adds a producer-side read surface, or opens a notify path for drops. The one
clause of that entry binding this change is already stated in the DECIDED counting entry
— drops are counted per link, never as one blended total.

---

## MODIFIED: §Processor model — the rename, `newest` / `ordered`

The two words land everywhere the three words live; no alias, no deprecation, pre-1.0.
The enum becomes `DeliveryProfile::Newest` / `DeliveryProfile::Ordered`, and
`DELIVERY_PROFILE_DECLARATION_VALUES` becomes `[&str; 2] = ["newest", "ordered"]`
(`sdk/streamlib-processor-schema/src/processor_schema.rs:372`). Every spelling site is
enumerated; the census is complete because the schema constant is the single legal-values
source and everything else quotes it:

- Resolution and parsing: `runtime/streamlib-engine/src/iceoryx2/delivery_profile.rs` —
  `from_manifest_str`, `as_manifest_str`, the resolve triple, the doc comments including
  the `Lossless` variant's measured-gap admission, and the error text listing legal
  values.
- Python surface: `_DELIVERY_PROFILES = ("latest", "every_sample", "lossless")`
  (`sdk/streamlib-python-wheel/python/streamlib/_processor_declaration.py:31`) and its
  docstring (`:51`). Pure Python, checked from source — no `.pyi` entry exists or is owed.
- Macro grammar: `sdk/streamlib-macros/src/grammar.rs` — the doc examples (`:725` among
  them) and the quoted values.
- Control-plane rendering docs: `core/graph/nodes/port_info.rs:19`,
  `core/json_schema.rs:78-79,211-212` and the two tests asserting rendered values
  (`:416-423`, `:444-449`).
- Declarers: 15 × `latest` → `newest` (test_support, attribute-macro tests,
  `display_window.rs`), `speaker_sink.rs:105` `lossless` → `ordered`,
  `python_test_harness_endpoints.rs:193` `every_sample` → `ordered`, and the
  `microphone_source.rs:479` doc reference.
- Wheel fixtures and probes that spell a profile: the audio fixture set under
  `runtime/streamlib-engine/tests/fixtures/` and `sdk/streamlib-python-wheel/tests/`
  (`audio_channel_drain.py:15`, `captured_audio_waveform_recorder.py:45`,
  `speaker_sink_probes.py:34`, `microphone_source_probes.py:33`).

A typo still fails at wire time listing the legal values
(`unknown_declared_value_is_rejected_with_the_legal_values` keeps its invariant with the
new spellings).

## MODIFIED: §Processor model — the deletion, no link ever blocks a producer

`Overflow` is deleted as an enum, not narrowed to one variant: with `Block` gone,
`DropOldest` is not a policy but the only behaviour, and a one-variant enum is dead
machinery. `DeliveryResolution` collapses to `(drain_order, depth)`;
`open_iceoryx2_service_op.rs` hardcodes `enable_safe_overflow(true)` at the builder
(`:402` today derives it) and its service-config JSON keeps the key as a wire fact;
the multi-destination profile-conflict handling (`:479-489`) loses the genuinely
ambiguous case — with one overflow policy engine-wide, conflicts reduce to depth and
drain order. `TapChannelSizing.enable_safe_overflow` (`core/runtime/tap.rs:73`) and its
plumbing retire.

Tests move with the machinery:

- `overflow_disabled_publisher_back_pressures_on_full_buffer`
  (`iceoryx2/node.rs:488`) deletes with the capability — it locks the contract being
  removed, and its own doc admits the semantics were an upstream default.
- `overflow_enabled_publisher_does_not_block_on_full_buffer` (`node.rs:429`) stays: it
  locks the surviving contract.
- The tap's parked-forwarder-on-a-blocking-channel test (`tap.rs:548-575`) reworks: the
  hazard it defends against is gone, but the always-drain/never-park forwarder contract
  it proves is not — it re-anchors on the mpsc-full drop-and-count behaviour without
  constructing a blocking channel.
- `lossless_resolves_to_fifo_block_deep` deletes;
  `latest_resolves_to_skip_drop_shallow` / `every_sample_resolves_to_fifo_drop_deep`
  rename with their profiles and drop their `enable_safe_overflow` assertions.

The audio built-ins shed the workarounds the blocking word forced, in the same PRs that
touch them:

- `MicrophoneSource`'s `PUBLISH_THREAD_EXIT_GRACE` detach-and-leak comment
  (`microphone_source.rs:61-64`) loses its rationale — a publishing thread can no longer
  be held inside `write` by a consumer. The bounded-exit shape may stay as ordinary
  defensive shutdown; the comment naming `lossless` as the cause cannot.
- `SpeakerSink`'s drain-wait comment (`speaker_sink.rs:434-439`) re-states its truth:
  the wait is sink-internal bounded queueing (a drain thread pacing itself against its
  own ring, the device-stall envelope), not backpressure a profile asks for. Behaviour
  is unchanged — a full ring holds only the sink's own drain thread, and what overflows
  upstream of it now lands as counted drops at the port.
- `audio_samples_awaiting_playback_ring.rs:414-417`'s "would make `lossless` a lie"
  test doc re-anchors on the real contract: the ring bounds the sink's own queueing.

## MODIFIED: §Processor model — the counter, no loss is silent

The eviction site gains attribution and a count; nothing else about delivery changes.

- `PortMailbox` (`iceoryx2/mailbox.rs:13-16`) entries carry their link: the queue
  element becomes the payload plus the link index the subscriber binding already has —
  `add_channel_subscriber(local_port, link_id, subscriber)` (`iceoryx2/input.rs:267-271`)
  is the existing seam, so an evicted entry names the link whose bag was lost, exactly
  (fan-in attributes correctly because the tag rides the entry, not the port).
- Eviction in `push` increments a per-link counter owned by the mailboxes; `pop`/read
  paths are untouched. The counter is monotonic and cumulative, the tap's
  `dropped_bags` shape.
- `graph` surfaces it through the component that already renders:
  `ProcessorMetrics` (`core/graph/components/processor_metrics.rs`) is inserted on
  processor nodes and populated with the per-link dropped-bag counts;
  `observability/inspector.rs:46,94` already reads the component when present, so the
  control-plane path is wiring, not new surface. Its unused placeholder fields
  (`throughput_fps`, `latency_p50_ms`, `latency_p99_ms`) are not populated by this
  change and make no claim — populating them is not commissioned here.
- Port rendering stays exactly its DECIDED four fields (`ARCHITECTURE.md` §Processor
  model, port-rendering entry): counts ride metrics, not the port declaration.
- The audio built-ins' own edge counters (device-edge drops in `MicrophoneSource`)
  are a different subject and are untouched — theirs count loss at the device, this
  counts loss on the link.

## MODIFIED: §Media I/O — consequential only

The align already moved the plan's audio entries to `ordered`; this change moves the
declarers (`speaker_sink.rs`, harness, fixtures) listed above. No audio behaviour
changes: the device edge already drops-and-counts, `SpeakerSink` already refuses
wrong-format blocks loudly, and the loopback fixtures assert content, not profile names.

## REMOVED:

- REMOVED: Lossless
- REMOVED: LOSSLESS
- REMOVED: EverySample
- REMOVED: every_sample
- REMOVED: Overflow::Block
- REMOVED: Overflow::DropOldest
- REMOVED: runtime/streamlib-engine/src/iceoryx2/overflow.rs

**Blast radius, checked by running the gate's own sweep rather than assumed.** All seven
bullets match only definition and reference sites this change already opens, under the
sweep's exclusions (`docs/plan/**`, `docs/decisions/**`, `docs/learnings/**`,
`examples/**`, consumer `packages/**`, `CHANGELOG.md`). The plan's own retirement prose
("`lossless` is retired") and the ADR live in excluded paths by design.

**A lowercase `lossless` bullet is deliberately not written.** The pixel-exchange
surface legitimately says "lossless RGBA8 PNG"
(`runtime/streamlib-api-server/src/handlers.rs:286,300`) — a claim about PNG encoding,
not a delivery profile — so the bullet would fail forever. The profile spellings die
under the bullets above plus a validation grep for the quoted form instead.

## Not in scope

- **The reflection mechanism** (`ARCHITECTURE.md:271-278`, OPEN) — no producer-side
  surface, no notify-path work, nothing reads a drop count but the control plane. The
  per-link attribution built here is the entire door that entry holds open.
- **The windower and resampler** (§Media I/O window-contract entry) — the next change,
  proposed separately once this lands; it depends on `ordered` existing, not on being
  in the same train.
- **Conditioning, barge-in, audio plugins** — later rungs, unchanged.
- **Populating throughput/latency metrics** — `ProcessorMetrics` carries placeholder
  fields; this change populates drop counts only.
- **`packages/` and `examples/`** — lag by design. The pre-pivot `mp4` writer declaring
  `lossless` (`packages/mp4/processors/mp4_writer_linux.rs:21`) cannot compile at HEAD
  and is not edited.

## Validation

- **The rename is total**: `git grep -F '"lossless"' -- runtime sdk` and the same for
  `"every_sample"` and `"latest"` restricted to `delivery_profile` contexts return
  zero; `DELIVERY_PROFILE_DECLARATION_VALUES` carries exactly two entries; a declared
  `latest` fails at wire time listing `'newest', 'ordered'`.
- **The deletion is total**: the seven `REMOVED:` bullets are gate-clean; the engine
  builds with no `Overflow` type; every service opens with safe overflow on, asserted
  where sizing is already asserted (`iceoryx2/channel_sizing_tests.rs`).
- **The counter is exact per link**: a wired two-producer fan-in into one `ordered`
  port under a stalled consumer reports each link's own losses, and the sum equals
  published minus delivered — the 78-of-378 arrangement, now visible. A healthy run
  reports zero.
- **`graph` shows it**: the snapshot of a dropping run carries the per-link counts
  under the processor's metrics; the port's rendered declaration is unchanged
  (`test_a_declared_port_carries_no_type_key_under_any_spelling` still passes).
- **The tap is unaffected**: tap's `dropped_bags` counts what it always counted;
  the reworked forwarder test proves never-park without a blocking channel.
- **Audio still proves itself**: `/verify-audio` passes on the rig after the rename
  (wheel rebuilt first), and the null-backend graph runs end to end declaring
  `ordered`.
- Mechanical: `cargo xtask check-all-source-gates` green; wheel pytest suite green;
  `cargo check --target aarch64-apple-darwin` for the Apple cross-check.
