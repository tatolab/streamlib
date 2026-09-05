# moq-data-tracks

Generic data tracks on the MoQ extension wheel: any StreamLib bag — a map of objects,
arrays, numbers, strings and binary — published as a MoQ track beside video and audio in
one broadcast, and received back with the producer's stamp intact, under the
`streamlib_bag` container (`docs/plan/ARCHITECTURE.md` §Networking `:1775-1982`). Owner
intent, 2026-09-05: MoQ is how non-StreamLib nodes talk to a StreamLib node, and its value
over a websocket is that data rides the same time-synced transport as the media — so
"audio, video and data" was always the scope, and the "not a serialised link payload"
clause at `:1857-1859` over-reached past the media processors it was deciding. Nothing here
is a runtime-to-runtime fabric; Zenoh stays OPEN (`:1980`).

**Scale gate — this skill, plus an ADR section.** New behavior on the wheel's processor
surface and an extension of the `streamlib_bag` container. One engine change lifts the
tier: the publisher must encode a Python dict to msgpack and the subscriber decode one; the
only bag codec in the tree is the engine's (`sdk/streamlib-python-wheel/src/python_bag_conversion.rs`);
and §Packages `:111-113` says engine work an extension needs is done as engine code inside
the extension's change. Two module-level functions join the stubtest-gated `streamlib`
surface, and `docs/decisions/extension-model.md` gains one section.

**Precondition.** Every §Networking entry this touches is DECIDED — `:1806`, `:1821`,
`:1850`, `:1927`, `:1939` — and the section's one OPEN, Zenoh at `:1980`, is untouched.
§Processor model's delivery-profile entry (`:473-479`) is DECIDED and cited, not changed —
owner ruling 2026-09-05: no lossless mode, the two profiles stand. §Packages `:106-114` is
DECIDED and applied.

**Verified against the tree 2026-09-05 (HEAD 08e920ad1)** — two read-only recon sweeps.

- The wheel carries exactly two object shapes, both fixed structs keyed on media fields
  (`packages/streamlib-moq/src/streamlib_bag_object.rs:30-59`), encoded as msgpack named
  maps with `bitstream` as `bin` (`:32-33`, `:109`, `:122`). `decode_object` picks the
  struct by a two-variant `TrackMedium` (`encoded_media_sample.rs:62-65`), probes `codec`
  and refuses its absence by name (`streamlib_bag_object.rs:154-165`). No test module of
  its own; the round trip is proven from the subscriber side
  (`moq_broadcast_subscriber.rs:1055-1099`).
- Media-ness is hardcoded in the Python too: `_TRACK_MEDIUM_BY_CODEC`
  (`python/streamlib_moq/processors.py:78-82`), `track_medium_of_codec` refusing by name
  (`:105-120`), `process()` casting to `EncodedVideoFrame` / `EncodedAudioPacket`
  (`:196-224`), two static outputs (`:338-344`), `_bag_for` on the received pyclass type
  (`:500-507`), and an unconditional `bag["bitstream"]` in the oversize guard (`:406`).
- The catalog already gives every `streamlib_bag` track the shape a data track needs:
  `codec` is filled with the literal `"streamlib-bag"` and every media field left empty,
  because the catalog is written at connect, before any bag has said what it is
  (`moq_broadcast_publisher.rs:56-64`, `:1002-1011`; `moq_broadcast_catalog.rs:146-152`).
  No catalog field is added by this change.
- Group cadence: only a video sync point cuts, and audio never does
  (`moq_broadcast_publisher.rs:989-994`). A broadcast with no video rides the
  `HIGHEST_OBJECTS_IN_ONE_GROUP = 128` backstop (`moq_session.rs:45-55`), which the plan's
  cut entry (`:1816-1820`) does not yet record. A subgroup retains every object for its
  writer's life, and a joiner mid-group gets that group from its first object.
- Under `streamlib_bag` a track is its inbound link's channel name — `{processor_id}/{port}`
  (`runtime/streamlib-engine/src/iceoryx2/channel_name.rs:172-200`), the id a cuid2 minted
  at `add` — so a subscriber in another node cannot name it; the live fixture runs `cmaf`
  for exactly this reason and says so (`tests/live/moq_broadcast_roundtrip_node.py:32-37`).
  `Runtime.connect(source, destination)` takes no channel name (`_engine.pyi:428-431`).
- Asked to carry an undescribable track, a `cmaf` broadcast holds every track's media up
  to 64 MiB and then refuses every later bag on every track — a stall, not a rejection
  (`moq_broadcast_publisher.rs:75`, `:562-599`).
