---
name: list-running-streams
description: List the streams the machine's one StreamLib runtime holds — running, stopped or failed — so you can pick one by its stream id before running any other control verb. Use when you need to know what is running — after loading a stream with `tatolab run` / `tatolab dev`, when a verb refuses a stream id, or any time you must find the stream id every other verb takes. Wraps `tatolab streams`.
---

# list-running-streams

The entry point for every control-verb workflow. A machine runs one runtime, `tatolabd`, and every stream on it loads into that one runtime — each Python processor still runs in its own helper process; `tatolab streams` asks the runtime which streams it holds. Every verb reaches the runtime through one socket at a fixed path, `local-api.sock` in this user's runtime directory (`$XDG_RUNTIME_DIR/streamlib/`, else `/tmp/streamlib-<uid>/`), so no verb takes a target flag. A runtime on another machine is reached by running the verb there (`ssh <machine> tatolab streams`).

## Steps

### 1. List the streams
```bash
tatolab streams
```
An aligned table, then a line under it per failed stream saying why it failed:

```
ID                   NAME    STATE    NODES  PROJECT          FUNCTION
camera-main-3f2a9c   main    running  4      /home/me/camera  stream:main
lab-rig-0a1b2c       rig     running  6      /home/me/lab     stream:rig
old-main-9f8e7d      main    stopped  -      /home/me/old     stream:main
```

- `ID` — the stream id, which `stop`, `start`, `rm`, `expose`, `graph --stream` and `logs --stream` take, and the `<stream id>` in a port's broadcast `<machine>/<stream id>.hang`. The runtime computes it from the stream's location — `<folder>-<function>-<hash>` over the resolved project directory and the stream function — so running the same project again keeps it, and another checkout of the project gets another.
- `NAME` — the display name: the function's name, or `--name` on `run`. It is a label, never an address.
- `STATE` — `running` (loaded; or recorded and loaded again at the runtime's next start, with `-` nodes), `stopped` (unloaded by `tatolab stop`), or `failed` (its code failed at its last load, or it was implicated in the runtime's last two crashes in a row or unable to re-load at its start). Every stream the runtime loads is recorded, whichever `tatolab run`, `tatolab dev` or MCP connection loaded it, and comes back on every start of the runtime until `stop` or `rm`.
- `NODES` — the loaded graph's node count; `-` when the stream is not loaded.
- `PROJECT` and `FUNCTION` — the stream's location: the project directory the runtime compiled it in, and its stream function as `module:function`; `-` for a record an earlier runtime wrote, which is listed `failed` until `rm` forgets it.

### 2. Read the outcome
- **A `running` row with a node count** — that stream is loaded; hand its id to `drive-running-stream`.
- **`No streams in this runtime.`** — the runtime answers and holds nothing. Load one with `tatolab run --dir <project>`; it prints the stream id and returns.
- **`no runtime is running on this machine: nothing answers at <socket>`** — no runtime is up for this user. The machine's runtime is a service (`systemctl --user start tatolabd` on Linux, the "Run Tatolab" switch in `Tatolab.app` on macOS) or `tatolabd` in a terminal — the owner's to start; a session that needs a runtime of its own starts the test-only build instead (`drive-running-stream`). When another user holds the machine, the refusal names them.
- **`preparing` / `stopping`** — a queued action is working on the stream: a load or start, or a stop or `rm`. Read the row again once it has run.
- **`stopped` / `failed`** — not loaded. `tatolab start <name>` loads it again (retrying a failed one); `tatolab rm --dir <project>` forgets every stream of its project.

## Notes
- No flags. `tatolab streams` takes none.
- No token: the socket is created `0600`, so only the user the runtime serves can reach it.
- `tatolab graph` with no `--stream` returns every loaded stream's graph under the machine's name — use it when you need topology, not just names (`inspect-live-graph`).
