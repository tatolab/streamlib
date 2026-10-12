---
name: tap-live-channel
description: Attach a read-only tap to one output port of a running StreamLib stream and collect a bounded sample of raw bags to confirm data is actually flowing. Use to answer "are frames/samples really moving through this link?" — after inspecting the graph, or to spot-check a link before and after a stream-code change. Wraps `tatolab tap`.
---

# tap-live-channel

Proof-of-life for a link. A channel is addressed as its output port, `<stream>/<node>/<port>` — the stream's name, the source node's name, and the output port it publishes on. Tapping collects a bounded sample of raw bags off that port and prints each bag's hex plus its byte length; it does NOT decode pixels or audio samples, so treat it as "bytes are flowing, and roughly this many per bag," not as a rendered frame.

## Steps

### 1. Get the address from the live graph
Run `inspect-live-graph` and read the stream's name, the source node's `name`, and its output port under `ports.outputs`, then join them:
```
<stream>/<node>/<port>   e.g.  lab/camera/video
```
`<machine>/<stream>/<node>/<port>` naming this machine is the same port. An address naming another machine is refused until the sharing step.

### 2. Tap a bounded sample
```bash
tatolab tap lab/camera/video --count 10
# or, the stream named apart:
tatolab tap camera/video --stream lab --count 10
```
`--count` bounds how many bags to collect before returning; omitted, the tool's own default bound applies. A port is tappable once a link carries from it.

### 3. Interpret the sample
- **Bags arrive, non-zero byte lengths** — data is flowing on that link; the source node is producing.
- **Byte lengths change frame-to-frame** — live, varying content rather than a stuck buffer.
- **Zero bags / the call returns empty** — no bag arrived inside the sample's short window, which a slow or bursty port can do; tap again before concluding the port is quiet, then re-check the address against the graph, and confirm the source node is `Running` (`inspect-live-graph`).
- **A bag flagged `hex_truncated`** — it was larger than `--max-bag-bytes` and cannot be decoded; raise the cap.

## Notes
- `tap` has NO `--output` flag. To persist the sample as evidence, redirect stdout (`tatolab tap ... > bags.json`) — see `capture-stream-evidence`.
- Read-only: a tap never mutates the graph and never blocks the producer or disturbs the real subscribers on the port.
- The sample is bounded by a short collection window as well as `--count`, so a large `--count` on a slow port returns fewer bags; tap again for more.
