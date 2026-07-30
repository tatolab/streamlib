# Execution model: blessed as built, flavors intended but undesigned

Rationale for the `[execution-model]` entries in `docs/plan/ARCHITECTURE.md`
§Processor model & scheduling, decided 2026-07-29.

## Trigger

Read this before adding execution or scheduling options, proposing a thread pool or
async runtime for processors, or treating the unreferenced execution-flavor components
as dead code to delete.

## Decision

The implemented model is the decided model: three execution modes (reactive / manual /
continuous); one dedicated OS thread per processor; thread priority driven by the
processor's registered descriptor (realtime / high / normal), never by name heuristics;
synchronous lifecycle traits (no host async runtime — a processor needing async builds
its own in setup); and the Full/Limited capability typestate on the phase axis
(setup/teardown privileged, process limited).

Additional execution flavors — green-thread-style lightweight scheduling and similar —
are intended so one node can scale to many more processors than one OS thread each
allows, trading some realtime guarantees; dedicated threads remain the path for
realtime processors. The design is unstarted, so the plan records this OPEN with
direction: do not build until designed, and the design must not add configuration
dials — flavor selection arrives via defaults and derivation, not new knobs in
processor declarations.

## Rejected alternatives

- **Blessing flavors as DECIDED now** — commits to a design that doesn't exist.
- **Dropping the flavor intent from the plan** — loses a settled owner intent and
  invites deleting the component-layer groundwork as cleanup.
- **Name-heuristic thread priority** — already replaced by descriptor-driven priority;
  recorded here so it doesn't return.
- **A host-provided async runtime for processors** — the synchronous trait surface is
  the contract; embedding an async runtime in the host couples every plugin to it.

## Consequences

- The execution-flavor components stay in the tree as groundwork, not residue.
- Any future flavor design is DevEx-gated: the configuration surface must not grow.
- Realtime processors keep dedicated threads regardless of what flavors are added.
