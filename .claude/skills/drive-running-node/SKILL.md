---
name: drive-running-node
description: Resolve exactly one running StreamLib node and health-check it with a live `graph` round-trip, then pin its `--url` / `--node` so every following control verb targets the same node. Use at the start of a session against a running app — after `discover-running-nodes`, or whenever you are about to inspect, tap, read logs, or capture evidence and need a confirmed target locked in. Wraps `streamlib nodes` then `streamlib graph`.
---

# drive-running-node

Turns "some node is running" into "this specific node answers, and here is how I address it." A `graph` call is the cheapest full control-plane round-trip, so it doubles as the health check: if `graph` returns the live topology, the node is drivable.

## Steps

### 1. Find the candidates
```bash
streamlib nodes
```
Read the `RUNTIME_NAME` and `CONTROL_URL` of the row you want (see `discover-running-nodes` for the columns). Copy one identifier — the runtime name or its `runtime_id` (both for `--node`), or the `control_url` (for `--url`).

### 2. Health-check + pin the target
Pick ONE addressing form and reuse it verbatim on every later verb:

By the name the runtime carries (or its `runtime_id` — `--node` takes either):
```bash
streamlib graph --node <runtime name>
```
By explicit control URL:
```bash
streamlib graph --url <control_url>
```
If exactly one node is live, both flags may be omitted and the resolver uses that sole node:
```bash
streamlib graph
```

A JSON graph dump (processors, links, states, metrics, loaded capability extensions) means the node is healthy and the address is good. A non-zero exit means it is not drivable:
- `no running StreamLib nodes found` — nothing is running; start a node.
- `N live nodes — pick one with --node <runtime name or id> or --url <url>` — more than one is live and you passed neither flag; re-run with a specific `--node`/`--url`.
- `no live node named <name>, and none with that runtime_id` — the `--node` value is wrong or the node exited; re-run `streamlib nodes`. `N live nodes answer to <name>` means two runtimes were given one name: pick one by `runtime_id`.
- A transport/HTTP error — the endpoint is unreachable or auth failed (set `STREAMLIB_MCP_TOKEN`).

### 3. Pin it for the rest of the session
Record the chosen `--node <runtime name>` (preferred — stable across a port change and across runs) or `--url <control_url>`, and pass the same flag to every subsequent verb (`inspect-live-graph`, `tap-live-channel`, `capture-node-evidence`, `teardown-running-node`).

## Notes
- Prefer `--node <runtime name>` over `--url` when several nodes may be live — it survives the `ApiServer`'s port auto-increment, and unlike a `runtime_id` it is the same on every run of one app.
- `STREAMLIB_MCP_TOKEN`, when set, rides as the bearer token on the round-trip; export it once for the whole session.
