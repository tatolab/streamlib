---
name: tap-live-channel
description: Attach a read-only tap to one named channel of a running StreamLib node and collect a bounded sample of raw bags to confirm data is actually flowing. Use to answer "are frames/samples really moving through this link?" — after inspecting the graph, to verify a `submit`/`connect` produced live output, or to spot-check behavior before and after a `hot-swap-live-processor`. Wraps `streamlib tap`.
---

# tap-live-channel

Proof-of-life for a link. A channel is named `{source_processor}/{source_output_port}` — the source processor plus the output port it publishes on. Tapping collects a bounded sample of raw bags off that channel and prints a hex preview plus byte length per bag; it does NOT decode pixels or audio samples, so treat it as "bytes are flowing, and roughly this many per bag," not as a rendered frame.

## Steps

### 1. Get the channel name from the live graph
Run `inspect-live-graph` and read the source processor and its output port, then join them:
```
{source_processor}/{source_output_port}   e.g.  camera/frames
```

### 2. Tap a bounded sample
The channel is a positional argument; `--count` bounds how many bags to collect before returning:
```bash
streamlib tap --node <runtime_id> camera/frames --count 10
# or
streamlib tap --url <control_url> camera/frames --count 10
# or, when exactly one node is live:
streamlib tap camera/frames --count 10
```
Each collected bag prints as a hex preview and a byte length. Omitting `--count` uses the tool's own default sample bound.

### 3. Interpret the sample
- **Bags arrive, non-zero byte lengths** — data is flowing on that link; the source processor is producing.
- **Byte lengths change frame-to-frame** — live, varying content (e.g. a moving camera image) rather than a stuck buffer.
- **Zero bags / the call blocks then returns empty** — nothing is publishing on that channel; re-check the channel name against the graph, and confirm the source processor is running (states/metrics in `inspect-live-graph`).

## Notes
- `tap` has NO `--output` flag. To persist the sample as evidence, redirect stdout (`streamlib tap ... > frames.json`) — see `capture-node-evidence`.
- Read-only: a tap never mutates the graph and never disturbs the real subscribers on the channel.
- The channel positional and `--count` can appear in either order; the node is selected by `--url` / `--node` exactly like the other verbs.
