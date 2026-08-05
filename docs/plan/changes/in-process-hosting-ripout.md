# Change: in-process-hosting-ripout

<!-- check-no-in-process-placement:allow-file — this change names the banned model to delete it -->

**The helper-placement pivot's rip-out — delete the in-process hosting Change A shipped.**
Implements `[helper-process-placement-only]` (ADR:
`docs/decisions/helper-process-placement-only.md`, owner 2026-08-04). Executes **inside
#1714** together with the helper spawn path — never in parallel with it (owner: no
coexistence window; the ban on parallel old/new shapes applies to our own last ship).
Also amends Change B (`importable-python-library-ripout.md`) where its REMOVED
inventory, written when in-process was the destination, would delete helper substrate.

Scale tier: touches the processor model and the Python API's public contract → change
artifact + ADR (the ADR exists). Recon: eight read-only agent sweeps 2026-08-04/05
(mechanism panel ×3: Python spawn mechanisms, ownership seam, prior art + GPU handoff;
inventory ×5: docs, code, tickets, operating model, deletion archaeology) — findings
inlined; no API invented here is unverified against the tree.

## ADDED

- **Engine-owned helper spawn in the wheel.** A spawn host on the
  `DynGeneratedProcessor` seam, modeled on `PythonNativeSubprocessHostProcessor`
  (no mailboxes, no writer, Manual on the Rust side, port wiring as JSON): exec of
  `sys.executable` captured at `Runtime` construction — never fork, never
  `multiprocessing`; `PYTHONPATH` carries the app entry directory to the child;
  child-side `PR_SET_PDEATHSIG` + `getppid` recheck (orphan cleanup on parent
  SIGKILL); `setpgid` so terminal Ctrl-C reaches children only via the parent's
  teardown RPC; registration-deadline kill on the existing bridge handshake; child
  crash surfaces as `ProcessorState::Error` + `health` (today `subprocess_dead`
  silently no-ops); `rt.run()` teardown reaps every child (the forward lock
  `test_no_survivors_are_left_in_the_process_group` runs with a real graph).
- **Import-path identity, folded from `processor-class-identity.md` (owner
  2026-08-04):** `module:qualname` derivation from the class object (new code — both
  old derivation sites die with the module loader), the `__main__` wiring error at
  `rt.add` naming the fix, the identity-stability test, and the registry closure
  capturing the import path instead of `Py<PyAny>`.
- **Child-side runtime loop in the wheel** (`python -m` entry): the
  `subprocess_runner.py` structure (handshake → escalate channel → reader thread →
  log install → import class → lifecycle loop) rewritten against the wheel's API;
  children import the wheel itself; PyO3-native iceoryx2 bindings replace the ctypes
  `slpn_*` shim; `_processor_hosting.py` construction moves child-side; an
  `apply_config` bridge verb (none exists).