- The engine's bag codec is total and schema-free: a bag is a dict with string keys at every
  level, values from `dict, list, tuple, str, bytes, int, float, bool, None`, `bytes` as
  `bin` at 1× (`python_bag_conversion.rs:31-38`, `:274-318`, `:355-370`); anything else is
  a `TypeError` at the write. `encode_bag_to_msgpack` and `decode_msgpack_to_python_object`
  are `pub(crate)` (`:31`, `:48`); one bag-conversion function is already a module-level
  export (`decode_tapped_channel_bag_frame_to_python_object`, `lib.rs:83`,
  `_engine.pyi:1543`). The two ext-passthrough keys `__msgpack_ext_type__` /
  `__msgpack_ext_data__` are the only names the codec reserves (`:329-330`).
- An arbitrary bag already crosses a helper link with no dataclass at either end
  (`python_processor_link_data_access.rs:552`; `test_read_into_target.py:251`, `:270`);
  `read(port)` with no `into` is the untyped read (`_engine.pyi:658-659`). The link's
  ceiling is charged against the framed bag, header included (`iceoryx2/output.rs:274`),
  and an over-ceiling write from a helper is dropped with no exception and, since a helper
  installs no `tracing` subscriber, no log line either
  (`python_processor_link_data_access.rs:519-526`) — a third face of §Processor model's
  loss-visibility OPEN (`:503-509`), recorded here and not solved here.
- #2159 (open) adds a drop policy and a priority ladder to the publisher; its ladder has
  two rungs, audio over video, and no data case.

---

## DECIDED (owner, 2026-09-05) — what a late joiner sees: A + C

A MoQ subscriber that joins mid-group receives that group from its first object. For
video that is a GOP and correct. For a data track it means a new subscriber receives up to
the whole open group of messages before the live edge — up to 128 on a broadcast with no
video, or everything since the last video sync point on one with. A websocket delivers
nothing from before the connect; MoQ delivers the group. Product behavior the plan did
not state; the owner chose **A + C** from these, on the recommendation below.

- **A. Accept the replay.** Zero work; matches media; a late joiner gets state catch-up
  for free. On a sparse data-only track (one message a second) the open group can be
  minutes old, and all of it arrives.
- **B. Live edge.** The subscriber discards the partial first group and starts writing at
  the next group boundary. Clock-free — no cross-host stamp comparison, which monotonic
  stamps could not support. Delays first delivery by the rest of the open group, so it
  is only sane with C.
