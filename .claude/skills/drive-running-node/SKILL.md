---
name: drive-running-node
description: Resolve exactly one running StreamLib node and health-check it with a live `graph` round-trip, then pin its `--node` so every following control verb targets the same node. Use at the start of a session against a running app — after `discover-running-nodes`, or whenever you are about to inspect, tap, read logs, or capture evidence and need a confirmed target locked in. Wraps `streamlib nodes` then `streamlib graph`.
---

# drive-running-node

Turns "some node is running" into "this specific node answers, and here is how I address it." A `graph` call is the cheapest full control-plane round-trip, so it doubles as the health check: if `graph` returns the live topology, the node is drivable.

## Steps

### 1. Find the candidates
```bash
streamlib nodes
```
Read the `RUNTIME_NAME` and `RUNTIME_ID` of the row you want (see `discover-running-nodes` for the columns). Copy one — `--node` takes either.

### 2. Health-check + pin the target
Pick ONE identifier and reuse it verbatim on every later verb:
```bash
streamlib graph --node <runtime name>
```
If exactly one node is live, `--node` may be omitted and the resolver uses that sole node:
```bash
streamlib graph
```

A JSON graph dump (nodes, links, states, metrics, loaded capability extensions) means the node is healthy and the address is good. A non-zero exit means it is not drivable:
- `no running StreamLib nodes found` — nothing is running; start a node.
- `N live nodes — pick one with --node <runtime name or id>` — more than one is live and you passed no `--node`; re-run with one.
- `no live node named <name>, and none with that runtime_id` — the `--node` value is wrong or the node exited; re-run `streamlib nodes`. `N live nodes answer to <name>` means two runtimes were given one name: pick one by `runtime_id`.
- An error naming the node's socket path — the registry lists it but nothing answers there; the node is starting, wedged, or died without deregistering. Re-run `streamlib nodes`.

### 3. Pin it for the rest of the session
Record the chosen `--node <runtime name>` (preferred — stable across runs) or `--node <runtime_id>`, and pass the same flag to every subsequent verb (`inspect-live-graph`, `tap-live-channel`, `capture-node-evidence`, `teardown-running-node`).

## Notes
- Prefer `--node <runtime name>` over a `runtime_id` when several nodes may be live — the name is the same on every run of one app.
- To drive the same node from an MCP host instead of the CLI: `claude mcp add streamlib -- streamlib mcp` (append `--node <runtime name>` when several are live), or `claude mcp add streamlib -- ssh <machine> streamlib mcp` for a node on another machine. No token.
