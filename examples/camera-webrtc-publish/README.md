# camera-webrtc-publish

Camera + microphone → H.264 and Opus → one WHIP session, two RTP tracks.

The first showcase of a **processor extension**: `WhipPublisher` is not a
built-in. It ships in the `streamlib-webrtc` wheel — Rust inside, an ordinary
`@processor` class as the binding — and this app reaches it the way it reaches
any third-party processor, by importing it and passing the class to `rt.add`.

## The model this example teaches

**An extension processor is an ordinary processor.** There is no plugin
manifest, no registry, no ABI. `pip install streamlib-webrtc` is the whole
installation step, and what makes the wheel more than a library is one entry
point pip records — `streamlib.extensions` — which the engine runs once in every
process taking an engine role. That hook is what brings the tokio runtime and
the TLS provider up inside the helper before `WhipPublisher` is imported there.
Nothing in this file asks for it.

**A track is an inbound link.** Both encoders connect to the publisher's one
`tracks` input, and each link becomes one RTP track whose medium its first bag
settles by its `codec`. It is `Mp4Sink`'s shape, reused — with one endpoint
constraint the file writer does not have: a WHIP session carries at most one
video and one audio track. `setup()` refuses more than two links, before
anything is offered; a second link of the same *medium* is not knowable until
its first bag says so by its `codec`, and is refused by name there.

**The session opens on the first bag, not in `setup()`.** A relay round trip
inside `setup()` would spend the helper's sixty-second registration budget, and
a relay outage there would take the whole graph down. So the graph starts
whether or not the endpoint is reachable, and a failure to connect is reported
against the first bag.

**The publisher runs in its own helper process**, like every Python processor.
The encoded bags cross to it over the helper link, which is why this app sits on
the *encoded* side of the codec blocks: an access unit at any sane bitrate is
kilobytes, and no raw frame or surface ever crosses.

## Run it

```bash
uv venv --python 3.12 && uv sync
export STREAMLIB_WHIP_URL='https://customer-....cloudflarestream.com/<key>/webRTC/publish'
streamlib run
```

`uv sync` installs `streamlib` from the simple index pinned in
`pyproject.toml`, and builds `streamlib-webrtc` from this checkout — that is a
maturin project, so it needs a Rust toolchain. Outside a checkout, drop the
`tool.uv.sources` path entry and both wheels resolve from the index.

To work against a checkout of the engine as well, install that checkout's wheel
into this venv:

```bash
uv pip install maturin
maturin develop --manifest-path ../../sdk/streamlib-python-wheel/Cargo.toml
```

## The URL is a credential

Cloudflare Stream puts the stream key in the URL's path, so the WHIP URL *is*
the ingest credential — there is nothing else to authenticate with. It is read
from `STREAMLIB_WHIP_URL` and never written down here; the app refuses to start
without it rather than falling back to a placeholder.

An endpoint that authenticates with RFC 9725's `Authorization: Bearer` instead
takes `STREAMLIB_WHIP_BEARER_TOKEN`.

## What the hardware has to have

A GPU with **Vulkan Video encode** queues for H.264. Without them the encoder
logs `the encoder session could not be minted; every later frame is discarded`
and the video track never produces — the audio track publishes regardless, and
the session offers both because the wiring, not the media, settles the offer.

No decode queue is needed: nothing here decodes. Playing the stream back is
`WhepPlayer`'s job, and the wheel's live fixture
(`packages/streamlib-webrtc/tests/live/`) is where the two meet.

## Observing it

The running app is a node. From another terminal:

```bash
streamlib nodes           # the live nodes on this machine
streamlib graph           # this node's processors, ports and links, as JSON
streamlib logs --follow   # the node's JSONL log
```

`graph` also carries an `extensions` key — the capabilities the installed
extension wheels registered at startup. `webrtc` appearing there is the proof
the hook ran in the app process.

The publisher says `opening a session on the first bag`, then `the relay
accepted the session`, then reports every 300 bags. A silent stream with no
error line means no bag reached it — look upstream at the encoder.

## Runtime knobs

| Env var | Effect |
| --- | --- |
| `STREAMLIB_WHIP_URL` | **Required.** The WHIP endpoint. A credential — export it, never commit it. |
| `STREAMLIB_WHIP_BEARER_TOKEN` | Bearer token, for an endpoint that wants one. Cloudflare Stream does not. |
| `STREAMLIB_CAMERA_DEVICE` | Capture device (e.g. `/dev/video0`). Unset picks the first device found. |

The microphone takes the backend's default capture device. Both encoders are
added bare; their keys are written down in `streamlib/_engine.pyi`, and
`WhipPublisher`'s in `streamlib_webrtc`'s own stub.

## One known offset

Video reaches the far end presenting roughly one frame interval later than
audio. The RTP payloader applies a sample's duration to the frame *after* it, so
a publisher that cannot see the next frame without delaying this one numbers
video one frame behind real time; audio takes its duration from each packet's
own sample count and is exact. At 30 fps that is about 33 ms — inside the
ITU-R BT.1359 comfort bound, but not zero.
