---
name: inspect-live-graph
description: Dump a running StreamLib node's live graph — nodes by name, ports, links, the runtime name channels are addressed under, states, and metrics — as JSON, to use as ground truth before tapping it. Use when you need the current topology of a running app: to find a channel name for `tap-live-channel`, to learn the exact port names a node publishes on, or to diff the graph across an app-code change. Wraps `streamlib graph`.
---

# inspect-live-graph

The read-only ground-truth verb. Everything else keys off names that only the live graph knows — node names, port names, and the runtime name a channel is addressed under (`<runtime_name>/<node>/<port>`). Never guess these; dump the graph and read them.

## Steps

### 1. Export the live graph
Target the node with the same flag you pinned in `drive-running-node`:
```bash
streamlib graph --node <runtime_id>
# or
streamlib graph --url <control_url>
# or, when exactly one node is live:
streamlib graph
```
The result is the `graph` MCP tool's JSON (pretty-printed): `stream` (once one is loaded), `nodes`, `links`, `exposed`, `extensions` and `mesh`.

### 2. Read what you need out of it
- **Node names** — each node's `name`, which instances the runtime actually stood up, and under which `type`. A name is cast to lowercase URL-safe (`name="Front Camera"` is `front-camera`; a defaulted `CameraSource` is `camerasource`, a second one `camerasource-2`).
- **Port names** — the exact input / output port names each node declares under `ports`, as the runtime reports them rather than as the source reads.
- **Links** — each end is `{node, port}` for a port here, or `{runtime_name, node, port}` for a port on another runtime.
- **Channel names** — form the tap target `<runtime_name>/<node>/<port>` from `mesh.runtime_name`, the source node's `name` and its output port; feed it to `tap-live-channel`.
- **States / metrics** — confirm a node is running and moving data (non-zero counters) rather than merely instantiated.

### 3. Save it when it is evidence
To freeze the topology for a PR or a before/after diff, redirect to a file (`graph` has no `--output` flag — use shell redirection):
```bash
streamlib graph --node <runtime_id> > /tmp/graph-before.json
```
For a full evidence bundle (graph + tapped frames + logs), use `capture-node-evidence`.

## Notes
- Pure read: `graph` never mutates the node.
- A non-zero exit is a resolver or transport error, not an empty graph — an empty pipeline still returns valid JSON with empty arrays. See `drive-running-node` for resolver error messages.