- **C. Time-bound the group on a video-free broadcast.** On the next bag, if the open
  group is older than a bound (≈1 s on the publisher's own monotonic clock — no timer),
  cut first. Bounds A's replay and B's wait alike. Adds a second backstop beside the
  128-object one; the video-cut rule is untouched.

**Chosen: A + C.** Replay is MoQ's behavior and media's, and C bounds it to about a
second where video would not; a downstream that wants the live edge filters on the stamp
it already receives. B is a subscriber flag a later rung can add without touching the
wire. No `[NEEDS DECISION]` remains.

## Assumptions stated, not asked

- **One publisher, one subscriber — no new processor pair.** The whole point is one
  broadcast with video, audio and data under one group cadence; a second pair would be a
  second session and lose cross-track alignment.
- **A link is data when its first bag carries no `bitstream`.** `bitstream` is the wire
  contract's defining key for encoded media (both bag types require it); a bag with one
  takes the existing media path and its existing refusals by name. A user data bag that
  happens to name a `bitstream` key is refused as encoded media with a message naming the
  key, and the user renames it.
- **The user's bag is nested, not flattened.** The object is `{"sequence_index",
  "timestamp_ns", "bag"}` with the user's map under `bag` byte-preserved. Flattening would
  reserve four names in every user's namespace; nesting reserves none. The envelope is
  built in Python and written by the wheel's Rust as bytes — the Rust handles bytes and no
  engine object, as §Packages `:165` has it.
- **The subscriber writes the user's bag verbatim.** `timestamp_ns` rides the frame header
  as the write stamp; `sequence_index` never enters the bag. The subscriber uses it to
  count gaps and says so through the Python log at its progress cadence.
- **One data track per subscriber**, `data_track: str | None`, symmetrical with
  `video_track` / `audio_track`; two data tracks are two subscribers. Keeps the bag
  verbatim — a demux key would be pollution.
- **Data rides `streamlib_bag` only.** A data bag on a `cmaf` broadcast is refused by name
  at its first bag, before any hold — CMAF has no packaging for it. A mixed broadcast
  (CMAF media for `moq-js`, a bag data track beside it) is a later rung: it needs
  per-track `packaging` in the catalog and its own `moq-sub` interop check.
- **Stable track names under `streamlib_bag`.** Publisher config `track_names:
  Sequence[str] | None`, positional in wiring order — the same order `cmaf` already relies
  on for `{id}.m4s`. Count mismatch refused by name at `setup()`; absent, today's channel
  name stands; refused by name under `cmaf`, whose names interop fixes. This retires the
  live fixture's stated reason for running `cmaf`.
- **Data never cuts a group.** `MEDIA_TRACK_PRIORITY` is gone: #2159 split it into
  `AUDIO_MEDIA_TRACK_PRIORITY` (126) and `VIDEO_MEDIA_TRACK_PRIORITY` (127), keyed on
  `TrackMedium`, which has no data variant — so a data track's rung is #2172's to place
  when it adds the medium. #2159 gained the data case in its scope note, filed by
  `/derive-tickets` as a comment, not a rewrite.

---

## ADDED: §Networking — data tracks

- **DECIDED** — A MoQ broadcast carries data tracks beside video and audio, under
  `streamlib_bag` only. `MoqBroadcastPublisher` classifies each inbound link by its first
  bag: a bag with a `bitstream` key is encoded media and takes the typed path unchanged; a
  bag without one is data, and the link is a data track for the publisher's life — a later
  media bag on it is refused by name, as a codec change on a media link already is. The
  publisher mints a per-track monotonic `sequence_index`, builds the object in Python as
  `{"sequence_index": int, "timestamp_ns": int, "bag": <the bag>}`, encodes it with
  `streamlib.encode_bag_to_msgpack_bytes`, and hands the bytes to
  `_native.MoqBroadcastPublishingSession.publish_data_object(inbound_link_name,
  object_bytes)`. The wheel's Rust writes the bytes as the object payload, never cuts a
  group on a data track, and refuses a data object on a `cmaf` broadcast by name before any
  hold. `MoqBroadcastSubscriber` gains config `data_track: str | None` and a third static
  output `data_bags`; `next_media` returns a `ReceivedDataObject` (`track_name`,
  `payload: bytes`) for it, the Python decodes the envelope with
  `streamlib.decode_msgpack_bytes_to_python_object`, refuses one missing any of its three
  keys by name, and writes `bag` verbatim with `timestamp_ns` as the stamp. `sequence_index`
  never enters the written bag; a jump in it is counted and reported through the Python
  log at the progress cadence. A subscriber naming none of its three tracks is refused, as
  one naming neither media track is today. The catalog is unchanged: a data track's entry
  is the entry every `streamlib_bag` track already gets. A subscriber that joins
  mid-group receives the open group from its first object — MoQ's behavior, accepted
  rather than masked (owner, 2026-09-05) — and on a video-free broadcast that group is
  bounded to about a second by the publisher's time backstop, so what a late joiner
  replays is a second of history, never minutes. [moq-data-tracks]
- **DECIDED** — Track names under `streamlib_bag` are the app's to choose.
  `MoqBroadcastPublisher` takes `track_names: Sequence[str] | None`, positional in wiring
  order — the order `runtime.connect` ran, which is the order `cmaf` already numbers
  `{id}.m4s` by. A count unequal to the inbound links is refused by name at `setup()`;
  absent, the track is its link's channel name as today; under `cmaf` the config is
  refused by name, because a subscriber not asked to fetch a catalog hardcodes those names.
  A second node can now name what it subscribes to, and the live fixture's reason for
  running `cmaf` is gone. [moq-data-tracks]
- **DECIDED** — The size guard is charged against what the link charges. The wheel's
  oversize warning compares the *framed encoded bag* — header included — against the
  helper-link ceiling, not `len(bitstream)`; the media path's guard is corrected to the
  same measure in passing, since it under-reported by the bag's other keys and the header.
  The ceiling itself stays the engine's and unexported; the wheel's copy stays a warning
  at the wrong size on drift, never a failing test, as its comment already says.
  [moq-data-tracks]

## ADDED: §Networking — the proof

- **DECIDED** — CI-run, GPU-free, endpoint-free, owned by the wheel: the data envelope
  round trip on the `wired_link` fixture — a nested bag with a `bytes` value crosses the
  publisher's encode, the subscriber's decode and a real link and arrives `==` with
  `bytes` still `bytes`; a `cmaf` broadcast refusing a data bag by name at its first bag
  with no hold entered; `track_names` count mismatch and `cmaf` refusals by name; a
  publisher classifying a bitstream-less bag as data and a later media bag on that link as
  a refusal; a video-free publisher fed two data bags stamped more than the bound apart
  cutting a group between them, and one fed bags inside the bound not cutting; and, in
  the engine wheel, `encode_bag_to_msgpack_bytes` /
  `decode_msgpack_bytes_to_python_object` proven against the existing codec tests' cases
  (named map, `bin`, refusal of a non-dict and of a non-string key) with stubtest and
  pyright over their entries. Live, rig-only, under `/verify-live`'s networking arm: the
  MoQ round-trip fixture gains a `streamlib_bag` run — video, audio and a data track
  carrying a per-frame telemetry bag, `track_names` set — through the Cloudflare draft-16
  relay, the data bag received `==` and stamped as sent, the media decode-back locking
  PSNR as before. The `cmaf` run and its `moq-sub` read are unchanged. [moq-data-tracks]

## ADDED: §Packages & extension model — the bag codec is a public function pair

- **DECIDED** — The `streamlib` wheel exports its bag codec as two module-level functions
  with stub entries: `encode_bag_to_msgpack_bytes(bag: Mapping[str, Any]) -> bytes` and
  `decode_msgpack_bytes_to_python_object(msgpack_bytes: bytes) -> Any`. They are the
  existing `encode_bag_to_msgpack` and `decode_msgpack_to_python_object` made reachable,
  with exactly the codec's rules — a dict with string keys, the eight value types, `bytes`
  as `bin`, refusal by name of anything else — and no new behavior. This is the first
  firing of §Packages' clause that engine work an extension needs is done as engine code
  inside the extension's change: an extension that carries a bag across its own transport
  needs the one codec, and a second one in the wheel would be the parallel abstraction
  the doctrine forbids. It is not a raw byte port — no link reads or writes bytes; the
  functions convert between a bag and bytes in the caller's hands.
  `docs/decisions/extension-model.md` records why. [moq-data-tracks]

## MODIFIED: §Networking — four entries

- **MODIFIED** — `:1850-1859`, the typed processors. The first sentence stands: the media
  path is typed and there is no raw byte port. The last sentence — "what crosses a network
  is a bitstream and the keys a decoder needs, not a serialised link payload" — is narrowed
  to media, which is what it was deciding: a data track's object *is* the bag, whole and
  nested under `bag`, because for data the bag is the payload. What does not carry over
  from the old processors is unchanged: they forwarded media as opaque envelopes and
  restamped on receive; a data track carries the user's own keys byte-exact and the
  producer's stamp untouched, which is the opposite of opaque. [moq-data-tracks]
- **MODIFIED** — `:1806-1820`, the ordering pair and the cut. Adds: a data track has no
  producer pair — the engine mints none for an arbitrary bag — so the publisher mints one
  `sequence_index` per data track, monotonic for its life, carried in the envelope and never
  written into the bag. Records the existing `HIGHEST_OBJECTS_IN_ONE_GROUP = 128` backstop
  for a broadcast with no video, which the entry does not yet name, and that a data bag
  never cuts a group — the audio rule, for the audio reason. Adds a second backstop
  beside it, for the same video-free broadcast: on the next bag, if the open group is
  older than about a second on the publisher's own monotonic clock, the publisher cuts
  first — no timer, a stamp comparison at the write. The video-cut rule is untouched, and
  a broadcast with video never reaches either backstop. Owner ruling 2026-09-05: a late
  joiner receives the open group, and this is what bounds it. [moq-data-tracks]
- **MODIFIED** — `:1821-1835`, many tracks. "Both subscribers expose one output per media
  kind" becomes one output per *track kind*: `MoqBroadcastSubscriber` exposes
  `encoded_video`, `encoded_audio` and `data_bags`, and takes `video_track`, `audio_track`
  and `data_track`; `WhepPlayer` is unchanged. Adds `track_names` to "under `streamlib_bag`
  each is its link's channel name": *unless the publisher was given names, positional in
  wiring order*. [moq-data-tracks]
- **MODIFIED** — `:1939-1952`, two container formats. Adds: `streamlib_bag` is also the
  only container a data track rides; `cmaf` refuses one by name at its first bag. A mixed
  broadcast is named as a later rung, not built. [moq-data-tracks]
- **MODIFIED** — `:1927-1938`, `MoqBroadcastPublisher`. Config gains `track_names`;
  `MoqBroadcastSubscriber` config gains `data_track` and its outputs gain `data_bags`.
  [moq-data-tracks]

## REMOVED: nothing

This change adds a track kind and a config door and deletes no artifact. No bullet, so the
ship gate has nothing to prove here; the fixture comment that gives "one node" as the
reason for `cmaf` is corrected in-stream when `track_names` lands, as doc hygiene inside
the work.
