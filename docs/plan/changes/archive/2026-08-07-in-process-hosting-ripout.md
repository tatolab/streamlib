# Change: in-process-hosting-ripout

<!-- check-no-in-process-placement:allow-file — this change names the banned model to delete it -->

**The helper-placement pivot's rip-out — delete the in-process hosting Change A shipped.**
Implements `[helper-process-placement-only]` (ADR:
`docs/decisions/helper-process-placement-only.md`, owner 2026-08-04). Executes
**inside #1714** together with the helper spawn path — never in parallel with it
(owner: no coexistence window; the ban on parallel old/new shapes applies to our own
last ship).
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
  old derivation sites die with the module loader), the wiring error at `rt.add`
  naming the fix for every identity a fresh interpreter cannot import —
  `__main__`-defined and function-local (`<locals>` in the qualname) classes alike —
  the identity-stability test, and the registry closure capturing the import path
  instead of `Py<PyAny>`.
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
  staging buffer (the one machinery gap); ~~`CpuAccessGate` wired to the
  `produce_done`/`consume_done` OPAQUE_FD timeline pair~~ — Corrected 2026-08-07 at ship
  (flagged during #1714's second session): that pair is the cpu-readback adapter's
  single-writer contract and never touches this path; what shipped synchronizes on the
  staging's single `refill_done` timeline, which travels in the `produce_done` wire slot
  with `consume_done` empty; `acquire_texture` either
  registers pool textures into surface-share or refuses from a child with a named
  error (implementer's call from transport cost, both acceptable).
- **Child log forwarding as a hard deliverable**: records ride the escalate `Log` op
  into `polyglot_sink`; per-child attribution is a process constant. Scaffold splits
  into `app.py` + the effect class in its own importable module; `release-wheel.yml`'s
  scaffold gate follows.
- **Behavioural placement gate** (with this change, complementing the vocabulary
  xtask): a test proving the parent never hosts the class — the app's own
  registration import is the only parent-side load (`rt.add` and N bags add nothing
  to the parent's `sys.modules`), the bag-carried pid differs from the app's, two
  instances of one class get two pids, and a native built-in reports the app's pid
  (the boundary, discriminated).
- **Single-processor test harness keeps its API, gains a real transport** (owner,
  2026-08-05): `SingleProcessorTestPipeline`'s feeder/collector become real graph
  endpoints over a parent-owned IPC channel, replacing the module-global queues that
  only a shared interpreter could reach. Tests keep asserting what they assert today,
  against the real placement. An exempted in-process test-only path was rejected — it
  would make the banned shape legal somewhere.
- **Crash policy: surface, keep running** (owner, 2026-08-05): a crashed helper shows
  as `ProcessorState::Error` in `health`, the rest of the pipeline keeps running, its
  in-flight frame is lost — at-most-once, never silently replayed. Bounded
  auto-restart with `setup()` replay is designed post-MVP in its own align. Isolation
  is the promise; self-healing is a feature.

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
- Format vocabulary deduplicates into one shared definition (today
  `python_processor_context.rs` and `subprocess_escalate.rs` carry byte-identical
  copies).
- Tests: the module-global observation queues rewire to the decided harness
  transport; the two `__main__`-declared fixtures move to importable modules;
  `test_the_scaffolded_app_reaches_a_running_graph` becomes the demo gate with
  `MINIMUM_FRAMES_FOR_LIVE_VIDEO` re-baselined against real IPC cost.
- `docs/plan/changes/one-monotonic-clock.md`'s MODIFIED bullet targeting
  `spikes/streamlib-pyembed-spike/src/monotonic_clock.rs` retires with the spike tree
  (below) — one line, must not be forgotten.
- **Engine-side in-process leftovers get named dispositions** (merged-PR audit,
  2026-08-05 — the only three engine-side items the in-process era left behind; nothing
  cross-process was removed, weakened, or bypassed by #1718/#1720/#1732/#1733/#1737):
  `emit_python_processor_log_record` (`core/logging/mod.rs`, from #1720) dies with its
  sole wheel caller unless the helper log protocol explicitly reuses it — either way,
  named, never silently kept; the `spawn_processor_op.rs` comment justifying the
  "Python processor" label with "runs in the app's own interpreter" is corrected (the
  label may stay); `device_export_staging`'s module-doc obligations to #1714 —
  register each staging + exportable timeline with surface-share, and provide a
  helper-reachable refill/copy-back trigger (the `GpuContextLimitedAccess`
  passthroughs are host-direct by design today) — are promoted from doc comment to
  checklist items.
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
- ~~REMOVED: set_iceoryx2_resources~~ — Reassigned 2026-08-07 at ship: live plugin-ABI
  surface (the #894 `GeneratedProcessor` seam); every carrier (`processor_vtable.rs`,
  `core/plugin/`, the generated-processor pair, the subprocess spawn ops) is on Change B's
  REMOVED inventory, and it dies there with them.
- REMOVED: test_in_process_authoring
- REMOVED: test_a_write_blocked_by_backpressure_stalls_no_other_python_processor
- ~~REMOVED: STREAMLIB_PYTHON_NATIVE_LIB~~ — Reassigned 2026-08-07 at ship: every
  remaining carrier (`native_lib_resolver`, `spawn_python_native_subprocess_op`, the
  module loader, old `sdk/streamlib-python`) is Change B's deletion scope; the env var
  dies with the machinery that reads it.
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
supersession spans with an odd-count refusal, and carries exactly two allow-file
pragmas — this change and the ADR, the two documents that must quote the banned
vocabulary to delete and ban it (each also carries the numbers' retraction; the third
quoting file, `importable-python-library.md`, is covered by its supersession spans).
Residual risk stated plainly: git history and GitHub edit-history retain the originals;
a paraphrase trips nothing — the plan entry, the glossary, and the memory memo are the
defence there.

## Out of scope

- The operating-model enforcement half (`.claude/rules/placement.md`, reviewer
  hard-fail criteria, the xtask gate implementation) — its own PR per
  `.claude/rules/flow.md`, commissioned by the pivot, rule text owner-approved via
  `/propose-rule`.
- Ticket-body amendments and re-sequencing (#1711, #1714, #1730, #1713 split, #1712
  edge drop, #1554 re-milestone) — `/reconcile-tracker` batch.
- Everything Change B already owns that this change does not amend.
