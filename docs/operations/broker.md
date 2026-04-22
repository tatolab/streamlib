# streamlib-broker — operations

`streamlib-broker` is the Linux daemon that brokers cross-process
DMA-BUF surface sharing between the streamlib host and any polyglot
subprocess (Python, Deno). Every FD that crosses a process boundary on
Linux passes through it, so its availability is a hard prerequisite for
every polyglot pipeline.

This doc is operator-facing: how to install it, how to tell whether it's
healthy, where to look when things break. For implementation details
see [`libs/streamlib-broker`](../../libs/streamlib-broker) and the
consumer-side helpers in
[`libs/streamlib-broker-client`](../../libs/streamlib-broker-client).

---

## Install flavors

| Flavor     | Use when                                               | Broker binary                     |
| ---------- | ------------------------------------------------------ | --------------------------------- |
| Production | Installed streamlib release                            | `~/.streamlib/bin/streamlib-broker` (prebuilt) |
| Dev        | Working on streamlib; want the broker to track source  | `cargo run -p streamlib-broker` (always rebuilt) |
| Test       | CI / integration tests                                 | `target/debug/streamlib-broker` (built once per run) |

The **invariant** that keeps dev sane: *in dev, the broker you connect
to is the current source tree.* If you build a fix and connect, you're
exercising the fix — no manual reinstall step.

---

## Production: socket-activated systemd install

This is the recommended deployment path. The `.socket` unit keeps the
listening socket available at all times; the daemon starts on the first
client connect and can idle-exit later without losing the socket.

```bash
mkdir -p ~/.config/systemd/user
cp scripts/streamlib-broker.socket  ~/.config/systemd/user/
cp scripts/streamlib-broker.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now streamlib-broker.socket
```

From then on, every client that connects to
`~/.streamlib/broker.sock` activates the daemon automatically. No
`systemctl start streamlib-broker.service` step is required in the
onboarding flow.

### Mechanics

systemd binds the listening socket itself, then passes it to the daemon
as fd 3 with `LISTEN_FDS=1` and `LISTEN_PID=<daemon pid>` in the
environment. The broker's hand-rolled probe
(`libs/streamlib-broker/src/main.rs::sd_listen_fd`) reads those env
vars, validates pid match when present, and hands the inherited
listener straight to `UnixSocketSurfaceService::with_inherited_listener`
so no `bind()` call is ever made from the daemon.

When the daemon exits (crash, SIGTERM, idle-exit), systemd keeps the
socket. The next client connect re-activates the daemon and it inherits
the same listening socket again. The socket file itself is never
unlinked until the `.socket` unit is stopped.

### Idle-exit (optional)

Pass `--idle-exit-seconds=N` on the daemon's `ExecStart=` line to have
it self-terminate after N seconds with no active client connections.
Combined with socket activation, this gives "run only when needed"
behavior. Leave it unset for long-running deployments.

### Non-activated fallback

If `LISTEN_FDS` is not set, the daemon falls back to binding
`--socket-path` itself. This is the code path used by:

- Legacy installs (`systemctl --user enable streamlib-broker.service`
  without the `.socket`).
- Non-systemd environments.
- Ad-hoc dev (`scripts/streamlib-broker-dev.sh`, tests).

Stale-socket cleanup applies on this path: the daemon unlinks a leftover
socket file from a previous crash before `bind()`.

---

## Dev flavor: cargo run + standalone helper

For ad-hoc dev work (no `dev-setup.sh` bootstrap needed), use the
standalone lifecycle helper:

```bash
scripts/streamlib-broker-dev.sh start    # cargo build, spawn, probe until ready
scripts/streamlib-broker-dev.sh status   # prints pid / socket
scripts/streamlib-broker-dev.sh probe    # 0 on pong, 1 otherwise
scripts/streamlib-broker-dev.sh stop     # SIGTERM, wait, SIGKILL fallback
scripts/streamlib-broker-dev.sh restart
```

The helper is pid-file-gated (`$STREAMLIB_HOME/broker.pid`) to prevent
double-spawn. By default it uses:

| Env                           | Default                                 |
| ----------------------------- | --------------------------------------- |
| `STREAMLIB_HOME`              | `<repo>/.streamlib`                     |
| `STREAMLIB_BROKER_SOCKET`     | `$STREAMLIB_HOME/broker.sock`           |
| `STREAMLIB_BROKER_PORT`       | `50052`                                 |
| `STREAMLIB_BROKER_LOG`        | `/tmp/streamlib-broker-dev.log`         |

For the full dev-environment bootstrap (proxy scripts, `.env`,
user-service install), use `scripts/dev-setup.sh`.

---

## Test flavor: `streamlib_broker.sh` fixture

Integration tests that need a real daemon (not the in-process
`UnixSocketSurfaceService`) use
[`libs/streamlib/tests/fixtures/streamlib_broker.sh`](../../libs/streamlib/tests/fixtures/streamlib_broker.sh):

```bash
SOCKET=$(./libs/streamlib/tests/fixtures/streamlib_broker.sh start)
./libs/streamlib/tests/fixtures/streamlib_broker.sh probe "$SOCKET"
./libs/streamlib/tests/fixtures/streamlib_broker.sh stop  "$SOCKET"
```

The fixture always uses `target/debug/streamlib-broker` (builds it if
missing) so tests aren't paying `cargo run`'s per-invocation cost.

---

## Probing the broker

The daemon ships with a `--probe` subcommand. It connects to the socket,
sends `{"op":"ping"}`, expects `{"pong":true}`, and exits 0 on success
or 1 on any failure:

```bash
streamlib-broker --probe ~/.streamlib/broker.sock && echo ok
```

This is the same command the fixture and dev helper use. It's safe to
call against a socket-activated broker (the ping itself triggers
activation) and against a running daemon.

---

## Log paths

| Deployment        | Log sink                                          |
| ----------------- | ------------------------------------------------- |
| systemd           | `journalctl --user -u streamlib-broker.service`   |
| `streamlib-broker-dev.sh` | `/tmp/streamlib-broker-dev.log` (or `$STREAMLIB_BROKER_LOG`) |
| Fixture           | alongside the socket: `<socket-dir>/broker.log`   |

---

## Common failures

- **"Failed to bind Unix socket … Address already in use"** — a previous
  daemon is still running. `streamlib-broker-dev.sh status` to check, or
  `ps` / `systemctl --user status`. If the socket file exists but no
  daemon does, the bind-path cleanup (`remove_file` before bind) handles
  it on the next start. Under socket activation this error can't happen
  — the socket is owned by systemd.

- **Client connects but hangs** — with socket activation, this usually
  means the daemon crashed while starting. Check
  `journalctl --user -u streamlib-broker.service` for a stack trace.
  The `.socket` unit will hold queued client connects until a successful
  daemon start, so a crash loop manifests as client-side hangs.

- **"LISTEN_FDS set but invalid" warning** — the env vars were
  inconsistent (wrong pid, wrong count, non-integer). The daemon falls
  back to bind. Check whether systemd actually activated you (set
  `LISTEN_PID`) or whether the env leaked from a grandparent.

- **Tests can't find the broker binary** — they rely on
  `CARGO_BIN_EXE_streamlib-broker`, which requires `cargo test -p
  streamlib-broker`. Running a single-test invocation from elsewhere
  won't trigger the binary build.
