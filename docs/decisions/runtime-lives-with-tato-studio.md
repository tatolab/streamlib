# The runtime runs while Tato Studio runs

Rationale for the `[runtime-lives-with-tato-studio]` entries in `docs/plan/ARCHITECTURE.md`
(§Product "The runtime is required" and "Who starts the runtime", §Media I/O on Apple permission,
§Distribution & versioning: the installer and Tato Studio), decided 2026-11-05 and 2026-11-06.

## Trigger

Read this before making any installer register a service, before making the app install or
register the runtime at launch, before letting Quit leave a runtime the app started running, and
before making a verb start a runtime.

## Decision

- Tato Studio starts `tatolabd` as its own child process, from the runtime unit its bundle carries.
  `tatolabd` already runs in the foreground and never detaches, so it needs no new mode.
- The runtime lives as long as the app, not as long as a window. Closing every window keeps the app
  in the tray with the runtime up, so the CLI, the MCP server and the local API keep working. Quit
  stops the runtime with `SIGTERM` and waits for the shutdown ladder to end it.
- One runtime serves every window. A runtime that already answers on the socket is reused, and Quit
  leaves that one running.
- No installer registers a service on any platform. `curl | sh` on Linux lays files and links; a
  person who wants a service copies the unit the README shows.
- With no runtime, every verb fails and names how to start one; nothing a verb does starts one.

## Rejected alternatives

- **The runtime as an always-on per-user service.** There are no always-on relays to keep
  publishing to, and people expect an app to run when they open it and stop when they quit it.
  A launch agent or a systemd unit the app installs keeps a GPU process running that nobody asked
  for (owner, 2026-11-05).
- **Keeping the systemd user service for `curl | sh` machines.** A second way to host the runtime
  for the rare headless machine, which a person can set up from the README in one copy. Owner,
  2026-11-06: "tell them how via the docs without forcing it via the install".
- **The app running `install.sh --from-unit` on each launch whose build differs.** It existed to
  give the service a path outside the bundle. A child process runs from wherever the app is, so the
  install step, the folder per build for the app's runtime and the wait on the service manager have
  nothing left to do.
- **A runtime per window.** Every window of one app would contend for the GPU and the machine lock;
  a window is a client.

## Consequences

- On macOS the runtime is credited to Tato Studio, its responsible parent. Its prompts name Tato
  Studio, its `Info.plist` carries the usage strings, and a grant given to `Tatolab Runtime` does not
  carry over.
- Something has to start a runtime that exits unasked; the app does, as the service manager did, or
  the crash-loop rule never sees a second crash.
- A runtime the app leaves behind after a crash is reused by the next launch and outlives that
  launch's Quit.
- The installer's test needs no systemd, and the tarball carries no unit file.
