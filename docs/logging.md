# Logging in streamlib

There is **one** sanctioned way to log in streamlib, per runtime. This document
names it, tells you what to reach for, and explains the enforcement layers that
keep everyone on the same path.

## The one way

| Runtime     | API                                    |
| ----------- | -------------------------------------- |
| Rust        | `tracing::{trace,debug,info,warn,error}!` |
| Python SDK  | `streamlib.log.{trace,debug,info,warn,error}(message, **attrs)` |

The Python SDK and the Rust host produce the same unified JSONL
stream on disk (`<STREAMLIB_HOME>/.streamlib/logs/<runtime_id>-<started_at>.jsonl`)
and mirror to stdout. The host handler (escalate IPC `{op:"log"}`)
forwards helper-process records to the subscriber that owns the file.

**Don't reach for `eprintln!` / `println!` / `print` /
`logging.basicConfig`.** They're banned in library code.
Use the API above instead.

## Enforcement — three layers, all load-bearing

1. **Compile-time (Rust, clippy)** — `clippy.toml` configures
   [`disallowed-macros`](https://rust-lang.github.io/rust-clippy/master/index.html#disallowed_macros)
   to deny `println!`, `eprintln!`, `print!`, `eprint!`, `dbg!`. Library crates
   opt in via `[lints] workspace = true` in `Cargo.toml`. `cargo clippy
   --workspace` fails on any violation.

2. **AST walk (Rust + Python)** — `cargo xtask lint-logging`. On Python it
   scans `sdk/streamlib-python-wheel/python/**/*.py` for banned substrings
   (`print(`, `sys.stdout`, `sys.stderr`, `logging.basicConfig`). On Rust it
   parses rather than greps, so `#[cfg(test)]` and
   `#[allow(clippy::disallowed_macros)]` are honoured, and it skips `tests`
   directories outright. That last exemption is why it and clippy can disagree
   about the same file: a `[[bin]]` whose `path` points into `tests/` is a
   default target to clippy and an exempt path to this walk. Exits non-zero
   with each offending file+line on failure.

3. **Runtime capture** — the Rust host captures a helper process's fd2 at
   the process level for anything the static layers can't see
   (third-party libs, native code).

All three catch different things. The static-analysis layers (1 + 2) stop
first-party code from regressing. The runtime layer (3) catches third-party
dependencies and anything that slipped through. Do NOT delete any layer on
the grounds of redundancy.

## CI

`cargo xtask lint-logging` runs in `source-gates.yml`, as one of the
consolidated source-walking gates. `cargo clippy --locked --workspace
--no-deps` runs in `test.yml`'s Linux job, which already carries the engine's
build dependencies; default targets only, so a test's `println!` stays a test's
business, matching layer 2's exemption. `cargo fmt --all --check` runs in
`source-gates.yml` beside the walks.

`cargo xtask run-local-ci-gates` runs the two static layers before you push.
Layer 3 is a property of the running host, not a gate, so nothing runs it
ahead of time.

> ~~Both checks run on every PR and push to `main` via
> `.github/workflows/lint-logging.yml`.~~ — Superseded 2026-08-16. That
> workflow was retired when PR #1857 consolidated 13 jobs into 6, and for a
> while afterwards nothing ran `cargo clippy` or `cargo fmt` at all: the claim
> above described enforcement that had stopped existing, and an `eprintln!` in
> each of two adapter test helpers sat unnoticed behind it.

## Exceptions — how to add one when you really need it

### Binary crates

Binary-only crates (`xtask`, examples)
do NOT opt into the workspace `[lints]` block because stdout IS their user
output channel. The rule only applies to library crates.

### Test code

`#[cfg(test)]` modules inside library crates and integration tests under
any zone crate's `tests/` dir (`runtime/*/tests/`, `sdk/*/tests/`,
`adapters/*/tests/`, …) are allow-listed — `println!` / `eprintln!` there are
fine. This matches the original lockout design (#441): "Overrides
allowed only in `tests/`, `examples/`, `build.rs`, and `xtask`." CI
enforces this naturally — `cargo clippy --workspace --no-deps` compiles
only lib + bin targets, so `#[cfg(test)]` code isn't linted.

If you run `cargo clippy --workspace --all-targets` or `--tests` locally
you'll see disallowed-macro errors from this test-side code. They are
NOT regressions and do NOT need fixing — the CI gate intentionally
doesn't include `--tests`.

### Individual files

Files that legitimately bypass the unified pathway because they *install*
it — e.g. the wheel's helper bootstrap (`_helper.py`) and log plumbing
(`_runtime_log_reader.py`) — carry a file-level pragma near their
copyright header:

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

## Release-build level stripping

`tracing` supports compile-time level filtering — call sites above the
configured maximum are codegen'd to `{}` and have zero runtime cost.
Streamlib pins two behaviors:

| Build | `trace!` | `debug!` | `info!` / `warn!` / `error!` |
| --- | --- | --- | --- |
| debug | live | live | live |
| release (default) | **stripped** | live | live |
| release + `--features streamlib/strip_debug_logging` | **stripped** | **stripped** | live |

- **Workspace default** enables `tracing/release_max_level_debug` — every
  release build strips `trace!` so per-frame / per-RHI-op tracing is
  safe to sprinkle on hot paths without runtime cost.
- **Opt-in `strip_debug_logging`** on the `streamlib` crate activates
  `tracing/release_max_level_info`, which additionally strips `debug!`.
  Production images that want a smaller release output enable this
  explicitly: `cargo build --release --features streamlib/strip_debug_logging`.
- **`warn!` / `error!` are never stripped.** Production JSONL must
  capture failure modes under every config. `release_max_level_off`,
  `release_max_level_error`, and `release_max_level_warn` are NOT
  exposed as streamlib features.
- **`debug!` stays live in release by default** — on-site diagnostics
  rely on it. Don't bump the workspace default to `release_max_level_info`.

Verify the effective level at compile time via
`tracing::level_filters::STATIC_MAX_LEVEL`.

## Recap

- One API per language: `tracing::*!` (Rust) / `streamlib.log.*` (Python).
- Three enforcement layers: clippy, xtask lint, runtime fd capture.
- `trace!` is zero-cost in release; `debug!` is opt-out via
  `strip_debug_logging`; `warn!` / `error!` are never stripped.
- Binary crates and installer/bootstrap files are the only acceptable
  exceptions.
- CI fails fast on regressions; don't try to bypass it — extend the
  pathway instead.
