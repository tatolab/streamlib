# camera-webrtc-publish

Camera + microphone → H.264 and Opus → one WHIP session, two RTP tracks.

The showcase of a **node package**: `WhipPublisher` is not a built-in. It ships
in the `tatolab-webrtc` package — Rust inside, an ordinary `@node` class as the
binding — and this stream reaches it the way it reaches any third-party node,
by importing it and passing the class to `add`.

## The model this example teaches

**A node package is an ordinary Python package.** There is no plugin manifest,
no registry and no ABI: `tatolab-webrtc` is one more dependency in
`pyproject.toml`, and `from tatolab.webrtc import WhipPublisher` is the whole
integration:

```python
@stream
def main(stream_builder: StreamBuilder) -> None:
    ...
    publisher = stream_builder.add(WhipPublisher, config={"url": ...})
    stream_builder.connect(
        video_encoder.output("encoded_video"), publisher.input("tracks")
    )
    stream_builder.connect(
        audio_encoder.output("encoded_audio"), publisher.input("tracks")
    )
```

The package's WebRTC stack comes up inside the publisher's own interpreter the
first time a session needs it. Nothing in `stream.py` asks for it.

**A track is an inbound link.** `tracks` is a fan-in input: any number of
links may name it, and each one becomes one RTP track whose medium — video or
audio — the first message on that link settles, by the codec it carries. A WHIP
session carries at most one video track and one audio track, so the publisher
holds two links at most. More than two is refused in `setup()`, before anything
is offered to the endpoint. A second link of the same *medium* cannot be known
until it delivers, so it is refused by name on its first message. One publisher
is one session: to publish two cameras, add two publishers.

**The session opens on the first message, not in `setup()`.** A round trip to
the endpoint inside `setup()` would spend the child interpreter's sixty-second
start-up budget, and an endpoint outage there would take the whole stream down.
So the stream starts whether or not the endpoint is reachable, and a failure to
connect is reported against the first message.

**The publisher runs in its own child interpreter**, like every Python node.
The encoded messages cross to it from the runtime, which is why this stream
sits on the *encoded* side of the codec blocks: an access unit at any sane
bitrate is kilobytes, and no raw frame or GPU surface ever crosses.

## Run it

With this machine's runtime running (`tatolabd`, which the installer starts),
and the WHIP URL in *its* environment (see below):

```bash
uv sync
tatolab dev
```

`uv sync` installs `tatolab-stream` from PyPI and builds `tatolab-webrtc` from
this checkout, as an editable path dependency (`[tool.uv.sources]` in
`pyproject.toml`). The package is a maturin project, so the build needs a Rust
toolchain. Outside this checkout, drop the `[tool.uv.sources]` entry and the
dependency resolves from PyPI like any other.

Use `tatolab dev` while editing — it loads the stream again on every saved edit
— and `tatolab run` otherwise. Both keep the stream as long as the command
runs; Ctrl-C unloads it, which closes the WHIP session.

## The URL is a credential

Cloudflare Stream puts the stream key in the URL's path, so the WHIP URL *is*
the ingest credential — there is nothing else to authenticate with. It never
appears in `stream.py`: the stream reads it from `STREAMLIB_WHIP_URL`, and
refuses to load without it rather than falling back to a placeholder that would
fail later and further away. An endpoint that authenticates with RFC 9725's
`Authorization: Bearer` instead also takes `STREAMLIB_WHIP_BEARER_TOKEN`;
Cloudflare Stream needs none.

**The runtime compiles the stream, so the variable is read from `tatolabd`'s
environment — not the shell `tatolab dev` runs in.** Exporting it before
`tatolab dev` does nothing. Either start the runtime yourself, in a terminal of
its own, with the variable set:

```bash
systemctl --user stop tatolabd     # if the installer runs it as a service
read -rs STREAMLIB_WHIP_URL && export STREAMLIB_WHIP_URL
tatolabd
```

or hand it to the service manager and restart the service, which unloads every
stream the runtime holds:

```bash
read -rs STREAMLIB_WHIP_URL
systemctl --user set-environment STREAMLIB_WHIP_URL="$STREAMLIB_WHIP_URL"
systemctl --user restart tatolabd
```

`read -s` keeps the URL out of your shell history. Every stream that runtime
loads can read the variable, and the URL lands in the publisher's config — see
Observing it.

## What the hardware has to have

A hardware **H.264 encoder**: Vulkan Video encode queues on Linux, VideoToolbox
on macOS. Without one the encoder's session fails to mint, logs one error line
and discards every later frame, so the video track never produces. The audio
track publishes regardless, and the session still offers both, because the
wiring — two links — settles the offer, not the media.

Nothing here decodes, so no decoder is needed. Playing the stream back is
`WhepPlayer`'s job, from the same package.

## Observing it

From another terminal:

```bash
tatolab streams                 # the streams this machine's runtime holds
tatolab graph --stream main     # nodes, ports, links and counters, as JSON
tatolab logs --stream main -f   # the stream's records, Rust and Python alike
```

**`tatolab graph` renders every node's config, so its output carries the WHIP
URL — the credential.** Read it in a pipe and never save it, paste it or attach
it anywhere:

```bash
tatolab graph --stream main | jq '.nodes[] | {name, type}'
```

A node is named after its class, lowercased — `camerasource`, `h264encoder`,
`microphonesource`, `opusencoder`, `whippublisher` — unless `add` passes
`name=`.

The publisher logs `opening a session on the first bag`, then `the relay
accepted the session`, then `bags_published=` every 300 messages. A stream with
none of these and no error line means nothing reached the publisher — look
upstream, at the encoders.

Nothing in this stream is exposed. To watch what is being published on this
machine too, expose the video encoder's output while the stream runs and open
it in a browser:

```bash
tatolab expose main h264encoder encoded_video
```

```text
http://127.0.0.1:4446/watch/<machine>/main
```

`http://127.0.0.1:4446/` lists this machine's name and every stream's exposed
ports.

## Knobs

Everything but the credential is literal config in `stream.py` — edit it and
save while `tatolab dev` runs.

| Knob | Where | Effect |
| --- | --- | --- |
| `STREAMLIB_WHIP_URL` | `tatolabd`'s environment | **Required.** The WHIP endpoint. A credential — never in a file in this directory. |
| `STREAMLIB_WHIP_BEARER_TOKEN` | `tatolabd`'s environment | Bearer token, for an endpoint that wants one. Cloudflare Stream does not. |
| `CAMERA_DEVICE_ID` | top of `stream.py` | Capture device — a V4L2 path (`/dev/video2`) on Linux, an AVFoundation unique ID on macOS. `None` picks the first device found. |
| `device_id` | `MicrophoneSourceConfig` | Capture device. Absent: the audio backend's default device. |
| `bitrate_bps` | `H264EncoderConfig`, `OpusEncoderConfig` | Target bitrate. Absent: the encoder's own default. |

Every built-in's keys are written down in `tatolab.stream`; `WhipPublisher`'s
are `WhipPublisherConfig` in `tatolab.webrtc`.

## One known offset

Video reaches the far end presenting roughly one frame interval later than
audio. The RTP payloader applies a sample's duration to the frame *after* it, so
a publisher that cannot see the next frame without delaying this one numbers
video one frame behind real time; audio takes its duration from each packet's
own sample count and is exact. At 30 fps that is about 33 ms — inside the
ITU-R BT.1359 comfort bound, but not zero.
