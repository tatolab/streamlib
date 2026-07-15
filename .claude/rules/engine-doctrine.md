# Engine doctrine

StreamLib is a game-engine-shaped substrate: one core system per concern, many consumers each.

- **Search first, extend never parallel.** Before adding any trait / struct / helper / module
  reused across more than one call site, prove no core system already covers the concern (RHI,
  `GpuContext`, pubsub, processor model). Extend the existing one; a parallel abstraction is the
  default-wrong move. Load-bearing new shapes get a short "why / what / alternatives" note in the
  PR.
- **Production-grade by default for engine work** (RHI, IPC wire format, processor model, public
  ABI crates, escalate ops). Comprehensive error taxonomy at trait birth (named variants, no `()`
  errors, no panic-on-internal-bug); `tracing::instrument` on every public entrypoint; ABI version
  constants on every cross-process / cross-crate / cross-language boundary; conformance tests when
  a trait will have multiple implementors; layout regression tests for every `#[repr(C)]` type in
  every language that mirrors it. A lighter shape is a scope-cut that needs a stated reason.
- **Design for the use-case class, not the one example in front of you.** A filed issue or in-tree
  caller is a known requirement, not a hypothetical.

Prohibited in library code (tests/examples exempt):
- `todo!()` / `unimplemented!()`, "temporary" hacks, no-op methods, back-compat / compat shims
  (pre-1.0, no external consumers — rename cleanly instead).
- Bypassing type safety to compile; reshaping library code to satisfy a test; tests that mock half
  the system or ignore errors to paper over a broken API.

Discipline:
- **Engine-wide defects get fixed at the engine layer**, never bandaided in the consumer that
  surfaced them — every consumer of that primitive hits the same bug.
- **Canonical-pattern sweep.** When a change makes a new pattern canonical, migrate every consumer
  of the old pattern in the same PR (Rust + Python + Deno + examples + processors + tests + docs +
  learnings), with a dated strikethrough on any doc that endorsed the old shape.
- **No silent DRY refactors** (extraction is fine if it replaces real duplication AND is called out
  in the PR); **no auto-fixing unrelated issues** surfaced by check/test/clippy — report them.

Conventions:
- Errors via the core `Error` enum + `Result<T>`; `?` over `.unwrap()` in library code.
- Logging is `tracing` only — no `println!` / `eprintln!` (CI enforces).
- All timekeeping uses monotonic clocks (Rust / Python / Deno), never wall-clock or sleep-based.
- Git deps pinned by `rev = "<sha>"` or `tag`; never bare `git` / `branch`, including
  `[patch.crates-io]`.
