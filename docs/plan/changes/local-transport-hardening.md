# local-transport-hardening

The same-host transport, hardened before the runtime mesh wires onto it live. After this
change:
- iceoryx2 runs in a domain the engine owns, one per OS user.
- A channel takes a late consumer of any delivery profile, and a port fans out to 32 consumers
  plus a tap and in from 256 links.
- A link onto a helper reads `wired` only once the helper has opened its port.
- A helper built from a different engine is refused by name.
- Every app exits on the shutdown ladder.

The change implements the `[shutdown-ladder]` entries in `docs/plan/ARCHITECTURE.md`: §Processor
model `:717-732`, §Networking `:2196-2201` and §Language SDKs `:2501-2512`. Owner align
2026-09-14 (PR #2258). Rationale lives in `docs/decisions/shutdown-ladder.md` (exists) and
`docs/decisions/local-transport-hardening.md` (added by this proposal). The runtime mesh's
egress and ingress will join running channels of any profile and wire and unwire links live
(`:2448-2453`), so this change comes first.

**Scale gate: this skill, plus an ADR.** The change touches three contracts:
- **The IPC wire format:** the helper handshake, a reply to `wire_link`, the parent→child
  wiring envelope, and a new helper environment variable.
- **The processor model:** the ladder.
- **The Python API's public contract:** what `run()` does on repeated signals and what it
  raises, and a `KeyboardInterrupt` delivered inside a callback at shutdown.

**Precondition.** Every entry this change implements is DECIDED: `:717`, `:2196`, `:2501`, and
the late-joiner sentence at `:2573-2574`, and the node registry at `:2625`. Two OPENs are left untouched:
- the windowed-connect OPEN at `:1523`, which belongs to `loss-visibility`;
- the auth OPEN at `:2708`.

The align classified two items as needing no plan text (PR #2258 body): the per-user iceoryx2
domain and the link-cap raise. Both are carried below as scope and rationale, not as plan
entries.

**Verified against the tree 2026-09-14 (HEAD 8c727eb77).** Three read-only recon sweeps
confirmed the audit's citations. The audit (`~/Documents/streamlib-iceoryx2-deep-dive/`) ran
at `117486268`, and every commit since touches other code.

**Domain**
- iceoryx2 configuration is ambient. `Iceoryx2Node::new` calls `NodeBuilder::new()` with no
  `.config()`, `.name()` or signal mode (`runtime/streamlib-engine/src/iceoryx2/node.rs:28-36`).
  iceoryx2 therefore reads `$CWD/config/iceoryx2.toml`, then `~/.config`, then `/etc`, once per
  process.
- There are 67 node-construction sites: 2 in library code (`core/runtime/runtime.rs:199` and
  the wheel's `python_processor_link_data_access.rs:239`) and 65 in tests and benches.
- The tree has no `global_config`, `Node::list` or `try_cleanup_dead_nodes` call, and no
  socket-path budget check.
- Linux refuses to start without `XDG_RUNTIME_DIR` (`runtime.rs:1281-1288`), so CI sets it by
  hand (`.github/workflows/python-wheel.yml:172`, `:304`; `release-extension-wheel.yml:112`).
  The node registry falls back instead, to a temp folder every user shares
  (`runtime/streamlib-api-server/src/node_registry.rs:117-126`).

**Sizing**
- A channel's depth is fixed by its first consumer's profile
  (`core/compiler/compiler_ops/open_iceoryx2_service_op.rs:430-441`).
- Every subscriber's buffer equals the service maximum (`node.rs:125`, `:185`).
- A second profile on one output port is refused (`:537-579`, text `:560-570`).
- The MCP prompts mirror that rule twice: `OutputPortConsumersKept` and
  `delivery_profiles_queueing_deeper_than` (`runtime/streamlib-api-server/src/mcp_prompts.rs:355-450`).
- The wiring envelope's one `max_queued_messages` field is at once the service maximum, the
  mailbox depth and the subscriber buffer in the helper (`python_processor_link_data_access.rs:288-292`,
  `:390-402`).
- Service factories are locals, dropped when the op returns (`open_iceoryx2_service_op.rs:143-152`).
  The doc comment at `:47-48` says the opposite.
- The caps are `MAX_DESTINATIONS_PER_CHANNEL = 8` (+1 tap) and
  `MAX_INBOUND_LINKS_PER_DESTINATION = 8` (`runtime/streamlib-ipc-types/src/lib.rs:261-279`).
- `max_nodes`, borrowed and loaned are left at iceoryx2 defaults (20, 2 and 2).
- `DEFAULT_MAX_QUEUED_MESSAGES` (`:239`) is used only by tests. `next_read_required_len`
  (`:223`) has no callers.

**Wiring**
- `wire_link` is fire-and-forget (`sdk/streamlib-python-wheel/src/python_helper_process_spawn_host.rs:652-671`).
- `Wired` is stamped unconditionally (`open_iceoryx2_service_op.rs:220-225`).
- A child that fails only logs (`_helper.py:746-770`).
- `LinkState::Error` exists but is never stamped (`core/graph/edges/link_state.rs:9-21`).
- The bridge routes every non-escalate frame as a lifecycle reply
  (`core/compiler/compiler_ops/subprocess_bridge.rs:394-439`), so an unsolicited frame would
  desync it. A reply needs its own rpc tag.
- MCP tells agents to confirm `wired` in `graph` (`mcp.rs:259`; `mcp_prompts.rs:614,713,782`).

**Handshake**
- `STREAMLIB_SUBPROCESS_PROTOCOL_VERSION = 2`, `MIN_SUPPORTED_SUBPROCESS_PROTOCOL` and
  `validate_subprocess_protocol` live at `subprocess_bridge.rs:64-102`.
- The Python side, `_assert_the_parent_speaks_this_protocol` (`_helper.py:799-809`), passes
  silently when the variable is absent.
- The tree carries no build id.

**Helper stop**
- Each helper waits 5 s for `stopped`, then 5 s for teardown (`REPLY_DEADLINE`,
  `python_helper_process_spawn_host.rs:58`).
- A timeout sets `child_is_gone`, and user `teardown()` is skipped (`:256`, `:281`).
- Kills target the pid only (`:381`, `:410`), even though `pre_exec` already calls
  `setpgid(0,0)` (`:436`). Nothing calls `killpg`, and there is no `close_range`.
- Removal is serial, one processor fully joined before the next is signalled, with an
  unbounded join (`core/compiler/compiler.rs:268-389`, `:306`).
- Liveness is detected only by bridge EOF (`:591-599`).
- The helper installs no signal handling. A consumed EOF leaves its outer loop blocked forever
  (`_helper.py:548-554`, `:689-704`).
- `SubprocessHandleComponent` is inserted nowhere, and its SIGTERM path at `compiler.rs:320-365`
  is dead.

**App stop**
- Signal ownership covers only SIGINT and SIGTERM (`core/signals.rs:195`), latched rather than
  counted (`core/runtime/runtime_shutdown_request.rs:24`).
- Ownership ends before the engine drop (`python_runtime_lifecycle.rs:407` vs `:421`).
- tokio drops without a timeout (`runtime.rs:146-158`).
- The stdio interceptor joins its readers unbounded, and it `dup`s stdout and stderr and opens
  pipes without CLOEXEC (`core/logging/stdio_interceptor.rs:60-66`, `:166`, `:183`). That is the
  "won't quit even with SIGKILL" cause.
- `run()`'s stub says only "until Ctrl-C, SIGTERM or `shutdown()`" (`_engine.pyi:502-503`).

---

## ADDED: §Processor model — a link onto a helper is wired when the helper says so

Before the helper confirms, a link into or out of a helper-placed processor never reports
`wired`. The helper answers every `wire_link` it receives with a link-scoped `wired` or
`wire_failed` reply, and `wire_failed` carries the reason its open failed. The reply rides its
own rpc tag, which the bridge routes to that link's state, never to the lifecycle reply queue. A
link carried in the startup envelope needs no reply of its own: `ready` confirms it, as it does
today, and a failure there still refuses the processor's start by name. MCP's instruction to
confirm `wired` in `graph` becomes true rather than hopeful. A link an
engine-to-helper wire has not yet confirmed is never re-planned as unadded
(`compiler.rs:151-155`).

**DECIDED (owner, 2026-09-14) — `connect` onto a helper does not wait for the helper's reply.**
`connect` returns with the link `pending`. The helper's reply flips it to `wired`, or to `error`
carrying the helper's reason, and `graph` renders that reason until the link is disconnected.
This is the first use of `LinkState::Error`. The caller learns whether the change took by reading
`graph`, as the MCP instructions already tell it to (`mcp.rs:259`), and those instructions gain
the `error` case. The helper reads commands only between callbacks (`_helper.py:651-652`), so
waiting would put a control-plane call on user code; `:710-711`'s isolation axis settles it
over `:2566-2568`'s "the caller learns whether its change took". Rejected: `connect` waiting,
bounded, for the reply and rolling the link back on `wire_failed`.

## ADDED: §Processor model — the helper handshake checks the engine build

A helper refuses to start unless the engine it imported is the same build as the parent's. The
build id is:
- the crate version;
- the git sha, or `unknown` where the build has no `.git`;
- a nonce minted per build by the engine's build script.

It is compiled into `_engine.abi3.so`. The parent passes its id in the helper's environment. The
helper compares it with its own before it opens any channel or socket, and on a mismatch writes a
refusal naming both ids to raw stderr and exits, so the parent reports the processor's start as
refused, naming the helper's stderr. An absent id is a refusal too, never a silent pass. The
protocol version number, its minimum, its validator and its environment variable are retired: one
check per invariant, and a hand-bumped integer never caught a helper built against a different
iceoryx2 patch or a stale wheel on the helper's `sys.path`.

## MODIFIED: §Packages & extension model `:60-62` — scoped to the plugin ABI

"No load handshake, no build fingerprints" names what the deleted plugin ABI carried: a
dlopen'd cdylib's load handshake. The sentence gains that scope, so it does not read against a
helper's handshake, which imports the one wheel and checks that it is the parent's build. No
dlopen or ABI surface returns.

## MODIFIED: §Control plane & observability `:2625-2628` — the runtime directory always resolves

"The OS's standard per-user runtime directory" becomes one engine-resolved directory for the node
registry, the surface-sharing socket and the iceoryx2 domain. On Linux it is
`$XDG_RUNTIME_DIR/streamlib/` when that variable is set and non-empty, and otherwise — empty or
unset, and on macOS always — `/tmp/streamlib-<uid>/`, created owner-only and checked as a real
directory the uid owns with no group or other bits. The check runs once as the runtime starts,
before its first node, socket or registry write, and a failure refuses the start by name; every
user takes the resolved directory. No StreamLib variable overrides it — a container or CI job
sets `XDG_RUNTIME_DIR` — so a runtime starts anywhere with nothing set, and the Linux refusal
goes. The wheel's Python registry reader (`_node_registry.py:58-61`) resolves identically.
What a runtime keeps — logs, caches — stays under the project's `.streamlib/`
(`core/streamlib_home.rs`); this directory holds only what means nothing once the processes are
gone. Owner, 2026-09-14.

## Shutdown ladder: the readings the tree will build

The plan text at `:717-732` and `:2501-2512` is DECIDED. Each point below reads that text where
an implementer would otherwise choose inline, states the reading, and names where it bites. None
of them adds a mechanism.

1. **The watchdog ends the process.** It arms when any engine teardown starts: `run()`'s,
   `shutdown()`, context-manager exit, `atexit`. On expiry it logs what is still running and
   exits with status 124, distinct from the third interrupt's 130.
   - Bites: an embedding host (Isaac Sim, a notebook) loses its interpreter. The ADR accepts
     that nothing hangs the app.
2. **`run()` raises `RuntimeError` naming each abandoned processor** by display name and id.
   Every other `run()` failure already raises that type, and the CLI already reports it as a
   launch error (`cli.py:258-263`), so the stub gains no class. A forced shutdown that abandoned
   nothing returns normally.
3. **An abandoned native thread keeps the engine alive by leaking it.** The thread holds the
   runner (`runtime.rs:476`), and the engine is deliberately never dropped before process exit.
   This is `:2507`'s "the engine stays alive beneath it", and it means a thread that returns
   late never runs tokio shutdown, the fd restore or device wait-idle on its own thread during
   interpreter finalization. `EngineTornDownAndThreadsJoined` is renamed to say joined or
   abandoned.
4. **An interrupted `setup()` still gets `teardown()`.** `:722` reads literally: any Python
   callback interrupted at shutdown, `setup()` included, is followed by `teardown()`.
   - A `setup()` that raises by itself keeps today's no-teardown rule
     (`spawn_processor_op.rs:377-392`).
   - Bites: a `teardown()` that touches state `setup()` never built raises. That is caught and
     logged like any hook failure (`_helper.py:472-487`).
5. **The second interrupt gives a Python helper no `teardown()`.** It terminates the helper's
   process group, which `:2503-2505` states. A recording keeps what its closed fragments hold,
   which is the case the fragmented layout was chosen for (`:1905-1914`).
6. **A live `remove_processor` whose native thread outlives its budget still removes the
   processor.** The node and its links are removed, the thread is abandoned, and the call fails
   naming it, so the caller learns the change did not end cleanly.
7. **Stdout and stderr readers are detached at helper exit, never closed.** A surviving `setsid`
   descendant's writes are still logged rather than raising SIGPIPE, and each reader thread
   lives while a survivor holds its pipe. This is `:728`'s "stops waiting on those pipes".
8. **Signal ownership stays scoped to `run()`.** Teardowns outside it (`shutdown()` before a
   run, `Drop`, `atexit`, `__exit__`) own no signals; the watchdog alone bounds them.
9. **The ladder is Linux-first.** Process groups, `waitid` and CLOEXEC-at-source compile on
   both platforms, and macOS falls back to closing fds one at a time where `close_range` is
   absent. Escalation and SIGHUP on macOS's `ctrlc` / `NSApplication` path are not built, as
   `run()`'s rustdoc already scopes (`python_runtime_lifecycle.rs:363-378`). Cross-compile
   verified.

Pattern choices stay inside the tickets:
- sub-budget names and values (1 s interrupt, 5 s teardown, term grace, kill wait);
- `waitid(WNOWAIT)` over pidfd;
- where the escalation counter and a lock-free process-group registry live;
- the tokio `shutdown_timeout` and interceptor-join bounds;
- helper-side `set_inheritable(False)` and sticky EOF;
- the iceoryx2 dead-node sweep at helper death, run with the engine's domain config;
- the CLOEXEC gate's banned patterns.

The lifecycle-reply correlation id stays out.

## Carried without plan text: engine-owned domain, sizing and caps

The align classified these as pattern (PR #2258). They are scope here, with rationale in the new
ADR.

**Domain.**
- One engine function builds every node's iceoryx2 configuration from the defaults, never from
  the lookup path, and every node and static call uses it.
  - Prefix `sl{uid}_`.
  - Root `iox2/` inside the runtime directory the MODIFIED entry above resolves.
  - Nodes named `streamlib-runtime/{runtime_id}` and `streamlib-helper/{processor_id}`, which
    are labels, never identities.
  - `SignalHandlingMode::Disabled`, which changes nothing today and guards a future
    `Node::wait` or `WaitSet`.
- Before the first node, the engine refuses by name a root plus prefix past the Unix-socket
  budget: 63 bytes on Linux, 58 on macOS.
- The parent hands the root to a helper in one environment variable. A helper started without it
  refuses by name.
- A test-only override gives each test process its own domain. The 13 pytest callers of
  `ProcessorLinkDataAccess()` and the extension wheels' tests use it.
- All 67 sites migrate in one PR, with a new source-walking gate refusing `NodeBuilder::new()`
  outside `iceoryx2/node.rs`. The gate covers test modules and benches, unlike every existing
  gate, because a partial migration hangs tests silently across two domains.
- CI's hand-set `XDG_RUNTIME_DIR` is deleted from the workflows, so CI runs the fallback arm.

**Sizing.**
- Every data service is created at `ORDERED_DEPTH` (16) through one creation-depth function,
  which `loss-visibility` later raises for a windowed destination (`:1514-1518`). It is never
  fed a windowed mailbox depth.
- Each subscriber's buffer is its own port's depth, and drain order resolves per destination
  port.
- The envelope carries the service's creation depth and the port's depth as two keys.
- The parent holds the data and notify service factories for the channel's life, helper↔helper
  channels included, so whichever helper opens first no longer decides the size.
- The same-profile refusal and both MCP mirrors are deleted. The insert recipe always connects
  the new consumer before disconnecting the link it replaces.
- Borrowed samples 1, loaned samples 1, history 0.
- Cost (audit F.3): a `newest` channel moving from 4 to 16 adds 0.22 MiB of publisher heap and
  0.03 MiB of shared memory per connection.

**Caps.**
- `MAX_DESTINATIONS_PER_CHANNEL` = 32 (33 subscribers with the tap), costing +0.82 MiB per
  publisher and +0.10 MiB per connection.
- `MAX_INBOUND_LINKS_PER_DESTINATION` = 256, at a measured cost of about zero.
- `max_nodes` is twice the cap on both builders, leaving room for dead nodes not yet swept.
- The fan-out refusal drops "fan out through another output port", because a live-added
  processor cannot grow ports.
- Measured in the audit's note 20. Re-homing past 32 stays written down as the path if a real
  graph ever needs it.

## Assumptions stated, not asked

- **A helper that dies with a link unconfirmed** takes the link down with it on the same
  death path the ladder runs; the link never reads `wired`.
- **Bug tickets B1–B7** from the audit (the input-side lock, wake-ups, lock order, helper-line
  fixes, event bus, helper diagnostics, tap hygiene) are not this change. They are bugs against
  shipped behavior and get no change artifact. None is filed yet. B1 and B2 must land before
  the mesh, not before this change.
- **One ADR for the four protocol and sizing items**, `docs/decisions/local-transport-hardening.md`.
  The ladder's rationale stays in `shutdown-ladder.md`.

## Expected slices

`/derive-tickets` decides the breakdown. The shape the recon supports:

| # | Slice | Blocked by | Proof |
|---|---|---|---|
| M1 | One runtime-directory resolver (registry, surface socket, iceoryx2), engine-owned domain, node names, budget refusal, helper env var, test override, 67 sites + gate | — (first, alone) | CI: starts with `XDG_RUNTIME_DIR` unset, its registry entry found through the Python reader and gone after a clean teardown; a fallback folder that is a symlink, another uid's, or mode 0755 or 0770 is refused by name before any node; a CWD `config/iceoryx2.toml` ignored; budget refusal by name; two test processes in disjoint domains; gate green |
| M2 | Creation depth 16, per-port rings, envelope split, held factories, refusal and mirrors deleted, caps 32+tap / 256, `max_nodes`, borrowed/loaned/history | M1 | CI: a `newest` then an `ordered` consumer of one running port both wire; 33 subscribers from 33 nodes; 256 notifiers from distinct nodes |
| M3 | `wired` / `wire_failed` reply on its own rpc tag, link state from it | M2 | CI: a helper that cannot open its port leaves the link not `wired`, with the reason; `test_helper_process.py` arm |
| M4 | Build id in the handshake; protocol version retired | — | CI: a mismatched and an absent id each refused by name before any channel opens |
| S1 | Helper ladder with mid-run death detection, group kill, `close_range`, helper-side interrupt and sticky EOF, dead component deleted | M1 | CI: ladder against stub children; rig `/verify-live`: stuck helpers end in about one ladder, grandchildren never hold stdout past exit + 1 s |
| S2 | App ladder: parallel removal, abandon and name, ownership through the drop, SIGHUP, 1st/2nd/3rd interrupt, watchdog, bounded tokio and interceptor, CLOEXEC at source + gate, `run()` docstring | S1 | Rig: Ctrl-C with `process()` asleep exits in ≤ ~2 s with teardown; a 3rd Ctrl-C exits 130; SIGHUP tears down; the MP4 SIGTERM proof (`:2035`) stays green |

Every new engine test is named in `.github/workflows/test.yml`'s slice and in the
`run_local_ci_gates` mirror (`xtask/src/main.rs:204`), or it runs nowhere. The cap and
same-profile tests in the tree today run nowhere either.

## Records the implementation owes, in the same PRs

These are statements the change makes false; each is a record, not a question:
- `/tmp/iceoryx2` comments at `core/utils/loop_control.rs:155` and `core/pubsub/bus.rs:206`.
- The factory and depth comments at `open_iceoryx2_service_op.rs:47-48`, `:133-135`,
  `:360-363`, `:420-422`, `:526-534`; `node.rs:64-66`, `:96-104`; `tap.rs:15-20`;
  `delivery_profile.rs:48-50`.
- "Unanswered" and "already reports the link wired" at `python_helper_process_spawn_host.rs:645-651`
  and `_helper.py:580`, `:763`.
- "EOF is the only signal" at `python_helper_process_spawn_host.rs:592-597`.
- Stale docstrings at `test_interpreter_lifecycle.py:83-90`, `test_helper_placement.py:267-271`
  and `test_helper_process.py:250-251`.
- `xtask/src/main.rs:120`'s "all eleven".
- `docs/learnings/pubsub-lazy-init-silent-noop.md:47` and
  `startup-crash-iceoryx2-wire-vs-gpu-setup-race.md:74-76`, annotated per the docs policy.
- The `run()` docstring in `_engine.pyi:502-503` and its rustdoc.

## REMOVED

Runtime directory (M1):
- REMOVED: XDG_RUNTIME_DIR is not set
- REMOVED: node registry falling back to the system temp dir
- REMOVED: /tmp/streamlib-runtime-dir

Channel sizing (M2):
- REMOVED: delivery_profiles_queueing_deeper_than
- REMOVED: OutputPortConsumersKept
- REMOVED: conflicting_destination_profile_is_a_configuration_error
- REMOVED: inserting_a_type_that_queues_deeper_than_the_links_channel_is_refused_by_name
- REMOVED: a_new_consumer_reading_another_profile_than_the_port_already_feeds_is_refused_by_name
- REMOVED: DEFAULT_MAX_QUEUED_MESSAGES
- REMOVED: next_read_required_len
  Dead code with zero callers, in the envelope-constants file this change rewrites.

Handshake (M4):
- REMOVED: STREAMLIB_SUBPROCESS_PROTOCOL_VERSION
- REMOVED: MIN_SUPPORTED_SUBPROCESS_PROTOCOL
- REMOVED: validate_subprocess_protocol
- REMOVED: STREAMLIB_PROTOCOL_VERSION
- REMOVED: _assert_the_parent_speaks_this_protocol

Helper ladder (S1):
- REMOVED: SubprocessHandleComponent
- REMOVED: runtime/streamlib-engine/src/core/graph/components/subprocess_handle_component.rs
