# MoQ relays: a stream's exposed ports are tracks at relays, forwarded without logic

Rationale for the `[moq-relays]` entries in `docs/plan/ARCHITECTURE.md` and the 2026-10-16
vocabulary in `docs/plan/GLOSSARY.md`, from the owner's align of the sharing step on
2026-10-16. It amends `moq-on-the-tailnet.md`, whose struck clauses point here.

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

- **The publisher holds no logic.** One per runtime; per exposed port one reader on the
  channel's reserved slot; payload bytes forwarded as written; groups from the envelope;
  the catalog merged from the ports' producer-written fragments as text. It decodes nothing
  and imports nothing, so one code path serves every port of every stream.
- **A payload is bytes.** `write(port, bytes)` and `read(port) -> bytes`; `read(port,
  into=T)` is `T.from_bytes`. msgpack is a helper the stream package ships, applied in the
  node's own code; a built-in writes bytes in the encoding its catalog fragment names. The
  audio-window stage and the loopback snapshot door — reading a `VideoFrame` record on a port whose
  fragment names that cast — are the two places the engine reads a payload, each a declared
  encoding, neither in the forwarding path.
- **The encoders write the stack's media frame.** `H264Encoder`, `H265Encoder` and
  `OpusEncoder` emit the frame layout the moq-dev players read — a varint timestamp then the
  bare access unit or packet — and state codec, extent and audio parameters in their catalog
  fragment. `EncodedVideoFrame` and `EncodedAudioPacket` are deleted; the decoders, `Mp4Sink`
  and the WebRTC nodes read and write the same bytes.
- **The group boundary is the writer's**, one default-on flag on `write`, in the envelope.
- **The catalog is assembled from port descriptions.** Each output port may carry a fragment —
  `encoding`, `cast` import path, `kind` — a reader follows; `connect` compares nothing.
- **A stream is one broadcast; each exposed port one track.** The relay's listing is the
  directory. A machine is its hostname, cast; its root at a relay is the credential's.
- **Exposure maps onto relays.** Internal at none, private at the local relay's loopback
  listener only, public at every joined relay and the local relay's every-interface listener.
- **The local relay is `moq-relay`, a child of `tatolabd`,** shipped in the runtime unit on
  both floors. The runtime is a MoQ client of every relay and serves no endpoint.
- **Relay adapters carry only what differs:** dialect, credential, root. Local, Cloudflare,
  moq.pro; Red5 on hold.
- **Reading a relay is a source node.** `remote_output` and the cross-stream iceoryx2 link are
  deleted; the relay source node writes payloads out as received and restates stamps locally.
- **The runtime reads, configures and requires nothing of Tailscale.** No status read, no
  `tailscale serve`, no tailnet HTTP, no off-machine read-only MCP.
- **The runtime's HTTP is loopback only:** the listing, `/watch/<address>` (the preview page)
  and `/snapshot/<address>`.
- **The runtime opens no windows.** Every window path, test and fixture is deleted in the
  sharing step's change.
- **The scaffold is the sample stream** — a branded `TestPatternSource` animation → effect →
  `H264Encoder` → public — and `tatolab new` offers no camera and no flag.
- **`H264Encoder` gets a CPU arm** over openh264, loading Cisco's binary, fetched once.

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
  by someone else; building it from source moves that cost here, and a royalty-free codec in
  its own block would make the scaffold choose a codec per device.

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

## Consequences

- **A shipped wire contract changes.** The encoded-video and encoded-audio payloads, the two
  casts over them, and `write`/`read`'s signatures change in the sharing step's change, with
  every reader and writer in the tree.
- **Two streams on one machine no longer share a raw GPU frame.** Stated above; the fast path
  is open for later.
- **Safari and iOS do not play through the preview page** while the stack's player keeps
  WebKit on its WebSocket path; unchanged from 2026-10-04.
- **The private boundary is a loopback binding, not a token.** The local relay has two
  listeners — loopback with every exposed port, every interface with public only — because a
  bearer token minted on the machine can be carried off it; the kernel's binding cannot. The
  review of the align PR caught the token design on 2026-10-16.
- **The catalog has one entry per track**, named by `<node>/<port>` and grouped under its
  `kind`, so several cameras in one stream are several `video` entries rather than one
  replacing another.
- **The sharing step is large:** publisher, adapters, local relay, catalog, group flag, bytes
  payloads, the source node, the sample stream, the CPU encoder arm, the loopback forms, and
  the deletions — windows, `remote_output`, the Tailscale read, `tap`, `exchange`.
