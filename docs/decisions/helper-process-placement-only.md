# Helper-process placement only

<!-- check-no-in-process-placement:allow-file — this ADR must name the banned model to ban it -->

Rationale for the `[helper-process-placement-only]` entries in `docs/plan/ARCHITECTURE.md`,
decided by the owner 2026-08-04, direction confirmed verbatim the same day. Supersedes the
placement clauses of `importable-python-library.md`, the `__main__` bullet of
`schema-free-ports.md`, and passages of `product-mvp-sentence.md` and
`single-binary-launch.md` (all annotated in place). The distribution decision — one wheel,
one venv, PyPI — is untouched.

## Trigger

Read this before hosting a Python processor in the app's interpreter, before building any
GIL-contention / GIL-hold / slow-callback diagnostic, before citing the #1702 spike's
in-process latency numbers, before describing the runtime as "one process", or when someone
asks why every Python processor is a child process.

## The direction, verbatim (owner-confirmed)

1. Helper-process placement is the only execution placement StreamLib has: every Python
   processor runs in its own child process, spawned by the Rust engine as an exec of
   `sys.executable` from the app's own venv — never fork, never `multiprocessing`, never a
   pool — with its own interpreter and its own GIL.
2. In-process hosting of a Python processor is banned outright — not a default, not a
   fallback, not an engine policy choice, not a latency optimisation — because the axis
   being optimised is isolation, not latency: no processor may ever block, stall, or degrade
   another, and that dora-style isolation at Holoscan-class performance is why StreamLib
   exists.
3. The ban is scoped to Python processor hosting — the engine's native built-ins stay in the
   app process, `rt.run()` still owns the main thread with the GIL released, and the
   GIL-release contract survives only so the wheel's blocking bindings never stall their own
   child interpreter, never as a co-tenancy remedy.
4. Author-facing code is one-mode and byte-identical — `rt.add(Class)`, `connect`,
   `setup(rt)`, no placement surface, no opt-in — with one new constraint: a processor class
   must live in an importable, side-effect-safe module, so a `__main__`-defined processor is
   a wiring error naming its fix and `streamlib new` scaffolds the processor beside
   `app.py`.
5. Shipped in-process hosting is legacy ripped out inside #1714 with no parallel
   coexistence; #1714 takes the critical path and grows to carry cross-process bags and
   pixel exchange; and the ban is held by a `.claude/rules/placement.md` stop-work rule,
   hard-fail criteria in both reviewers, and `xtask check-no-in-process-placement`.

## Why

**The owner rejected in-process at discovery and it kept coming back through the plan.** The
2026-08-02 SDK-shape pivot demoted placement to "engine policy" on the strength of a
measured latency gap. The demotion was the drift vector: on 2026-08-04 a session working
#1711 built a GIL-contention watchdog whose module doc read "One interpreter runs every
Python processor", refined it across three review rounds, and cited the plan's "both
placements are first-class" line as justification. Two independent reviewers graded its
code quality; neither flagged the model. A plan entry that permits in-process is read as an
instruction to build it.

**Isolation is the product, and it cannot be conditional.** StreamLib hosts arbitrary
ecosystem Python — torch inference, blocking I/O, unaudited pip packages — in `process()`
callbacks. Under a shared interpreter, one misbehaving callback holding the GIL stalls every
other Python processor silently, and a segfault in any C extension takes down every
processor and the engine. The dora-rs model — one process, one interpreter, one GIL per
node — removes both failure classes by construction rather than by authoring discipline.
Holoscan is the performance bar, not the placement precedent: its own
`PyOperator::compute()` acquires the GIL in one shared interpreter
(`python/holoscan/core/operator.cpp`), a coupling it accepts because appliance authors own
and profile every operator — a trust model a pip-installable runtime cannot borrow.

**The measured gap was never valid licence.** The #1702 spike's cited pair (in-process p50
0.085ms vs subprocess 0.161ms at 720p60) was measured with the two arms on different
CPython builds (corrected in commit `32ef370b`; no re-measured table was ever published),
and the in-process arm ran the one topology — three processors, one no-op stage — where
shared-GIL contention cannot appear; the ×2-chained-stage cell in the frozen protocol was
never run. The defensible reading of the same rig points the other way: 0.161ms p50 against
a 16.67ms 60fps frame budget is ~1% of a frame with zero drops, on the real engine spawn
path — two orders of magnitude of headroom for the isolation the product exists to deliver.

