---
name: drive-running-stream
description: Confirm the machine's StreamLib runtime answers and that one named stream on it is loaded and running, with a live `graph --stream` round-trip, then use that stream name on every following verb. Use at the start of a session against a running stream — after `list-running-streams`, or whenever you are about to inspect, read a track, read logs, or capture evidence — and when the session needs a runtime of its own, isolated from the machine's. Wraps `tatolab streams` then `tatolab graph --stream`.
---

# drive-running-stream

Turns "a stream is running" into "this stream answers, and here is its name." A machine has one runtime and every verb reaches it through one fixed socket, so there is no target to pin — only the stream. A `graph --stream` call is the cheapest full round-trip, so it doubles as the health check.

## Steps

### 1. Find the stream
```bash
tatolab streams
```
Read the `NAME` of the row you want (see `list-running-streams`). Only an `attached` or `kept` stream is loaded.

### 2. Health-check it
```bash
tatolab graph --stream <stream>
```
A JSON graph (nodes, links, exposed ports, states, metrics, and the `machine` name) means the stream is drivable. A stream answers from its load while its nodes are still `Pending`; it is running once every node's `components.state` is `Running` — poll until it is when you have just loaded it. A non-zero exit means it is not drivable:
- `no runtime is running on this machine: nothing answers at <socket>` — no runtime serves this user. Start one (below), or the machine's service.
- A refusal naming the loaded streams — no loaded stream has that name; re-read `tatolab streams` (a `stopped` or `failed` stream is not loaded).

### 3. Use the name for the rest of the session
Pass the same stream name to every later verb: `graph --stream <stream>`, `logs --stream <stream>`, `expose <stream> <node> <port>`, `stop <stream>`, and the broadcast `<machine>/<stream>.hang` that `tatolab-read-track` reads a port's track in (`inspect-live-graph`, `read-live-track`, `capture-stream-evidence`, `stop-running-stream`).

## A runtime of the session's own

The machine's runtime belongs to the owner: its kept streams, its state directory, and the machine lock. A session that needs a runtime to load its own stream into — a worktree build, a change under test — starts an isolated one from the test-only build instead of stopping or reusing the machine's:

```bash
cargo xtask build-runtime --machine-directories-under-a-test-root
export TATOLAB_TEST_MACHINE_ROOT="$(mktemp -d /tmp/tl-XXXXXX)"
( TATOLAB_TEST_LOCAL_RELAY_LOOPBACK_PORT=14443 TATOLAB_TEST_LOCAL_RELAY_EVERY_INTERFACE_PORT=14445 \
  TATOLAB_TEST_LOOPBACK_HTTP_PORT=14446 target/tatolab-runtime/bin/tatolabd > "$TATOLAB_TEST_MACHINE_ROOT/tatolabd.log" 2>&1 &
  echo $! > "$TATOLAB_TEST_MACHINE_ROOT/tatolabd.pid"
  wait $!; echo $? > "$TATOLAB_TEST_MACHINE_ROOT/tatolabd.exit" ) &
```
The pid and, once it ends, the exit status land in files, because a later Bash call cannot `wait` on a process an earlier one started.

- The test-only build moves the machine lock, the state directory and the runtime directory — the local API socket among them — under `TATOLAB_TEST_MACHINE_ROOT`, so it runs beside the machine's runtime and re-loads none of its kept streams. Keep the root short, under `/tmp`: socket paths must stay under 104 bytes.
- A test build never binds the machine's relay or HTTP ports. It runs a local relay only with both `TATOLAB_TEST_LOCAL_RELAY_*_PORT`s set, and serves the loopback listing and `/watch` only with `TATOLAB_TEST_LOOPBACK_HTTP_PORT` set — so its tracks are read at `http://127.0.0.1:14443` and its listing is `http://127.0.0.1:14446/` above, never `4443` / `4446`, which are the machine's runtime's. Pick free ports when another session runtime holds those.
- Drive it with its own `tatolab`, `target/tatolab-runtime/bin/tatolab` — an installed `tatolab` on PATH is a release build and reaches the machine's runtime instead. It refuses to run without `TATOLAB_TEST_MACHINE_ROOT`, and only reaches this runtime with the same value. Shell state does not persist between Bash calls, so set it on every call that runs a verb.
- Set every stream setting on `tatolabd`, not on `tatolab run`: the runtime compiles a stream and starts its processor interpreters with its own environment, never the caller's (`STREAMLIB_CAMERA_DEVICE=/dev/videoN target/tatolab-runtime/bin/tatolabd …`).
- Poll `tatolab streams` until it answers (`No streams in this runtime.`), then load: `tatolab run --dir <project> --name <stream> &` (attached) or `tatolab run -d --dir <project> --name <stream>` (kept, in this root's state directory only).
- It is a test build, never a release: the fixture drivers under the engine's `tests/fixtures/` refuse it, so rebuild with plain `cargo xtask build-runtime` before running one. End this runtime first (`stop-running-stream`): a rebuild replaces the lend its processor interpreters borrow and the binaries in `bin/`, whichever kind it builds. The devices are still the machine's — one consumer per `/dev/videoN`.
- End it with `stop-running-stream`, then delete the root.

## Notes
- To drive the runtime from an MCP host instead of the CLI: `claude mcp add tatolab -- tatolab mcp`, or `claude mcp add tatolab -- ssh <machine> tatolab mcp` for another machine's runtime. Every tool about one stream names it with `stream`; `graph` takes it optionally, `run_stream` takes `name`, and `list_streams` and the relay tools take none. No token. MCP observes the graph and the logs; what a port carries is read as a track, never through a tool.
