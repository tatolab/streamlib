# Logging in streamlib

There is **one** sanctioned way to log in streamlib, per runtime. This document
names it, tells you what to reach for, and explains the enforcement layers that
keep everyone on the same path.

## The one way

| Runtime     | API                                    |
| ----------- | -------------------------------------- |
| Rust        | `tracing::{trace,debug,info,warn,error}!` |
| Python SDK  | `streamlib.log.{trace,debug,info,warn,error}(message, **attrs)` |
| Deno SDK    | `streamlib.log.{trace,debug,info,warn,error}(message, attrs)`   |

Both polyglot SDKs and the Rust host subscribe produce the same unified JSONL
stream on disk (`~/.streamlib/logs/<runtime>.log`) and mirror to stdout. The
subprocess-side interceptors capture anything that slips through
(`print()`, `console.log`, raw writes to fd1/fd2) and tag it
`intercepted=true`. The host handler (escalate IPC `{op:"log"}`) forwards
records to the subscriber that owns the file.

**Don't reach for `eprintln!` / `println!` / `print` / `console.log` /
`Deno.stdout.write` / `logging.basicConfig`.** They're banned in library code.
Use the API above instead.

## Enforcement — three layers, all load-bearing

1. **Compile-time (Rust, clippy)** — `clippy.toml` configures
   [`disallowed-macros`](https://rust-lang.github.io/rust-clippy/master/index.html#disallowed_macros)
   to deny `println!`, `eprintln!`, `print!`, `eprint!`, `dbg!`. Library crates
   opt in via `[lints] workspace = true` in `Cargo.toml`. `cargo clippy
   --workspace` fails on any violation.

2. **CI lint (Python + TypeScript)** — `cargo xtask lint-logging` scans
   `libs/streamlib-python/**/*.py` and `libs/streamlib-deno/**/*.ts` for
   banned substrings (`print(`, `sys.stdout`, `sys.stderr`,
   `logging.basicConfig`, `console.log`/`warn`/`error`/`info`/`debug`,
   `Deno.stdout.write`, `Deno.stderr.write`). Exits non-zero with each
   offending file+line on failure.

3. **Runtime interceptors** — the subprocess-side `_log_interceptors.py` and
   `_log_interceptors.ts` replace `sys.stdout`/`sys.stderr`/`console.*` with
   line-buffered shims that emit through `streamlib.log` with
   `intercepted=true`. The Rust host also captures fd2 at the process level
   for anything that escapes that (third-party libs, native code).

All three catch different things. The static-analysis layers (1 + 2) stop
first-party code from regressing. The runtime layer (3) catches third-party
dependencies and anything that slipped through. Do NOT delete any layer on
the grounds of redundancy.

## CI

Both checks run on every PR and push to `main` via
`.github/workflows/lint-logging.yml`. A PR is merge-blocked until both jobs
are green.

## Exceptions — how to add one when you really need it

### Binary crates

Binary-only crates (`streamlib-cli`, `streamlib-runtime`, `xtask`, examples)
do NOT opt into the workspace `[lints]` block because stdout IS their user
output channel. The rule only applies to library crates.

### Individual files

Two kinds of files legitimately bypass the unified pathway, because they
*install* it: the interceptor itself (`_log_interceptors.py`,
`_log_interceptors.ts`) and subprocess bootstraps that emit diagnostics
before the logger is wired (`subprocess_runner.py`, `subprocess_runner.ts`).
Those files carry a file-level pragma near their copyright header:

```python
# streamlib:lint-logging:allow-file — installs the unified pathway; must touch sys.stdout/sys.stderr directly
```

`cargo xtask lint-logging` reads this marker and skips the entire file.
Don't add the pragma to new files lightly — justify why in the same
comment.

### Individual lines

For a single-line exception (Python/TS), append a trailing
`# streamlib:lint-logging:allow-line` or
`// streamlib:lint-logging:allow-line` comment. Prefer a file-level
pragma if the whole file justifiably bypasses; prefer per-line for one-off
shims.

### Rust library exceptions

On the Rust side, bootstrap error paths in `core/logging/init.rs` wrap their
one `eprintln!` fallback in:

```rust
#[allow(clippy::disallowed_macros)]
{
    eprintln!("streamlib::logging: ...");
}
```

Use this pattern sparingly — it should be obvious from context *why* tracing
is unavailable at the call site. If the call site *could* use tracing,
please do.

### Third-party chatty dependencies

If a dep writes to stdout/stderr directly and that noise shows up in your
logs: the runtime fd-level interceptor already captures it and tags the
records `intercepted=true channel=stdout|stderr`. You don't need to do
anything. If the noise is genuinely unhelpful, consider filtering it in
the subscriber rather than suppressing at the source.

## Recap

- One API per language: `tracing::*!` (Rust) / `streamlib.log.*` (Python,
  Deno).
- Three enforcement layers: clippy, xtask lint, runtime interceptors.
- Binary crates and installer/bootstrap files are the only acceptable
  exceptions.
- CI fails fast on regressions; don't try to bypass it — extend the
  pathway instead.
