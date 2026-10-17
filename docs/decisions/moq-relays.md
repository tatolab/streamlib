# MoQ relays: a stream's exposed ports are tracks at relays, forwarded without logic

Rationale for the `[moq-relays]` entries in `docs/plan/ARCHITECTURE.md` and the 2026-10-16
vocabulary in `docs/plan/GLOSSARY.md`, from the owner's align of the sharing step on
2026-10-16 and the owner's realignment of 2026-10-17, which struck the clauses below that a
session had written into the plan without a decision. It amends `moq-on-the-tailnet.md`,
whose struck clauses point here.

Annotated in place: `moq-on-the-tailnet.md`, `exposure-levels.md`, `one-runtime-per-machine.md`
(its sharing step), `processor-owned-windows.md` and `runtime-hosting.md`.

## Trigger

Read this before any of the following:
- putting any knowledge of a payload — a codec, a cast, msgpack — into the publisher, a relay
  adapter or the engine's forwarding path;
- adding a link kind, a `remote_output`, or any engine-side reader of a relay;
- reading or configuring Tailscale from the runtime, or serving HTTP off loopback;
- opening a window from the runtime, or adding a template to `tatolab new`;
- encoding a payload behind `write` or decoding one behind `read`.

## The direction, in the owner's words

The goal is a system where we build streams and can push anything out over MoQ in a generic
fashion, splitting writing a stream from publishing it, while keeping the zero-copy
performance. Anything public is just sent out, without logic. A relay adapter is generic
over the whole runtime; it is just shipping exposed outputs. MoQ is already general purpose
and lets you send anything, so Tatolab is general purpose too — nothing baked in for remote
streaming, data flowing to any consumer, another runtime or ffmpeg or gstreamer alike.
Tailscale reach is incidental: a local relay is on the machine, so the tailnet reaches it.

## Decided

- **The publisher holds no logic.** One per runtime, in `tatolabd` beside the adapters; per
  exposed port one reader on the channel's reserved slot; payload bytes forwarded as written,
  anything exposed; groups from the envelope, and under backpressure the rest of a group
  dropped, never a message inside one; the catalog merged from the ports' entries as text. It
  decodes nothing and imports nothing, so one code path serves every port of every stream.
- **A payload is bytes.** `write(port, bytes)` and `read(port) -> bytes`; `read(port,
  into=T)` is `T.from_bytes`. msgpack is a helper the stream package ships, applied in the
  node's own code; a built-in writes bytes in the encoding its catalog entry names. The
  audio-window stage, on an input's declared contract, is the one place the engine reads a
  payload, never in the forwarding path.
  > ~~The audio-window stage and the loopback snapshot door — reading a `VideoFrame` record
  > on a port whose fragment names that cast — are the two places the engine reads a
  > payload.~~ — Superseded 2026-10-17 by the owner's realignment: the snapshot door was a
  > session's addition in reply to a bot review, never decided; the align's "one place" stands.
- **The encoders write the stack's media frame.** `H264Encoder`, `H265Encoder` and
  `OpusEncoder` emit the frame layout the moq-dev players read — a varint timestamp then the
  bare access unit or packet — and state codec and extent in their catalog entry, the
  players' own. `EncodedVideoFrame` and `EncodedAudioPacket` are deleted; the decoders read
  the same bytes; the WebRTC wheel follows as a consumer on its own backlog.
  > ~~and state codec, extent and audio parameters in their catalog fragment … the decoders,
  > `Mp4Sink` and the WebRTC nodes read and write the same bytes.~~ — Superseded 2026-10-17:
  > `Mp4Sink` is deleted, and the wheel is a consumer, never in the change's scope.
- **The group boundary is the writer's**, one default-on flag on `write`, in the envelope,
  and Python and Rust readers see it alike.
