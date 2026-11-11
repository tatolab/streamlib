# runtime-lives-with-tato-studio

> **Asked for by the owner, 2026-11-05:** "I intend to move away from the service based approach
> since we don't have always on relays and instead just have it where we launch the app once and
> when people click it again in their apps folder it brings it back up and when they close with
> the x it keeps it on the system tray and from their they can quit it. I don't want a full
> background service anymore." On 2026-11-06, for a Linux machine without the app: "if someone
> really wants to run it as a service couldn't I just tell them how via the docs without forcing
> it via the install?"

Today the runtime is a per-user service: `install.sh` registers a systemd user unit on Linux and a
launch agent on macOS, and the desktop app runs `install.sh --from-unit` on every launch whose
build differs, then starts and stops that service from its "Run Tatolab" switch. Quitting the app
leaves the runtime running.

**After this change:**
- **Tato Studio hosts the runtime.** Opening it starts `tatolabd` from the runtime unit its bundle
  carries, as its own child process. Closing every window keeps the app and the runtime in the
  tray. Quit stops the runtime with `SIGTERM` and waits for it to exit.
- **One runtime serves every window.** Tato Studio is multi-window, like VS Code; each window is a
  local API client of the one runtime, as the CLI and the MCP server are.
- **A runtime already up is reused.** If something answers on the local API socket when the app
  opens (a `tatolabd` in a terminal, or a person's own service), the app connects to it and Quit
  leaves it running.
- **No installer registers a service, on any platform.** `curl | sh` on Linux lays the build and the
  CLI link and starts nothing. The README shows a systemd user unit a person can copy.
- **With the app quit, every verb fails** as it does today with no runtime, and the refusal names
  both ways to start one: open Tato Studio, or run `tatolabd` in a terminal.

**Scale gate — this skill plus an ADR** (`docs/decisions/runtime-lives-with-tato-studio.md`): it
reverses who hosts the runtime and what the installer does, two DECIDED entries in §Product and
two in §Distribution.

## Current state (recon, 2026-11-06, `main` `3156ed5`)

- **`install.sh` is service-shaped end to end.** `installer/install.sh` (592 lines) has the
  `--from-unit` flag and a `Darwin` arm that writes `~/Library/LaunchAgents/com.tatolab.tatolabd.plist`
  and drives `launchctl bootout` and `bootstrap` (`:200-330`). On Linux it copies
  `lib/systemd/user/tatolabd.service` from the unit into `~/.config/systemd/user/`, stops and starts
  it, waits until the runtime answers or `systemctl` reports it exited, rolls back on an exit, and
  offers `loginctl enable-linger`.
- **The unit carries the service file.** `scripts/build_linux_runtime_release_tarball.sh` copies
  `installer/tatolabd.service` into the tarball's `lib/systemd/user/`, and `install.sh` refuses a
  unit without it (`required_members_of_a_unit`).
- **Its test runs under a user systemd.** `installer/tests/run_install_sh_test_in_a_systemd_container.sh`
  boots systemd in a container and runs
  `installer/tests/install_sh_against_a_fake_release_under_a_user_systemd.sh`, which asserts the
  unit was started, rolled back and pruned, including the `--from-unit` cases (`:298-340`).
  `.github/workflows/installer.yml` runs it.
- **The refusal names one way to start a runtime.** `no_runtime_answers_refusal` in
  `runtime/tatolab-cli/src/machine_runtime_local_api_socket.rs:92-108` says "Start one by running
  `tatolabd` in a terminal."
- **`SIGTERM` already stops everything.** `runtime/streamlib-engine/src/core/signals.rs:47` owns
  SIGINT, SIGTERM and SIGHUP on Linux, and `:485-510` SIGTERM on macOS, through the shutdown
  ladder. Every helper gets its own process group and, on Linux, `PR_SET_PDEATHSIG`
  (`core/child_process_lifetime.rs:17-41`). The tatolabd tests stop a runtime with `SIGINT`
  (`runtime/tatolabd/tests/tatolabd_hosts_a_stream_on_the_gpu.rs:61`,
  `tatolabd_hosts_the_machines_streams.rs:59`); no test stops one with `SIGTERM`.
- **The app is a separate repository.** Spawning the child, the tray, the CLI link and restarting a
  runtime that exits unasked are `tatolab/tatolab-app`'s; this repository states the contract.

## ADDED

- **ADDED (§Product, "Who starts the runtime", DECIDED):** Tato Studio starts `tatolabd` as its
  child from the unit it carries, reuses a runtime that already answers, keeps it through closed
  windows, stops one it started on Quit with `SIGTERM`, and starts it again when it exits unasked.
- **ADDED (test, `tests/stream-on-runtime/test_helper_shutdown_ladder.py`):** a `SIGTERM` to a
  `tatolabd` hosting two Python nodes runs both helpers' `teardown()`, leaves no helper process
  behind, removes the socket and exits zero. This is the proof the app's Quit rests on.
- **ADDED (README, §Install):** "Keep the runtime running as a service", with a systemd user unit to
  copy, pointing at `~/.local/share/tatolab/current/bin/tatolabd`.

## MODIFIED

- **MODIFIED (§Product, the MVP sentence and "The runtime is required"):** the runtime is running
  while Tato Studio runs, or through `curl | sh` on Linux; no installer registers a service.
- **MODIFIED (§Distribution, the installer):** `install.sh` is `curl | sh` on Linux only. It checks
  the tarball, installs the folder per build, points `current` at it, links `tatolab` and `tatolabd`
  into `~/.local/bin`, keeps the previous build and prunes older ones, never one a running process
  runs an executable from. It starts nothing and waits on nothing; when a runtime is running another
  build it says the new one takes effect at its next start. `--uninstall` refuses while a runtime
  runs from an installed build and otherwise removes the files, never the state directory. A
  systemd user unit an earlier install registered is named and left to its owner.
- **MODIFIED (§Distribution, the app):** Tato Studio, a tray app, never runs `install.sh`, has no
  "Run Tatolab" switch, and links `tatolab` into `~/.local/bin` at the CLI its bundle carries.
- **MODIFIED (§Media I/O, permission on Apple):** macOS credits Tato Studio, the runtime's parent,
  so Tato Studio's `Info.plist` carries the usage strings. A device prompt comes up in a project's
  studio window, never at launch.
- **MODIFIED (`runtime/tatolab-cli/src/machine_runtime_local_api_socket.rs`):** the refusal reads
  "no runtime is running on this machine: nothing answers at <socket>. Start one by opening Tato
  Studio, or by running `tatolabd` in a terminal."
- **MODIFIED (`installer/tests/`):** the installer's test needs no systemd and no root. It runs
  `install.sh` against a fake release for a fresh home, asserting the build folders, `current`, the
  links, the pruning around a running runtime and `--uninstall`.

## REMOVED

- REMOVED: --from-unit
  `install.sh`'s flag and its tests; the app no longer installs anything.
- REMOVED: TATOLAB_LAUNCH_AGENT_PLIST
  the macOS arm of `install.sh`: the plist, `launchctl`, and the helper-bundle unit layout.
- REMOVED: com.tatolab.tatolabd
  the launch agent's label.
- REMOVED: launchctl
- REMOVED: TATOLAB_SYSTEMD_USER_UNIT_DIRECTORY
  `install.sh` writing, stopping and starting the systemd user unit.
- REMOVED: offer_to_keep_the_runtime_running_without_a_login
  the installer's offer to keep the runtime up without a login; the README names
  `loginctl enable-linger` beside the unit a person copies.
- REMOVED: lib/systemd/user
  the unit the tarball carried for `install.sh` to copy.
- REMOVED: installer/tatolabd.service
  the README carries the unit a person copies; nothing ships one.
- REMOVED: wait_until_the_runtime_answers_or_its_service_exits
  the wait, and with it the rollback that rested on it.
- REMOVED: installer/tests/install_sh_against_a_fake_release_under_a_user_systemd.sh
- REMOVED: installer/tests/run_install_sh_test_in_a_systemd_container.sh

## Stated assumptions (the owner corrects any of these)

1. **Tato Studio never runs `install.sh`.** It runs `tatolabd` from its bundle, which the issue's
   assumption 3 states, and lays the `tatolab` link itself. Where it bites: an AppImage mounts at a
   new path each run, so a link into it dangles while the app is quit. Verbs that need a runtime fail
   then anyway; `tatolab new` and `tatolab --version` would not run. That is the app's to solve
   (copy the CLI out, or prefer the `.deb`).
2. **macOS grants move to Tato Studio.** A grant someone gave `Tatolab Runtime` does not carry over;
   they are asked again, in a studio window.
3. **The app restarts a runtime that exits unasked.** The crash-loop rule ("a stream implicated in
   the runtime's last two crashes in a row is failed") needs something to start the runtime again,
   which the service manager did.
4. **A runtime orphaned by an app crash is reused, not adopted.** After a crash, `tatolabd` keeps
   running, the next launch finds it answering and connects, and that launch's Quit leaves it up.
   Making the app adopt the runtime it started before is the app's call, and needs nothing here.
5. **The one-runtime-per-machine lock is unchanged.** Nothing in this change needs it to move.

## Tickets

One tracer bullet, in this repository: the installer and its test without a service, the refusal's
words, the `SIGTERM` proof and the README section. The app's side is tracked in
`tatolab/tatolab-app`.
