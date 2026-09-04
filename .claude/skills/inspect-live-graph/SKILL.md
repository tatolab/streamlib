---
name: inspect-live-graph
description: Dump a running StreamLib node's live processor graph — processors, ports, links, channel names, states, and metrics — as JSON, to use as ground truth before tapping it. Use when you need the current topology of a running app: to find a channel name for `tap-live-channel`, to learn the exact port names a processor publishes on, or to diff the graph across an app-code change. Wraps `streamlib graph`.
---

# inspect-live-graph

The read-only ground-truth verb. Everything else keys off names that only the live graph knows — processor ids, port names, and channel data-service names (`{source_processor}/{source_output_port}`). Never guess these; dump the graph and read them.

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
The result is the `graph` MCP tool's JSON (pretty-printed): processors, links, states, metrics, and the capability extensions loaded in the node's process.

### 2. Read what you need out of it
- **Processor ids** — which instances the node actually stood up, and under which type.
- **Port names** — the exact input / output port names each processor declares, as the node reports them rather than as the source reads.
- **Channel names** — form the tap target `{source_processor}/{source_output_port}` from the source processor and its output port; feed it to `tap-live-channel`.
- **States / metrics** — confirm a processor is running and moving data (non-zero counters) rather than merely instantiated.

### 3. Save it when it is evidence
To freeze the topology for a PR or a before/after diff, redirect to a file (`graph` has no `--output` flag — use shell redirection):
```bash
streamlib graph --node <runtime_id> > /tmp/graph-before.json
```
For a full evidence bundle (graph + tapped frames + logs), use `capture-node-evidence`.

## Notes
- Pure read: `graph` never mutates the node.
- A non-zero exit is a resolver or transport error, not an empty graph — an empty pipeline still returns valid JSON with empty arrays. See `drive-running-node` for resolver error messages.
