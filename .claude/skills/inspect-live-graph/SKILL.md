---
name: inspect-live-graph
description: Dump a running StreamLib stream's live graph — nodes by name, ports, links, exposed ports, the machine's name, states, and metrics — as JSON, to use as ground truth before tapping it. Use when you need the current topology of a running stream: to form a channel address for `tap-live-channel`, to learn the exact port names a node publishes on, or to diff the graph across a stream-code change. Wraps `tatolab graph`.
---

# inspect-live-graph

The read-only ground-truth verb. Everything else keys off names that only the live graph knows — node names and port names, which with the stream name form a channel's address, `<stream>/<node>/<port>`. Never guess these; dump the graph and read them.

## Steps

### 1. Export the live graph
```bash
tatolab graph --stream <stream>
# or every loaded stream at once:
tatolab graph
```
With `--stream`, the result is that stream's graph in the shape a load takes: `stream`, `nodes`, `links`, `exposed`, and `machine` (this machine's name). Without it, `{machine, streams: [...]}` holds every loaded stream's graph.

### 2. Read what you need out of it
- **Node names** — each node's `name`, which instances the runtime actually stood up, and under which `type`. A name is cast to lowercase URL-safe (`name="Front Camera"` is `front-camera`; a defaulted `CameraSource` is `camerasource`, a second one `camerasource-2`).
- **Port names** — the exact input / output port names each node declares under `ports`, as the runtime reports them rather than as the source reads.
- **Links** — each end is rendered right-anchored: `{node, port}` in this stream, `{stream, node, port}` for a source in another stream on this machine.
- **Channel addresses** — the tap target is `<stream>/<node>/<port>`: the stream's name, the source node's `name`, and its output port under `ports.outputs`. Feed it to `tap-live-channel`. `<machine>/<stream>/<node>/<port>` names the same port with `machine` in front.
- **States / metrics** — confirm a node is `Running` and moving data (non-zero counters) rather than merely instantiated.

### 3. Save it when it is evidence
To freeze the topology for a PR or a before/after diff, redirect to a file (`graph` has no `--output` flag):
```bash
tatolab graph --stream <stream> > /tmp/graph-before.json
```
For a full evidence bundle (graph + tapped bags + logs), use `capture-stream-evidence`.

## Notes
- Pure read: `graph` never mutates a stream.
- `graph` renders every node's config, secrets included (a WHIP or WHEP endpoint carries its key in the URL). Read such a graph in a pipe; never save or attach it.
- A non-zero exit is a refusal or transport error, not an empty graph — a runtime holding no streams still returns valid JSON with an empty `streams`. See `drive-running-stream` for the refusals.
