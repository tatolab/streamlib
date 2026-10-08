---
name: tap-live-channel
description: Attach a read-only tap to one named channel of a running StreamLib node and collect a bounded sample of raw bags to confirm data is actually flowing. Use to answer "are frames/samples really moving through this link?" — after inspecting the graph, or to spot-check a link before and after a stream-code change. Wraps `tatolab tap`.
---

# tap-live-channel

Proof-of-life for a link. A channel is the output port's address, `<runtime_name>/<node>/<port>` — the runtime the source node runs in, the node's name, and the output port it publishes on. Tapping collects a bounded sample of raw bags off that channel and prints a hex preview plus byte length per bag; it does NOT decode pixels or audio samples, so treat it as "bytes are flowing, and roughly this many per bag," not as a rendered frame.

## Steps

### 1. Get the channel name from the live graph
Run `inspect-live-graph` and read the top-level `runtime_name`, the source node's `name`, and its output port under `ports.outputs`, then join them:
```
<runtime_name>/<node>/<port>   e.g.  lab-one/camera/frames
```
A tap reads a channel on the runtime it targets: a channel naming another runtime is refused, naming that runtime. Point the tap at that runtime instead (`--node <its runtime name>`), channel unchanged.

### 2. Tap a bounded sample
The channel is a positional argument; `--count` bounds how many bags to collect before returning:
```bash
tatolab tap --node <runtime name> lab-one/camera/frames --count 10
# or, when exactly one node is live:
tatolab tap lab-one/camera/frames --count 10
```
Each collected bag prints as a hex preview and a byte length. Omitting `--count` uses the tool's own default sample bound.

### 3. Interpret the sample
- **Bags arrive, non-zero byte lengths** — data is flowing on that link; the source processor is producing.
- **Byte lengths change frame-to-frame** — live, varying content (e.g. a moving camera image) rather than a stuck buffer.
- **Zero bags / the call blocks then returns empty** — nothing is publishing on that channel; re-check the channel name against the graph, and confirm the source processor is running (states/metrics in `inspect-live-graph`).

## Notes
- `tap` has NO `--output` flag. To persist the sample as evidence, redirect stdout (`tatolab tap ... > frames.json`) — see `capture-node-evidence`.
- Read-only: a tap never mutates the graph and never disturbs the real subscribers on the channel.
- The channel positional and `--count` can appear in either order; the node is selected by `--node` exactly like the other verbs.
