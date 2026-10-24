# machine-lock-without-administrator

> **Approved by the owner, 2026-10-24**, with Decided 1 as recommended (A).

Built from #2927. On macOS the machine lock is an `fcntl` lock on a root-owned 0666 file in
root-owned `/Library/Application Support/Tatolab/`. Root has to create that file, so
Tatolab.app's first run asks for an administrator password in a dialog that names "osascript".
The owner wants first run on a Mac to need no administrator password, as with OrbStack, Ollama
and Docker Desktop since 4.18 (owner, 2026-10-24, from tatolab/tatolab-app#3).

**After this change:**
- **The rule is unchanged.** There is one runtime per machine. A second `tatolabd`, from any
  user, is refused at start, and the refusal names the holder.
- **The macOS lock is the loopback port.** It is the fixed port the runtime already listens
  on, `127.0.0.1:4446`, which `tatolabd` binds itself at start. Nothing is created by root
  beforehand.
- **The weaker guarantee is accepted (owner, 2026-10-24).** Any process, not only a runtime,
  can take 4446 first. A root-owned file guarded against that, and the owner judged it not
  worth a password for a desktop app.
- **Linux is unchanged.** The abstract socket `@tatolab-runtime` already needs no root.
- **Tatolab.app asks for no password at first run.** The app half is tatolab/tatolab-app#3,
  which waits on this change.
- **What a stream author sees:** nothing. No node, port, config key or Python surface changes.

**Scale gate: this skill, no ADR.** The change alters a contract: the macOS lock mechanism,
the loopback listener's routes, and the refusals the CLI prints. It does not touch the RHI, the
IPC wire format, the processor model or the Python API.

**Precondition.** Every entry touched is DECIDED:
- §Product: the machine lock and the state directory (`ARCHITECTURE.md:128-142`);
- §Distribution & versioning: `Tatolab.app` (`:3350-3381`).

§Product is already IN-FLIGHT for runtime-description, which leaves the lock entry unchanged.
This change adds its own name beside it.

**A correction to the issue.** The issue calls 4446 "the local API's web listener". The local
API is the unix socket `<runtime directory>/local-api.sock`. Port 4446 is the **loopback HTTP
listener**, which serves the listing and `/watch` and is a separate server.

## Current state (recon, 2026-10-24, `main` `ec6adf522`)

**The lock module:** `runtime/streamlib-runtime-client-contract/src/machine_runtime_lock.rs`.
- **Public surface:**
  - `MachineRuntimeLock::take()` (`:115-121`);
  - `holder_of_the_machine_runtime_lock()` (`:124-128`);
  - `MachineRuntimeLockHolder {user_name, uid, pid, executable}` (`:30-39`);
  - `MachineRuntimeLockRefusal` (`:57-102`).
- **Where the platform is chosen:** the `cfg` arms of `MachineRuntimeLockLocation` (`:132-176`).
- **The macOS half:**
  - the constants `MACHINE_RUNTIME_LOCK_DIRECTORY_ON_MACOS` and `…_FILE_NAME_ON_MACOS`
    (`:22-26`);
  - `mod lock_location_trust_check` (`:595-778`), compiled on macOS and under test;
  - `mod macos_fcntl_lock` (`:780-1027`), with an in-process `AtomicBool` because an `fcntl`
    lock belongs to the process (`:798-801`). `holder_named_by_pid` (`:982-1027`) reads the uid
    with `proc_pidinfo(PROC_PIDTBSDINFO)` and the executable with `proc_pidpath`.
  - Two refusals exist only for the file: `LockLocationIsMissing`, which names the `sudo`
    commands (`:72-80`, built at `:1346-1349`), and `LockLocationCannotBeTrusted` (`:82-91`).
- **Already present and reusable:** `HeldByAProcessThatDoesNotAnswer {lock}` (`:62-63`), today
  Linux's refusal for a holder that answers no peer credentials, and
  `HeldByAProcessThatCannotBeNamed` (`:64-71`).

**Where the lock is taken and probed**
- **Taken:** in `tatolabd`'s `main` (`runtime/tatolabd/src/main.rs:71-76`), before the state
  directory, the engine and any thread.
- **Probed by the CLI:** in two places. Both call `holder_of_the_machine_runtime_lock()` and
  name the holder only when its uid is not this user's:
  - the no-runtime refusal (`runtime/tatolab-cli/src/machine_runtime_local_api_socket.rs:31-37`,
    `:56-75`);
  - `--version` (`cli_and_runtime_version_flag.rs:44-52`).

**The loopback HTTP listener**
- **The port:** `LOOPBACK_HTTP_LISTENER_DEFAULT_PORT = 4446`
  (`runtime/streamlib-api-server/src/loopback_http_listener.rs:34`).
- **How it binds:** `serve_the_loopback_http_listener(…, port, …)` (`:246`) binds
  `127.0.0.1:<port>` itself with `std::net::TcpListener::bind`, then serves the routes `/` and
  `/watch` behind the loopback-`Host` check (`:104-143`).
- **When it binds:** `tatolabd` serves it inside `run_owning_the_machine_shutdown_signals`
  (`runtime/tatolabd/src/machine_stream_hosting.rs:112-113`). That is after the state
  directory, the engine, the relay instances and the local API, and before kept streams
  re-load.
- **It is optional today.** A held port leaves the runtime running without it, said once
  (`runtime/tatolabd/src/loopback_http_listener_of_this_runtime.rs:16-53`).
- **A test build** binds only `TATOLAB_TEST_LOOPBACK_HTTP_PORT`, and serves nothing when that
  is unset (`:62-76`; `machine_directories_test_root.rs:28-30`).

**Test roots and fixtures that make the macOS lock file**
- **The test root:** `TestMachineRoot::machine_runtime_lock_directory()`
  (`machine_directories_test_root.rs:74-77`).
- **The suites:**
  - `runtime/tatolabd/tests/common/mod.rs:296-310`;
  - `tests/stream-on-runtime/conftest.py:262-273`;
  - `runtime_unit_under_test.py:80-84`.
- **The macOS CI lanes:** they create the root-owned file with `sudo` (`.github/workflows/ci.yml:869-876`,
  `reusable-macos-runtime-unit.yml:170-177`).
- **The README:** its troubleshooting entry names the missing-lock refusal (`README.md:654-656`).
- **A consumer:** `packages/streamlib-webrtc/tests/test_processors.py:243-258` makes the lock
  file too.

**What the kernel will tell an unprivileged prober on macOS**
- **For any pid,** `proc_pidinfo(PROC_PIDT_SHORTBSDINFO)` and `proc_pidpath` name its uid and
  executable. The full `PROC_PIDTBSDINFO` is refused for another user's process, so
  `holder_named_by_pid` reads the short flavor.
- **For a TCP port,** finding which pid holds it means enumerating that process's descriptors
  (`PROC_PIDLISTFDS`). The kernel refuses that for another user's process, which is why `lsof`
  without root lists only your own sockets.
- So the port alone cannot name another user's holder. Something has to say which pid it is.

## Decided 1 — how the macOS holder is named: A (owner, 2026-10-24)

The issue leaves this to design: is the port enough on its own, or does something beside it
name the holder?

- **A — The holder answers its pid on the port, and the kernel names the pid.** *(Chosen.)*
  - **The route:** the loopback listener serves `GET /machine-runtime-lock-holder`, which
    answers `{"pid":<i32>}`.
  - **The prober:** it reads the uid and the executable for that pid through the existing
    `holder_named_by_pid`. Only the pid is self-reported.
  - **What the refusals read:**
    - a connect refused: no holder;
    - a connect accepted, then no well-formed answer within a bounded timeout:
      `HeldByAProcessThatDoesNotAnswer`, naming `127.0.0.1:4446`;
    - a pid that libproc cannot read: `HeldByAProcessThatCannotBeNamed`, as today.
  - **Cost:**
    - **A lying squatter:** one could name someone else's pid. That is the weaker guarantee
      already accepted.
    - **A probe at start-up:** a probe made between the bind in `main` and the listener
      serving reads "does not answer" for that window.
  - **Shape:**
    - the route is served on both platforms, because `streamlib-api-server` is
      platform-agnostic. On Linux it is unused by the prober and harmless;
    - the answer's type, `MachineRuntimeLockHolderPidAnswer`, sits in the contract crate's
      `local_api_wire_contract.rs` beside the other wire types;
    - the probe is a blocking `std::net::TcpStream` `GET` carrying `Host: 127.0.0.1`, inside
      the contract crate, so it adds no new dependency.
- Rejected: **B — The holder answers the whole holder, `{user_name, uid, pid, executable}`.** This is
  simpler: no libproc call on the probe. But every field is self-reported, so a squatter could
  claim to be anyone, and the shape duplicates what the kernel already names from a pid.
- Rejected: **C — The port alone.** The refusal names `127.0.0.1:4446` and no holder. This fails the
  issue's "the refusal names the holder", so it is not recommended.

## MODIFIED: §Product — the machine lock and the state directory (`:128-142`)

The macOS half changes. The Linux half and the state-directory half stay word for word.
Proposed:

> On macOS it is the loopback HTTP listener's own socket, `127.0.0.1:4446`, which `tatolabd`
> binds at start, before the state directory, and the listener later serves. No file is
> involved and nothing is created by root, so on macOS a held port refuses the runtime and
> never leaves it running without the listing. A second `tatolabd`, or any squatter, is refused
> naming its user, pid and executable. On Linux these come from peer credentials. On macOS the
> holder answers its pid on `GET /machine-runtime-lock-holder` and the prober reads the user and
> executable for that pid from libproc. Something on the port that does not answer is refused
> naming the port. Any user may take the one runtime first, and on macOS any process may hold
> the port first: a weaker guarantee than a root-owned file, accepted so that first run needs
> no administrator password (owner, 2026-10-24). The CLI names the holder too.

**Removed from the entry:**
- "an `fcntl` lock on a root-owned 0666 regular file in root-owned `/Library/Application
  Support/Tatolab/`, which the app's first-launch administrator prompt creates";
- "any other type, owner or mode is refused by name, and a missing one is refused naming the
  commands that create it";
- "`F_GETLK`".

## MODIFIED: §Distribution & versioning — `Tatolab.app` (`:3350-3381`)

- **The CLI link:** "`Contents/MacOS/tatolab-cli`, … which `/usr/local/bin/tatolab` links
  to" becomes "… which the `tatolab` command links to".
- **First launch:** "First launch links `tatolab` into `/usr/local/bin` and creates the
  machine lock's directory behind one administrator prompt" becomes:
  > First launch asks for no administrator password. It links `tatolab` into a folder in the
  > user's home directory, and offers a link in `/usr/local/bin`, behind an administrator
  > prompt, as an option.
- **Which folder:** the folder, and how it reaches the terminal's `PATH`, is
  tatolab/tatolab-app#3's design. This entry names the folder when that lands, as a record.

## MODIFIED: the lock, the listener and `tatolabd`

- **The lock module.**
  - On macOS, `MachineRuntimeLock` holds a `std::net::TcpListener` bound to
    `127.0.0.1:LOOPBACK_HTTP_LISTENER_DEFAULT_PORT`.
  - The constant moves into the contract crate, so the lock and the listener read one value.
    `streamlib-api-server` already depends on that crate.
  - `EADDRINUSE` probes the holder and refuses with `HeldByAnotherRuntime` or
    `HeldByAProcessThatDoesNotAnswer`.
  - Any other bind failure is `CouldNotBeTaken`.
  - A bind is per socket, so a second take in the same process fails the same way, and the
    `AtomicBool` goes.
- **Handing the listener over.** `MachineRuntimeLock` hands over a second descriptor of its
  bound listener (`take_the_bound_loopback_http_listener()`, macOS only), and `tatolabd` serves
  the listing on it.
  - The lock keeps its own descriptor until the process exits, so a serve that is skipped,
    fails or stops at shutdown never frees the port.
  - `serve_the_loopback_http_listener` takes an already-bound `std::net::TcpListener` in place
    of a port, and sets it non-blocking itself. That one signature serves both platforms; a
    caller wanting a free port binds port 0.
  - On Linux, `tatolabd` binds 4446 itself, in `loopback_http_listener_of_this_runtime.rs`,
    and a held port still only leaves it without the listing.
- **Test builds.** In a test build on macOS the lock's port is
  `TATOLAB_TEST_LOOPBACK_HTTP_PORT`, and a test build that names none is refused by name at
  the lock. This keeps "a test build never binds the machine's own port", which is code fact,
  not plan text (`loopback_http_listener_of_this_runtime.rs:62-76`).
- **The CLI.** It is unchanged in shape. `holder_of_the_machine_runtime_lock()` keeps its
  signature, so both call sites stay as they are.

## Not modified

- **The Linux lock, its probe and its refusals**, and Linux's optional loopback listener.
- **The state directory**, including `~/Library/Application Support/Tatolab/`. That folder is
  per-user and needs no root, and it is not the lock.
- **The Linux installer entry** (`:3335-3349`).
- **The runtime-description change**, which reads the lock only through `--version`'s existing
  holder call.

## Tests

**Contract crate (both platforms where the code is shared)**
- A wire-text test of `MachineRuntimeLockHolderPidAnswer`, with a round-trip, as
  `local_api_wire_contract.rs` already does for each wire type.
- **Probe outcomes**, each against a stub listener on a free port:
  - a stub answering `{"pid":1}` is named `root` (uid 0), pid 1, with launchd's or init's
    executable. This pins the cross-user naming without a second account: pid 1 is another
    user's process on both platforms;
  - a stub that accepts and never answers gives `HeldByAProcessThatDoesNotAnswer` naming the
    address;
  - a stub answering garbage gives the same;
  - nothing listening gives no holder.

**macOS lock**
- `take()` on a free test port succeeds with nothing on disk.
- A second `take()` in the same process is refused.
- A port held by a plain foreign listener is refused as "does not answer".

**`tatolabd`**
- **On both lanes:** a second test-build `tatolabd` on the same test root and port is refused at
  start, and its stderr names the first's pid and executable. This is
  `runtime/tatolabd/tests/`, extended from the existing second-runtime refusal.
- **On macOS:** a test build with no `TATOLAB_TEST_LOOPBACK_HTTP_PORT` is refused by name.
- **On macOS:** the listing answers on the lock's own port.

**CLI**
- The no-runtime refusal and `--version` still name a holder serving another uid
  (`machine_runtime_local_api_socket.rs:149`, unchanged).

**CI**
- The macOS lanes drop their `sudo` steps and stay green. A green `Build (macOS)` lane is the
  gate.

**A second real user**
- A test that runs a second real user's `tatolabd` is not proposed: no suite starts a process
  as another uid today. The pid-1 probe test pins the naming, and the kernel's bind conflict
  does not depend on uid.
- Owner: say if you want one, using the lanes' passwordless `sudo -u`.

## Tickets

One tracer bullet: #2927. Needs no rig, but it does need the `Build (macOS)` lane.

**Consumer:** `packages/streamlib-webrtc/tests/test_processors.py:243-258` makes the macOS
lock file. Once the macOS lock takes a test port, that suite needs
`TATOLAB_TEST_LOOPBACK_HTTP_PORT`. Per the consumer rule this is filed as backlog at the
consumer, not fixed in the ticket. No `REMOVED` pattern below appears only there.

## REMOVED

- REMOVED: MACHINE_RUNTIME_LOCK_DIRECTORY_ON_MACOS
- REMOVED: MACHINE_RUNTIME_LOCK_FILE_NAME_ON_MACOS
- REMOVED: LockLocationIsMissing
- REMOVED: LockLocationCannotBeTrusted
- REMOVED: lock_location_trust_check
- REMOVED: macos_fcntl_lock
- REMOVED: the_commands_that_create_the_lock_location
- REMOVED: THIS_PROCESS_HOLDS_THE_MACHINE_RUNTIME_LOCK
- REMOVED: fn machine_runtime_lock_directory
  Continuation: this is `TestMachineRoot`'s method. The bare name is not a bullet, because the
  consumer's test spells a local variable the same way and would hold the gate on backlog.
- REMOVED: sudo chmod 0666
  Continuation: the macOS CI lanes' lock-file steps, and the refusal's creation commands.
- REMOVED: the machine runtime lock cannot be taken
  Continuation: the README troubleshooting entry for the missing-lock refusal.