- **The catalog is assembled from port descriptions.** Each output port may carry its entry
  in the players' own catalog layout — a hang section and that section's fields — written by
  the node that owns the port; a reader follows it and `connect` compares nothing.
  > ~~Each output port may carry a fragment — `encoding`, `cast` import path, `kind` — a
  > reader follows.~~ — Superseded 2026-10-17: the triple was a session's invention inside
  > the align's own diff; it half-copied hang's catalog, and a `binary` entry with no `mode`
  > would have made every player reject the whole broadcast.
- **`Mp4Sink` is deleted.** A stream's last mile — writing a track to a file — is the user's,
  through any MoQ client of a relay, never the runtime's; the runtime is stream technology,
  not an end-user application.
- **No side paths.** `tap`, `exchange` and the snapshot form are deleted and nothing replaces
  them. Every end-to-end proof is a deterministic tool outside the runtime reading a port
  the way a user does — a MoQ client of a relay, or the preview page; the engine's internals
  are proven by unit tests.
- **Relay adapters are crates.** An adapter SDK crate holds the trait; one crate per adapter
  implements it; every adapter is compiled into `tatolabd`; the app or the CLI adds any
  number of instances of each through `tatolabd`, recorded in `relays.json`, each with its
  own settings and whatever credential its provider needs, in the OS secret store.
- **The relay source node keeps the track's time.** It stamps each message from the track's
  own timestamp re-based onto the local clock, never arrival time.
- **`tatolabd` provisions the local relay's certificates** as files the relay reloads, rotated
  before expiry: pinned by fingerprint on loopback, publicly valid where the machine has a
  hostname.
- **A stream is one broadcast; each exposed port one track.** The relay's listing is the
  directory. A machine is its hostname, cast; its root at a relay is the credential's.
- **Exposure maps onto relays.** Internal at none, private at the local relay's loopback
  listener only, public at every joined relay and the local relay's every-interface listener.
- **The local relay is `moq-relay`, two children of `tatolabd`** — one per listener, since
  the program binds one address — shipped in the runtime unit on both floors. Only the
  runtime publishes to them; anyone subscribes. The runtime is a MoQ client of every relay
  and serves no endpoint.
- **Relay adapters carry what differs:** dialect, credential, root, their own settings.
  Local, Cloudflare, moq.pro; Red5 on hold. Cloudflare and moq.pro both ship.
- **Reading a relay is a source node.** `remote_output` and the cross-stream iceoryx2 link are
  deleted; the relay source node writes payloads out as received and restates stamps locally.
- **The runtime reads, configures and requires nothing of Tailscale.** No status read, no
  `tailscale serve`, no tailnet HTTP, no off-machine read-only MCP.
- **The runtime's HTTP is loopback only:** the listing and `/watch/<address>` (the preview
  page, a small web project built at release on the stack's player).
  > ~~and `/snapshot/<address>`.~~ — Superseded 2026-10-17: no snapshot form; a frame is seen
  > through the preview page or any MoQ client.
- **The runtime opens no windows.** Every window path, test and fixture is deleted in the
  sharing step's change.
- **The scaffold is the sample stream** — a branded `TestPatternSource` animation → effect →
  `H264Encoder` → public — and `tatolab new` offers no camera and no flag.
- **`H264Encoder` gets a CPU arm** over openh264, loading Cisco's binary, fetched once —
  deferred past the sharing step on 2026-10-17: it serves headless no-GPU machines, not
  sharing, and x264 is ruled out as GPL.

## Why

- **Any logic in the forwarding path makes Tatolab a media product.** The first design put
  a media opt-in on ports that the publisher read payloads under. The owner's reading: once
  the adapter listens for a cast, every relay adapter grows media code, and the system stops
  being general purpose. The fix was to move every needed fact into places the writer already
  owns — the envelope, the port description — and leave payloads opaque end to end.
- **The hidden encoding was the obstacle to standard playback.** `write` encoding msgpack for
  every payload meant an encoder's public port could never be played by a stock MoQ player
  without a conversion somewhere; every place to put the conversion baked in media. With bytes
  on the cable and encoders writing the standard frame, a public port simply *is* a playable
  track. msgpack stays, as a library, because a Rust built-in and a Python node still need a
  neutral default to meet on and an agent still needs to open a payload with nothing installed.
