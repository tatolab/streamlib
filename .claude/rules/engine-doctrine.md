# Engine doctrine

StreamLib is a game-engine-shaped substrate: one core system per concern, many consumers each.

## The plan decides; sessions implement

- The architecture plan (`docs/plan/ARCHITECTURE.md` + `docs/plan/architecture.excalidraw`) is
  the single source of architectural decisions. Tickets implement the plan; they never make
  architecture. If work needs a decision the plan doesn't state, **stop and bring it to the
  owner** — never decide inline, never infer it from existing code, consumers, or git history.
- Existing code that contradicts the plan is legacy to be replaced, not a pattern to accommodate.
  A pivot updates the plan first; code follows. When the plan changes direction, the old shape is
  ripped out — no parallel old/new coexistence.
- Scope is what the ticket states, gated by the plan. Findings outside it go in the PR
  description as notes, not into new tickets and not into the diff.

## Structure

- **Search first, extend never parallel.** Before adding any trait / struct / helper / module,
  prove no core system already covers the concern (RHI, `GpuContext`, pubsub, processor model).
  Extend the existing one; a parallel abstraction is the default-wrong move.
- **Engine-wide defects get fixed at the engine layer**, never bandaided in the consumer that
  surfaced them.
- **Pattern migrations cover the engine tree only** — runtime, SDK, adapters, engine tests,
  docs. `packages/` and `examples/` lag by design and are never in scope.

Prohibited in library code (tests/examples exempt):
- `todo!()` / `unimplemented!()`, no-op methods, back-compat shims (pre-1.0 — rename cleanly).
- Bypassing type safety to compile; reshaping library code to satisfy a test.
- Silent DRY refactors; auto-fixing unrelated issues surfaced by check/test/clippy — report them.

Conventions:
- Errors via the core `Error` enum + `Result<T>`; `?` over `.unwrap()` in library code.
- Logging is `tracing` only — no `println!` / `eprintln!` (CI enforces).
- All timekeeping uses monotonic clocks (Rust and Python alike), never wall-clock or sleep-based.
- Git deps pinned by `rev = "<sha>"` or `tag`; never bare `git` / `branch`, including
  `[patch.crates-io]`.
- Rustdoc: public items get a one-line doc; no `# Example` / `# Usage` sections or ASCII diagrams
  in doc comments; use intra-doc ``[`Type`]`` links; `cargo doc -p streamlib --no-deps` stays
  warning-free.
- Platform dirs are conditionally compiled — `core/` is platform-agnostic, `apple/` and `linux/`
  are per-platform. Never put a `#[cfg]` inside a platform-specific directory.
- macOS / Apple-path changes are cross-compile-verified on Linux (`cargo check --target
  aarch64-apple-darwin`) before merge.
