# Placement

One Python processor, one helper process, one GIL. There is no second placement.

- **Every Python processor runs in its own child process**, spawned by the Rust engine as an
  exec of `sys.executable` — never fork (GPU contexts are fork-unsafe), never
  `multiprocessing` or a worker pool — with its own interpreter and its own GIL. A processor
  must never be able to block another. This is the library's reason to exist; it is not an
  engine policy call, a heuristic, or a performance tradeoff.
- **Hosting a Python processor in the app's interpreter is banned.** Not deprioritised, not a
  fallback, not a fast path. Owner ruling, 2026-08-04 — reaffirmed from discovery. Plan:
  `docs/plan/ARCHITECTURE.md` §Processor model & scheduling; ADR:
  `docs/decisions/helper-process-placement-only.md`.

## STOP-WORK shapes

Encountering any of these — in a diff, a design, a doc, a ticket, or your own next step —
stops the work. Do not build it, do not measure it, do not diagnose with it, do not grade its
quality. Say what it is and take it to the owner.

- Running or registering a user processor class in the app's own interpreter.
- A GIL-contention, GIL-hold, slow-callback, or stall-attribution watchdog; any per-callback
  duration monitor whose warning names another processor.
- Any diagnostic, metric, log line, or design whose premise is that processors share a GIL,
  share an interpreter, or contend for one.
- Prose calling the runtime "one process", "one interpreter", or "one big process"; calling
  in-process placement "lowest latency"; or naming "both placements" / "either placement" /
  "placement heuristics" as live design.
- A capability offered to Python only by co-hosting it — shortening an escalate hop by moving
  the class into the app process.
- Citing the #1702 spike's in-process latency numbers as placement evidence — they are
  retracted (see the ADR).

## The boundary — these are NOT the ban

Do not flag them, and do not let a gate's false positive become a redesign. The glossary word
for these is **app-process**:

- **Native built-ins in the app process.** Rust camera, display, audio, codecs, kernels run
  in the app process by design. Their per-frame path never enters an interpreter.
- **`rt.run()` blocking the app's main thread with the GIL released**, and every other
  GIL-release-contract mention in the wheel (`allow_threads`, "with no GIL attached",
  "detached from the GIL"). That contract is about the *app's own* interpreter and one
  engine — and, in a child, the helper's own interpreter — and it survives the ban intact.
- **In-process Rust**: the engine and control plane in the app process, in-process adapter
  fast paths, `IsolationTier` minting an in-process `FullAccess` context. Different concern,
  same two words.

## Enforcement

- `cargo xtask check-no-in-process-placement` gates the vocabulary once its ticket lands
  (commissioned by the 2026-08-04 pivot; a behavioural test — beyond the app's own
  registration import, `rt.add` and running bags add nothing to the parent's
  `sys.modules`, and the processor reports a child pid — lands with #1714). Until the code that hosts in-process is
  deleted by #1714, the current tree is transitional debt, never evidence of intent — never
  read the tree as the model.
- `review-pr` and `rust-craftsmanship-reviewer` return `blocker` on any STOP-WORK shape.
  Vocabulary is not the invariant; a violation that uses none of these words is still a
  violation.