- **The group flag cannot vanish.** Relays start a late joiner at a group and drop whole groups;
  for video that must be a keyframe, and only the writer knows. One default-on flag that two
  built-ins set is the honest minimum.
- **One API for reading anything.** `remote_output` on the same machine, a different spelling
  for another machine, and a stamp-marking question inside links was three concepts; "add the
  source node, give it an address" is one, and it is the same code a client SDK uses later. The
  cost — a raw frame between two streams on one machine now pays an encode, two loopback hops
  and a decode, about ten milliseconds and a lossy codec — had no consumer, and a zero-copy
  fast path can return behind the same node with nothing new on the author's surface.
- **Per-port broadcasts lost the point of a broadcast.** Several camera tracks and an audio
  track in one broadcast, switched in a player, is the standard shape and what a stream is.
- **Tailscale as a requirement was the wrong half of the idea.** The owner's intent was reach
  "by virtue of having a local relay"; `tailscale serve` was a rights prompt and a manual step
  on every install, and reading status for a name gave hosted relays nothing — they assign the
  root. Reach, names, encryption and NAT are a relay's or the user's network's.
- **Windows were the viewer's job.** A stream's output is a track; a browser shows it; the
  runtime gets simpler on every floor, loses the `DISPLAY` dependency, and the software
  device's "no window" exception disappears.
- **A camera as the first thing a user meets fails for too many of them.** A branded animation
  proves the path on every install type with no device, no prompt and no vivid; a camera
  project is one line away in the docs.
- **openh264 through Cisco's binary** is the one H.264 CPU path whose patent cost is carried
  by someone else — for personal and non-remunerated use, which Cisco's cover is limited to;
  building it from source moves that cost here, and a royalty-free codec in
  its own block would make the scaffold choose a codec per device.
- **The catalog is the players' own layout, not a translation of ours** (2026-10-17). The
  fragment a session wrote — `kind`, `encoding`, `cast` — collapsed hang's `codec`, `mime` and
  `schema` into one free-text field, had no `mode`, and would have had the publisher invent
  the translation; a port describing itself in hang's own entry keeps the publisher
  logic-free and confines a bad description to its own track.
- **The runtime is stream technology, not an application** (2026-10-17). `Mp4Sink`, like the
  windows, was a last-mile concern from the time the runtime was an end-to-end app; with every
  track at a relay, a file is any MoQ client's to write.
- **Verification uses the product's doors** (2026-10-17). `tap`, `exchange` and the snapshot
  form were side paths that let a test, or an agent, reach a node a user could not, and each
  was code the engine carried for no user. A proof that reads a relay URL dogfoods the
  product; what cannot be proven that way is proven by a unit test.
- **Adapters as crates** (2026-10-17): adding a relay is adding a crate against the SDK, not
  editing the daemon, and the same crates serve headless and the app alike.
- **Whole groups drop** (2026-10-17): a message dropped mid-group leaves a reader decoding
  against a picture it never got until the next keyframe; dropping to the next `starts_group`
  is a brief freeze and a clean picture, and it is what moq-lite's own `max_age` does.
- **A reported loss drops the same way, and a group's first message after it is kept**
  (2026-10-17): the decoders and the publisher treat a nonzero `lost_since_last_read` as they
  treat backpressure, discarding to the next `starts_group`. A loss is reported on the first
  message after the lost one, so when that message itself starts a group the gap is behind it
  and the reader resumes there; discarding it would drop a whole group for nothing.
- **Track time, not arrival time** (2026-10-17): stamping at arrival bakes network jitter into
  every reader's timing; re-basing the track's own timestamps is the stack's player's model.

## Rejected alternatives (by the owner)

- **A media opt-in on an output port the publisher reads payloads under** — media logic in the
  adapter path.
