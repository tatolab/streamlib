---
name: stop-running-stream
description: Stop a stream on the machine's StreamLib runtime with `tatolab stop`, or forget it with `tatolab rm`, then confirm with `tatolab streams` that it is no longer loaded so its camera / GPU claim is released — and end a runtime the session started for itself. Use when done driving a stream — to free a `/dev/videoN` camera for another consumer, release the GPU, or clear a kept stream before loading a fresh one. Wraps `tatolab stop`, `tatolab rm` and `tatolab streams`.
---

# stop-running-stream

Ends a stream's work without touching the runtime. The machine's runtime stays on, as a service does; it has no shutdown verb. A stream leaves it by `stop` or `rm`, and an attached stream also leaves when the connection that loaded it closes.

## Steps

### 1. Stop or remove it
```bash
tatolab stop <stream>
```
Unloads the stream. A kept stream is recorded `stopped` and stays stopped across the runtime's restarts until `tatolab start <stream>`; an attached stream's `tatolab run` ends. A warning that the stop was not recorded means a restart of the runtime loads the kept stream again.

```bash
tatolab rm <stream>
```
Unloads the stream and forgets it — the way to clear a kept, stopped or failed stream for good.

For an attached stream you loaded in this session, ending its `tatolab run` is equivalent: Ctrl-C, or `kill <pid of tatolab run>` — the runtime unloads the stream when that connection closes. To stop a `tatolab dev` session for good, end `tatolab dev` itself; a `tatolab stop` ends its current load, and `dev` loads again on the next saved edit.

### 2. Confirm it is gone
```bash
tatolab streams
```
An attached stream's row is gone; a kept one shows `stopped` with `-` nodes; after `rm` no row remains. A stream no longer loaded has released its camera and GPU work for the next run or another worktree.

### 3. End a runtime the session started (only that one)
Never stop the machine's runtime on your own — it is the owner's, and other streams live in it. Before a fixture driver that needs it down (they refuse while it answers), ask the owner; then stop it — `systemctl --user stop tatolabd` on Linux, the "Run Tatolab" switch off in `Tatolab.app` on macOS, or Ctrl-C a `tatolabd` in a terminal — and start it again the same way once the fixture ends. A runtime the session started for itself (`drive-running-stream`, the test-only build) ends by a signal, and must end cleanly:
```bash
tatolabd_pid="$(cat "$TATOLAB_TEST_MACHINE_ROOT/tatolabd.pid")"
if [ ! -f "$TATOLAB_TEST_MACHINE_ROOT/tatolabd.exit" ] \
    && [ "$(ps -o comm= -p "$tatolabd_pid")" = tatolabd ]; then
  kill -INT "$tatolabd_pid"
fi
```
A present `tatolabd.exit` means it already ended, and its pid may since name another process — so it is signalled only while it has not exited and the pid is still a `tatolabd`.
Wait up to ~15 s for `$TATOLAB_TEST_MACHINE_ROOT/tatolabd.exit` to appear and read the exit status it holds: `0` is a clean stop; `124` means its watchdog abandoned a stream's teardown; a runtime that needs `kill -9` is a finding, not a flake. Then delete its `TATOLAB_TEST_MACHINE_ROOT`.

## Notes
- One camera consumer per `/dev/videoN` — unloading the stream that opened it is what frees the device.
- A runtime refuses to stop nothing: `stop` / `rm` of a name it does not hold is refused, naming the streams it does hold.