- **Cross-process pixel exchange** (re-adds what #1710's in-process port dropped):
  the placement-agnostic exchange strata (tensor layouts, DLPack capsules,
  `CpuAccessGate`) hoisted to a crate the child shares; CPU path swaps
  `plane_base_address` for `ConsumerVulkanBuffer::mapped_ptr` (live precedent:
  `examples/camera-python-subprocess`); three new escalate ops for device-export
  staging on the `run_cpu_readback_copy` template + surface-share registration of the
  staging buffer (the one machinery gap); `CpuAccessGate` wired to the
  `produce_done`/`consume_done` OPAQUE_FD timeline pair; `acquire_texture` either
  registers pool textures into surface-share or refuses from a child with a named
  error (implementer's call from transport cost, both acceptable).
- **Child log forwarding as a hard deliverable**: records ride the escalate `Log` op
  into `polyglot_sink`; per-child attribution is a process constant. Scaffold splits
  into `app.py` + the effect class in its own importable module; `release-wheel.yml`'s
  scaffold gate follows.
- **Behavioural placement gate** (with this change, complementing the vocabulary
  xtask): a test proving the parent never imports the user's module
  (`sys.modules` clean after `rt.add` and N bags), the bag-carried pid differs from
  the app's, two instances of one class get two pids, and a native built-in reports
  the app's pid (the boundary, discriminated).
- `[NEEDS DECISION]` **Single-processor test harness transport.**
  `SingleProcessorTestPipeline` passes bags through module-global queues — valid only
  under a shared interpreter, and it is how built-ins are tested. Options:
  (a) keep the API, feeder/collector become real graph endpoints over a parent-owned
  IPC channel — recommended: tests keep asserting what they assert today against the
  real placement; (b) an explicitly exempted in-process test-only path — rejected by
  recommendation: it makes the banned shape legal somewhere.
- `[NEEDS DECISION]` **Restart policy for a crashed helper.** Recommendation: MVP
  ships crash-surfacing only (`Error` state + `health`, pipeline keeps running,
  at-most-once delivery — a crashed helper's in-flight frame is lost, never silently
  replayed); bounded auto-restart with `setup()` replay is designed post-MVP in its
  own align. Isolation is the promise; self-healing is a feature.

## MODIFIED

- **Change B spared-from-ripout amendments** (its REMOVED list predates the ruling):
  the adapter `-cuda-helpers` OPAQUE_FD round-trip tests and the `-vulkan`/
  `-cpu-readback` `consumer_carve_out.rs` tests re-home (they are the only end-to-end
  cross-process GPU proofs); `adapter-abi`'s `subprocess_crash` module re-homes;
  `check-consumer-rhi-repr` is kept, rationale restored (the consumer RHI is the
  child's import surface); `docs/architecture/subprocess-rhi-parity.md` is rewritten
  as helper-process RHI parity, not deleted; the `core/plugin/` iceoryx2 log pair may
  die — its verify-before-delete resolves affirmatively, the successor is the
  surviving `core/logging/{polyglot_sink,stdio_interceptor,iceoryx2_log_bridge}`;
  `sdk/streamlib-python-native`'s ABI dep edges are cut before Change B's crate
  deletions fire (the port needs its source); the child-side Python SDK files beyond
  Change B's four listed deletions are port-source, not deletion scope.
- The engine host-services strip threads the needle Change B leaves ambiguous: the
  escalate GPU ops' dispatch path through `host_services` survives; only the cdylib
  branch dies.
- The wheel's `Cargo.toml` gains the consumer-side deps (`streamlib-consumer-rhi`,
  `streamlib-surface-client`, adapter cores) from doomed `streamlib-python-native`;
  the CUDA device binding converges on the wheel's strict UUID-refusal (the cdylib's
  ordinal-0 fallback is silent corruption on multi-GPU).
- The subprocess protocol-version handshake simplifies to an assertion (one artifact,
  but a stale child on `PATH` is still possible); bump the protocol version once.
- Format vocabulary deduplicates (`python_processor_context.rs` and
  `subprocess_escalate.rs` carry byte-identical copies).
- Tests: the module-global observation queues rewire to the decided harness
  transport; the two `__main__`-declared fixtures move to importable modules;
  `test_the_scaffolded_app_reaches_a_running_graph` becomes the demo gate with
  `MINIMUM_FRAMES_FOR_LIVE_VIDEO` re-baselined against real IPC cost.
- `docs/plan/changes/one-monotonic-clock.md`'s MODIFIED bullet targeting
  `spikes/streamlib-pyembed-spike/src/monotonic_clock.rs` retires with the spike tree
  (below) — one line, must not be forgotten.
- 13 doc comments premised on a shared GIL are purged in the edits touching their
  methods (`python_processor_host.rs`, `python_processor_link_data_access.rs`,
  `python_runtime_lifecycle.rs`, `python_logging.rs`, `python_processor_context.rs`
  ×3, `python_processor_declaration.rs`, `testing.py`, `__init__.py`, both wheel
  `Cargo.toml` descriptions, two test docstrings). The in-process **Rust** carve-out
  (adapters, plugin-sdk, vulkan-jpeg, macros) is untouched.

## REMOVED

Bare patterns — the ship gate greps each line verbatim as a fixed string.

- REMOVED: PythonProcessorHost
- REMOVED: python_processor_host
- REMOVED: LifecycleHookLeaseGuard
- REMOVED: FullAccessRuntimeContextViewPointer
- REMOVED: LimitedAccessRuntimeContextViewPointer
- REMOVED: install_view_lease_and_prime_caches
- REMOVED: revoke_view_lease
- REMOVED: set_iceoryx2_resources
- REMOVED: test_in_process_authoring
- REMOVED: test_a_write_blocked_by_backpressure_stalls_no_other_python_processor
- REMOVED: STREAMLIB_PYTHON_NATIVE_LIB
- REMOVED: streamlib-pyembed-spike

The spike deletion is one clause of a five-part disposition (miscitation research,
2026-08-05; invariant: *every search that surfaces the retracted numbers returns a
retraction, no search returns a claim*): (1) delete the tree — its README is the last
tree location where a grep returns "in-process beats the baseline" as a claim, its
subprocess arm drives machinery Change B deletes (it will not compile post-ripout),
and its published tables were never auditable (`/artifacts` was gitignored; the
0.085/0.161 pair ran different CPython builds per `32ef370b`, never re-measured); the
warm-restart exec-to-first-frame metric definition is lifted into #1714's body first;
(2) the deletion commit message carries the literals `0.085ms 0.161ms 0.089ms 0.180ms`
beside the retraction so `git log --grep` returns the retraction newest-first; (3) the
two GitHub surfaces (issue #1702 verdict comment, PR #1704 body) get a RETRACTED
banner prepended in place — including the un-numbered "0.6% of a frame budget"
sentence no pattern list catches — never deleted; (4) the owner-memory spike memo is
rewritten to carry the retraction (done with this change); (5) the
`check-no-in-process-placement` gate bans the ms-anchored literals and
`both placements viable` (never bare `both placements`), scans `.md`, skips `~~…~~`
supersession spans with an odd-count refusal, and carries exactly three allow-file
pragmas, all on retractions. Residual risk stated plainly: git history and GitHub
edit-history retain the originals; a paraphrase trips nothing — the plan entry, the
glossary, and the memory memo are the defence there.

## Out of scope

- The operating-model enforcement half (`.claude/rules/placement.md`, reviewer
  hard-fail criteria, the xtask gate implementation) — its own PR per
  `.claude/rules/flow.md`, commissioned by the pivot, rule text owner-approved via
  `/propose-rule`.
- Ticket-body amendments and re-sequencing (#1711, #1714, #1730, #1713 split, #1712
  edge drop, #1554 re-milestone) — `/reconcile-tracker` batch.
- Everything Change B already owns that this change does not amend.