**The spawn mechanism follows from ownership.** `rt.run()` parks the app's main thread in
Rust with the GIL released, and the engine's compiler ops decide when processors come and
go, so the engine spawns and supervises children with `std::process::Command` — no GIL,
ever. Fork is excluded (GPU contexts are fork-unsafe; CUDA requires spawn semantics
ecosystem-wide). Identity crosses as `module:Class` plus JSON config, so pickle carries
nothing. The seam is one trait: `DynGeneratedProcessor` already has the local host (dies)
and the subprocess lifecycle proxy (survives, re-scoped) as its two implementations. Data
rides iceoryx2 shared memory opened by the child; control, GPU brokerage, and logs ride the
escalate socketpair; GPU frames cross as OPAQUE_FD / DMA-BUF file descriptors over
SCM_RIGHTS with the `produce_done`/`consume_done` OPAQUE_FD timeline-semaphore pair — the
only robust cross-process sync on the NVIDIA proprietary driver (timeline-as-SYNC_FD is
spec-forbidden).

## Rejected alternatives

- **Both placements, engine-chosen** (the 2026-08-02 position) — two execution models, two
  failure modes, two debugging stories, and a heuristic no user can predict; isolation that
  is conditional is not isolation. The plan text blessing it produced banned artifacts
  within two days of shipping in-process hosting.
- **`multiprocessing` (spawn) / `ProcessPoolExecutor` as the spawner** — parent-side
  machinery (resource tracker, feeder/sentinel threads, atexit joins, global
  `set_start_method`) assumes a Python-owned process tree, but the tree is owned by a Rust
  engine running with the GIL released; pickle-the-callable re-imports the app's module
  graph when a `module:Class` string is strictly smaller; one worker crash marks a pool
  `BrokenProcessPool` — the inverse of the isolation axis; and the resource tracker's shm
  cleanup would fight iceoryx2's shared-memory namespace.
- **PEP 684 per-interpreter GIL / PEP 734 subinterpreters** — a private interpreter and a
  private GIL but not a private process: one shared address space, so a segfault, abort, or
  memory corruption in any C extension takes down every processor and the engine, and the
  kernel can enforce no per-processor memory or scheduling boundary. Also unavailable:
  `concurrent.interpreters` requires CPython 3.14 against our abi3-py310 wheels on pinned
  3.12, and PyO3-based modules explicitly refuse multi-interpreter use.
- **Free-threaded (no-GIL) CPython** — solves GIL contention, not isolation: still one
  address space, one crash domain; no abi3 support, experimental PyO3 support, and the
  owner has restricted the wheel to GIL-enabled builds.
- **`__main__` processors stay legal somewhere** — under one placement a `__main__`-defined
  class has no legal host: a helper importing `__main__` gets its own entry file, not the
  user's. Reversing the 2026-08-03 ruling costs one import line in the scaffold and makes
  identity launch-independent.

## Consequences

- #1720's in-process hosting (the in-process host type, the context-lease machinery, the
  in-process link data plane) is deleted inside #1714 — never run in parallel with the
  helper path. #1714 also absorbs import-path identity derivation (the unbuilt prerequisite
  `STREAMLIB_ENTRYPOINT` consumes), the `rt.add` `__main__` refusal, the scaffold split,
  and the cross-process pixel-exchange re-scope.
- The rip-out change file's REMOVED inventory is re-audited: the subprocess spawn/bridge/
  escalate/iceoryx2/surface-share machinery, the adapter `-helpers` cross-process parity
  tests, `streamlib-consumer-rhi` and its layout gate, and the polyglot log sink are spared
  or re-homed — they are the helper substrate, not plugin-ABI collateral.
  `docs/architecture/subprocess-rhi-parity.md` is rewritten as the helper-process RHI
  parity doc, not deleted.
- One real gap is new work: device-export staging has no escalate op and no surface-share
  registration (the `run_cpu_readback_copy` template applies). Cross-process GPU handoff is
  otherwise built and proven in-tree.
- The single-processor test harness's transport under helper-only placement is an open
  `[NEEDS DECISION]` — its module-global queues assume a shared interpreter.
- The GIL-release contract narrows to protecting each interpreter's own threads; the
  #1702 spike's numbers are retracted as placement evidence (annotated at the source).
- Enforcement lands as its own operating-model PR per `.claude/rules/flow.md`:
  `.claude/rules/placement.md` (stop-work shapes and the app-process boundary), hard-fail
  insertions in `review-pr` and `rust-craftsmanship-reviewer`, and the
  `check-no-in-process-placement` xtask gate (vocabulary now; the behavioural
  no-hosting-in-the-parent test lands with #1714 — the app's own registration import
  is parent-side by construction; hosting is what never is). "In-process" is retired as
  a word for the surviving app-process senses — the glossary now says **app-process** —
  so the banned term stays unambiguous and greppable.
