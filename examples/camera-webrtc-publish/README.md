# camera-webrtc-publish

Camera + microphone → H.264 and Opus → one WHIP session, two RTP tracks.

The showcase of a **node package**: `WhipPublisher` is not a built-in. It ships
in the `tatolab-webrtc` package — Rust inside, an ordinary `@node.process` class as the
binding — and this project reaches it the way it reaches any third-party node, by
its import path.

## The model this example teaches

**A node package is an ordinary Python package.** There is no plugin manifest,
no registry and no ABI: `tatolab-webrtc` is one more dependency in
`pyproject.toml`, and a node's `type` names the class. This project's
one node, `nodes/whip_publisher_to_the_runtime_environments_endpoint.py`,
subclasses `WhipPublisher` only to read its endpoint from the environment, and
the graph links both encoders into its `tracks`:

```json
{
  "nodes": [
    {"name": "h264encoder", "type": "tatolab.h264.processors:H264Encoder", "config": {}},
    {"name": "opusencoder", "type": "tatolab.opus:OpusEncoder", "config": {}},
    {"name": "whippublisher",
     "type": "nodes.whip_publisher_to_the_runtime_environments_endpoint:WhipPublisherToTheRuntimeEnvironmentsEndpoint",
     "config": {}}
  ],
  "links": [
    {"source": {"node": "h264encoder", "port": "encoded_video"}, "target": {"node": "whippublisher", "port": "tracks"}},
    {"source": {"node": "opusencoder", "port": "encoded_audio"}, "target": {"node": "whippublisher", "port": "tracks"}}
  ]
}
```

The file itself names each node and each link end by the node's UUID; the
names stand in for them here, and the camera and microphone feeding the
encoders are left out.

The package's WebRTC stack comes up inside the publisher's own interpreter the
first time a session needs it. Nothing in the graph asks for it.

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

With this machine's runtime running (`tatolabd`, which Tato Studio starts),
and the WHIP URL in *its* environment (see below):

```bash
tatolab dev
```

The runtime makes the project's `.venv` on its first load: it installs `tatolab-stream` from PyPI and builds `tatolab-camera`, `tatolab-h264`, `tatolab-microphone`, `tatolab-opus` and `tatolab-webrtc` from this checkout, as editable path dependencies
(`[tool.uv.sources]` in `pyproject.toml`); each module is a maturin project,
so the build needs a Rust toolchain.

`tatolab dev` follows the project's log and loads the project again on every
saved edit; `tatolab run` queues the load and returns. Ctrl-C ends `tatolab dev`
and leaves the stream loaded; `tatolab stop <stream>` stops it.

## The URL is a credential

Cloudflare Stream puts the stream key in the URL's path, so the WHIP URL *is*
the ingest credential — there is nothing else to authenticate with. It never
appears in the graph document, which the project's history keeps: the
publisher reads it from `STREAMLIB_WHIP_URL` as its node is constructed, and
refuses to start without it rather than falling back to a placeholder that would
fail later and further away. An endpoint that authenticates with RFC 9725's
`Authorization: Bearer` instead also takes `STREAMLIB_WHIP_BEARER_TOKEN`;
Cloudflare Stream needs none.

**Each node's process inherits `tatolabd`'s environment, so the variable is
read from there — not the shell `tatolab dev` runs in.** Exporting it before
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

`read -s` keeps the URL out of your shell history. Every node that runtime
runs can read the variable.

## What the hardware has to have

A hardware **H.264 encoder**: Vulkan Video encode queues on Linux, VideoToolbox
on macOS. Without one the encoder's session fails to mint, logs one error line
and discards every later frame, so the video track never produces. The audio
track publishes regardless, and the session still offers both, because the
wiring — two links — settles the offer, not the media.

Nothing here decodes, so no decoder is needed. Playing the stream back is
`WhepPlayer`'s job, from the same package.

## Observing it

From another terminal, in this folder:

```bash
tatolab streams                  # the streams this machine's runtime holds, `main` among them
tatolab graph --stream <stream>  # nodes, ports, links and counters, as JSON
tatolab logs -f                  # the project's records, Rust and Python alike
```

The nodes are `camerasource`, `h264encoder`, `microphonesource`, `opusencoder`
and `whippublisher`. No node's config holds the WHIP URL, so `tatolab graph`
never prints it.

The publisher logs `opening a session on the first bag`, then `the relay
accepted the session`, then `bags_published=` every 300 messages. A stream with
none of these and no error line means nothing reached the publisher — look
upstream, at the encoders.

To watch what is being published on this machine too, open `camerasource`'s
`video` or `h264encoder`'s `encoded_video` in Tato Studio's viewer.

## Knobs

The credential is in `tatolabd`'s environment. Every other knob is a node's
config in `streams/main.graph.json`; change it in Tato Studio's inspector or with
the runtime's `set_node_config` MCP tool, which records the edit in the project's
history and applies it to the running stream.

| Knob | Where | Effect |
| --- | --- | --- |
| `STREAMLIB_WHIP_URL` | `tatolabd`'s environment | **Required.** The WHIP endpoint. A credential — never in a file in this directory. |
| `STREAMLIB_WHIP_BEARER_TOKEN` | `tatolabd`'s environment | Bearer token, for an endpoint that wants one. Cloudflare Stream does not. |
| `device_id` | `camerasource` | Capture device — a V4L2 path (`/dev/video2`) on Linux, an AVFoundation unique ID on macOS. Absent picks the first device found. |
| `device_id` | `microphonesource` | Capture device. Absent: the audio backend's default device. |
| `bitrate_bps` | `h264encoder`, `opusencoder` | Target bitrate. Absent: the encoder's own default. |

Each node's keys are its module's config dataclass: `CameraSourceConfig` in
`tatolab.camera`, `H264EncoderConfig` in `tatolab.h264`, `MicrophoneSourceConfig`
in `tatolab.microphone`, and `OpusEncoderConfig` in `tatolab.opus`.

## One known offset

Video reaches the far end presenting roughly one frame interval later than
audio. The RTP payloader applies a sample's duration to the frame *after* it, so
a publisher that cannot see the next frame without delaying this one numbers
video one frame behind real time; audio takes its duration from each packet's
own sample count and is exact. At 30 fps that is about 33 ms — inside the
ITU-R BT.1359 comfort bound, but not zero.
