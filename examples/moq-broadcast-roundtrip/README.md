# moq-broadcast-roundtrip

Camera and microphone out to a Media-over-QUIC relay, and the same broadcast
pulled back down and played — publish and subscribe in one graph, with the
network in the middle.

The showcase of a **processor extension** on both sides at once:
`MoqBroadcastPublisher` and `MoqBroadcastSubscriber` ship in the
`streamlib-moq` wheel, not in the engine, and this app reaches them the way it
reaches any third-party processor.

## The model this example teaches

**What reaches the window came off the relay.** There is no local link between
the two halves. The publisher's helper sends over QUIC; the subscriber's helper
receives over QUIC; the only thing joining them is a broadcast name both sides
were told. Kill the relay and the window stops — which is the honest picture of
what a network hop is.

**A track is an inbound link.** Both encoders connect to the publisher's one
`tracks` input, and each link becomes one MoQ track. It is `Mp4Sink`'s shape,
reused.

**The container names the tracks, not the author.** On `cmaf` — the default,
because interop is the point — they are `.catalog`, an init track `0.mp4`, and
`{track_id}.m4s` media tracks numbered from one in declaration order. That order
is `rt.connect` order, which is why this file wires video into `tracks` first
and then names `1.m4s` and `2.m4s` in the subscriber's config. It is the
reference publisher's fallback contract, hardcoded by any subscriber not asked
to fetch a catalog, so it is not the wheel's to vary.

The other container, `"streamlib_bag"`, names each track after its link's own
channel — which is a cuid2 minted at startup, so it is not a name a second
process can be told in advance. That is a real constraint of this one-app shape,
not a defect: `streamlib_bag`'s subscriber is normally a *different* node that
read the name out of `graph` first.

**Two output ports, not one per track.** Ports are declared statically by
decorator, so a subscriber exposes `encoded_video` and `encoded_audio` and takes
the track names in config — which is what lets the decoders downstream be wired
by name before anything connects.

**What survives the hop depends on the container.** Under `"streamlib_bag"`
every bag key crosses byte-exact, the producer's `group_index` /
`sequence_index` pair and its stamp included. Under `"cmaf"` the container
carries neither, so the subscriber mints the pair by the engine's own
producer-side rule and takes the stamp from the fragment's decode time — a
gapless `sequence_index` downstream of a CMAF hop is therefore **not** evidence
of a lossless stream. Use `streamlib_bag` when the pair is what you are
measuring; use `cmaf` when another player has to read it.

## Run it

```bash
uv venv --python 3.12 && uv sync
export STREAMLIB_MOQ_RELAY_URL='https://draft-16.cloudflare.mediaoverquic.com/<token>'
streamlib run
```

`uv sync` installs `streamlib` from the simple index pinned in
`pyproject.toml`, and builds `streamlib-moq` from this checkout — that is a
maturin project, so it needs a Rust toolchain and `cmake` (its QUIC stack builds
`aws-lc-sys`). Outside a checkout, drop the `tool.uv.sources` path entry and
both wheels resolve from the index.

To work against a checkout of the engine as well, install that checkout's wheel
into this venv:

```bash
uv pip install maturin
maturin develop --manifest-path ../../sdk/streamlib-python-wheel/Cargo.toml
```

## The relay URL is a credential

A draft-16 relay is provisioned per account and authenticates — the token rides
the extended CONNECT's `:path`, so the URL *is* the credential and there is no
credential-free public relay to fall back to. It is read from
`STREAMLIB_MOQ_RELAY_URL` and never written down here; the app refuses to start
without it.

The wheel will not echo the URL in any error message for the same reason. If a
connection fails, the log names the broadcast and the failure, never the
address.

## Playing it somewhere else

Because the default container is CMAF, any MoQ player can read this broadcast.
`moq-sub`, built from [`cloudflare/moq-rs`](https://github.com/cloudflare/moq-rs):

```bash
moq-sub "https://draft-16.cloudflare.mediaoverquic.com/<subscribe-token>"
```

That is a stronger check than the local window: it is a third-party client
parsing the catalog, accepting the init segment and decoding the media.

## What the hardware has to have

A GPU with **Vulkan Video encode *and* decode** queues for H.264 — this app does
both. Without encode, the encoder logs `the encoder session could not be minted;
every later frame is discarded` and the video track never produces. Without
decode, `H264Decoder` refuses in `setup()`, sits in state `Error` in `streamlib
graph`, and the window stays black while the audio half keeps running.

## Observing it

The running app is a node. From another terminal:

```bash
streamlib nodes           # the live nodes on this machine
streamlib graph           # this node's processors, ports and links, as JSON
streamlib logs --follow   # the node's JSONL log
```

`graph` also carries an `extensions` key — the capabilities the installed
extension wheels registered at startup. `moq` appearing there is the proof the
hook ran in the app process.

The publisher logs its broadcast name, container and track count at `setup()`,
then says `first bag published to the broadcast`. The subscriber says
`subscribed to <broadcast>`, then `first bag written on encoded_video`. A
subscriber that connects but never writes is subscribed to a track name nothing
is publishing — check the numbering rule above.

## Runtime knobs

| Env var | Effect |
| --- | --- |
| `STREAMLIB_MOQ_RELAY_URL` | **Required.** The relay, token included. A credential — export it, never commit it. |
| `STREAMLIB_MOQ_BROADCAST` | The broadcast name both halves use. Default `streamlib/moq-broadcast-roundtrip`. |
| `STREAMLIB_CAMERA_DEVICE` | Capture device (e.g. `/dev/video0`). Unset picks the first device found. |

The microphone and the speakers take the backend's defaults. Every codec block
is added bare; their keys are written down in `streamlib/_engine.pyi`, and the
two MoQ processors' in `streamlib_moq`'s own stub.