- **The viewer page decoding our msgpack payloads itself** — a player written here, and no
  stock tool plays the track.
- **The engine subscribing inside the link machinery** (`remote_output("desk/…")`) — a MoQ
  reader in the engine and a second link kind.
- **One broadcast per exposed port** — loses multi-track switching and audio beside video.
- **Embedding `moq-relay` as a library in `tatolabd`** — the runtime becomes a relay.
- **A `?relay=` parameter on the preview page** — hosted relays have their own players.
- **A camera in the scaffold, default or flag.**
- **Building openh264 from source; a royalty-free codec in its own block.**
- **Keeping tailnet HTTP through `tailscale serve`, and the Tailscale status read.**
- **Dropping Rust built-ins for Python nodes over runtime capabilities, and dropping msgpack
  entirely** — raised, deferred to their own align; windows going makes the first more
  plausible, and the second is moot once msgpack is a helper.
- **A catalog entry in the runtime's own words** (`kind`/`encoding`/`cast`), 2026-10-17 —
  a translation the publisher would own, and not hang's.
- **The publisher refusing, warning on or filtering a port by what it carries**, 2026-10-17
  — logic in the forwarding path; a `VideoFrame` port published shows surface ids, and that
  is what was published.
- **A snapshot form; `exchange` kept as a verification door; an image-writing built-in for
  the rigs; an MCP image tool**, 2026-10-17 — side paths past exposure and the relays.
- **x264**, 2026-10-17 — GPL, which a BUSL binary cannot link.
- **The player's published package copied in with no build**, 2026-10-17 — a library of
  many packages a browser cannot load unbundled.
- **One trait inside `tatolabd` with no crate per adapter**, 2026-10-17.

## Consequences

- **A shipped wire contract changes.** The encoded-video and encoded-audio payloads, the two
  casts over them, and `write`/`read`'s signatures change in the sharing step's change, with
  every reader and writer in the tree.
- **Two streams on one machine no longer share a raw GPU frame.** Stated above; the fast path
  is open for later.
- **Safari and iOS play through the preview page over the relay's WebSocket path**, which
  the relay's listener serves on the same port as its tracks.
  > ~~Safari and iOS do not play through the preview page while the stack's player keeps
  > WebKit on its WebSocket path.~~ — Superseded 2026-10-17: the relay serves that path; the
  > "no WebSocket" clause was a session's.
- **The private boundary is a loopback binding, not a token.** The local relay has two
  listeners — loopback with every exposed port, every interface with public only — because a
  bearer token minted on the machine can be carried off it; the kernel's binding cannot. The
  review of the align PR caught the token design on 2026-10-16; the owner ratified the
  listener boundary on 2026-10-17.
- **The catalog has one entry per track**, named by `<node>/<port>` under its hang section.
  In hang's layout several video entries in one broadcast are renditions — alternatives a
  player picks one of — so the stock player shows one camera at a time and the preview page
  names the track in its URL; any client subscribes to any track by name.
  > ~~grouped under its `kind`, so several cameras in one stream are several `video` entries
  > rather than one replacing another.~~ — Superseded 2026-10-17: a session's reply to a bot
  > review, and a misreading of hang's renditions.
- **The PSNR rigs re-score through a relay**, a second lossy hop, so their baselines are
  re-taken on that path; the audio check subscribes to the raw `AudioBlock` track at loopback.
- **Headless no-GPU machines share no H.264** until the CPU arm returns.
- **Design changes never come from replies to a bot reviewer**, and "(owner, date)" is
  written only with an owner source — the rule the 2026-10-17 realignment commissioned.
- **The sharing step is large:** publisher, adapters, local relay, catalog, group flag, bytes
  payloads, the source node, the sample stream, the listing and the preview page, and the
  deletions — windows, `Mp4Sink`, `remote_output`, the Tailscale read, `tap`, `exchange`; the
  CPU encoder arm is deferred past it.
