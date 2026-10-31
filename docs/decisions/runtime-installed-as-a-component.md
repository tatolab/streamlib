# The runtime is installed as a component, never run from inside the app

Rationale for the `[runtime-installed-as-a-component]` entries in `docs/plan/ARCHITECTURE.md`
(§Product "Who starts the runtime", §Media I/O on Apple permission, §Distribution & versioning:
the installer and `Tatolab.app`), decided 2026-10-31.

## Trigger

Read this before pointing a launch agent or a systemd unit at a path inside an app bundle,
before keying an installed runtime by its version alone, before making the app install, update
or restart the runtime any way but by running the installer, and before adding a deadline to a
wait on the service manager or the runtime.

## Decision

- The runtime is a component the installer installs, on both platforms. Each build goes into a
  folder of its own, `<version>-<sha>`, and `current` names the one running. The per-user
  service — a systemd user unit on Linux, a launch agent in `~/Library/LaunchAgents` on macOS —
  names only `current`. Homebrew services keep the same shape: a versioned folder, a stable
  link, a service that names the link.
- One installer, streamlib's `install.sh`, does every install. `curl | sh` downloads a release
  on Linux; `--from-unit` installs a unit it is handed, which is how the app installs the one it
  carries on either platform.
- The app installs its runtime on every launch whose carried build differs from `current`, in
  either direction, and asks nothing: installing an app is the choice to run its runtime,
  because the app and the runtime ship features together. A build is its version and its
  streamlib commit, so a dev build of an unchanged version still counts.
- On macOS the runtime is a helper bundle with its own bundle id and usage strings, so macOS
  credits it for camera, microphone and local network, and its grants follow its signing
  identity and bundle id across build folders.
- An app opened while another copy runs hands it its build; a different build makes the running
  copy quit and open the new one.
- No wait has a deadline. Stopping waits for `launchctl bootout` or `systemctl` to return;
  starting waits until the runtime answers on its local API or the service manager reports the
  job exited. An exit puts the previous build back.

## Rejected alternatives

- **Running `tatolabd` from inside the app bundle through an `SMAppService` agent.** launchd
  keeps the job as Service Management recorded it for the copy that registered it: a second
  copy's registration has been seen to keep launching the first copy's binary, which Apple
  confirmed as a bug and whose only workaround is deleting the other copy and logging out;
  `register()` straight after `unregister()` has been seen to fail; and replacing a bundle
  leaves the process started from it running. Every update then rested on a heuristic: the app
  remembered which app version it last restarted the runtime under, which no dev build of an
  unchanged version and no second copy of the app could trip.
- **Checking the running runtime's build and re-registering until it matches.** It keeps the
  runtime inside the bundle and works around the registration it cannot control, with a bounded
  wait between steps that fails whenever the system is slower than the bound.
- **A `.pkg` installer.** It asks for an administrator password at every update, for a
  per-user runtime that needs none.
- **A system package (`apt`, `rpm`) as the runtime's updater.** A package's maintainer scripts
  run as root and cannot restart each user's service, so the app would still finish every
  update; it would also make the runtime machine-wide where it is per user.
- **The Linux app carrying no runtime.** The app and the runtime are installed apart and drift:
  a newer app against an older runtime fails in ways neither explains.

## Consequences

- `install.sh` gains `--from-unit`, a folder per build, a macOS arm, the wait for an answer and
  the rollback; `curl | sh` stays for Linux machines without the app.
- `Tatolab.app` wraps the runtime unit in a signed helper and drops its `SMAppService` agent;
  the `.deb` and the AppImage carry the Linux runtime unit.
- macOS shows its background-item notice once, when the launch agent is first written: the
  plist does not change between builds.
- A runtime restart forgets queued stream actions, as it always has; an install in the middle
  of a project's first sync loses that action, and the project's recorded streams come back at
  start.
- Opening an older app installs its older runtime. Records a newer runtime wrote are the older
  runtime's to refuse by name.
