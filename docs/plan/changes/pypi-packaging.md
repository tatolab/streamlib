# Change: pypi-packaging

> **Superseded 2026-08-02 by `importable-python-library.md`.** The shipped artifact is
> now a PyO3 wheel (Python API + CLI + engine), not packaging around a standalone
> binary, and initial releases are repo-hosted (PyPI deferred until the rename). Do not
> derive tickets from this file; `/reconcile-tracker` retires its open tickets.

Implements the "installs streamlib from PyPI" clause of `[product-mvp-sentence]`
(§Product) and the "PyPI ships exactly one artifact" consequence of
`[single-binary-launch]` (§Control plane). No ADR: nothing here touches the plugin ABI,
RHI, IPC wire format, or processor model — this is packaging and publish plumbing around
the existing binary. Recon verified every claim below at file:line on 2026-07-30.

## Current state (recon)

The `streamlib` binary (crate `streamlib-cli`, `tools/streamlib-cli/Cargo.toml:13-15`,
workspace version 0.9.7) already statically links the api-server and the build
orchestrator. The release build is ~30 MB and links only `libc`/`libm`/`libgcc_s`/
`libstdc++` — Vulkan, nvJPEG, and all media libs are dlopen'd at runtime — so it is
already near-manylinux-clean. There is no publish path of any kind: `release-please.yml`
is version bookkeeping only, Docker is the only assembled distribution, and no workflow
mentions PyPI/twine/maturin. The PyPI distribution name `streamlib` is currently claimed
*inside the tree* by the pure-Python subprocess SDK (`sdk/streamlib-python/
pyproject.toml:9`, version 0.4.30 on its own stale version line split from the
workspace's 0.9.7), and is unclaimed on pypi.org (verified 404, 2026-07-30).

## Behavior after this change

`pip install <name>` on a Linux x86_64 machine installs one PyPI artifact that puts the
`streamlib` binary on PATH — the ruff/uv pattern: one install, no Rust toolchain, no
system packages at install time. The artifact version is the workspace version; there is
no second version line for anything it contains. Publishing is staged: every release cut
by release-please attaches the wheel to the GitHub Release (pip-installable by URL,
name-free); the pypi.org claim is a separate owner-gated flip blocked on the public-name
decision (#1323 — crates.io `streamlib` is taken, a rebrand is in play; the distribution
name everywhere in this change is a placeholder for the decided name, and the import
name / dist name can differ, so the packaging work is name-agnostic). A pip-installed user's
Python processor subprocesses work offline: the native cdylib and the subprocess SDK
resolve from the installed distribution, never from `target/` and never from the network.

## ADDED

- ADDED: a platform wheel for the `streamlib` distribution, tag
  `py3-none-manylinux_<floor>_x86_64` (glibc floor measured at implementation against
  the platform-floor distro; Python-version-independent since nothing links libpython),
  containing the release `streamlib` binary exposed on PATH via the wheel scripts
  mechanism. Build tooling (maturin bin-bindings vs. hand-rolled wheel assembly) is an
  implementation choice; the contract is the tag set, the entry point, and the BUSL-1.1
  license metadata. The wheel version is the workspace version (PEP 440-normalized;
  `-dev.N` maps to `.devN`).
- ADDED: the wheel bundles `libstreamlib_python_native.so`, and
  `native_lib_resolver.rs` gains an installed-distribution tier — resolution order
  becomes env override → installed distribution → `<data_dir>/cache/native/<triple>` →
  workspace `target/` (`runtime/streamlib-engine/src/core/compiler/compiler_ops/
  native_lib_resolver.rs:61-108`).
- ADDED: the artifact carries the pure-Python subprocess SDK wheel as an embedded
  resource; orchestrator venv provisioning installs `streamlib` from it via the
  existing staged-wheel path (`tools/streamlib-build-orchestrator/src/
  python_venv.rs:110-125`) instead of an editable workspace install — pip-installed
  users provision processor venvs with zero network access to the SDK.
- ADDED: the distribution declares `uv` as an install dependency (uv ships its own
  binary wheels), so the orchestrator's `uv venv` / `uv pip` calls
  (`python_venv.rs:93-145`) work with no toolchain installed — the zero-ceremony "no
  toolchain" bar.
- ADDED: a publish job chained off the release-please release cut
  (`.github/workflows/release-please.yml:132-143` is the existing tag/release step):
  build the manylinux wheel, smoke-gate it (install into a clean venv on the floor
  distro; the CLI must respond), attach it to the GitHub Release. The pypi.org publish
  (trusted publishing / OIDC, no long-lived tokens) is a separate, owner-gated step
  that does not run until the public name is decided (#1323); flipping it on is a
  workflow-config change, not new engineering.

## MODIFIED

- MODIFIED: `sdk/streamlib-python/pyproject.toml` — per the wheel-composition decision
  below: either subsumed into the single distribution's metadata (option A) or renamed
  off the `streamlib` PyPI name (option B). Either way its independent version line and
  the stale authority comment (`:10-14`, claiming `cargo xtask static-package-source
  emit` — an emitter whose pypi tree was removed 2026-07-13 — is the version source) go.
- MODIFIED: `.github/workflows/release-please.yml` — triggers the publish job on
  release creation, alongside the existing schemas publish hook (`:139-143`).
- MODIFIED: `docs/architecture/` install/distribution pages and `README.md` quick
  start — describe the shipped pip install path once it ships (fold-in time, per docs
  policy: shipped state only).

## REMOVED

Each bullet is a pattern the ship gate verifies is gone: **one artifact per bullet, plain
text, on the bullet's first line.** Continuation lines are prose the gate does not search.

> ~~REMOVED: `static-package-source emit`~~ — Deleted 2026-08-08 as provably wrong, not
> merely unprovable. The bullet asserted a tree-wide removal, but this change explicitly
> keeps the xtask command, which is named in `tools/streamlib-cli/src/commands/pkg.rs`,
> `docs/architecture/package-source.md` (×3) and
> `docs/architecture/package-development-model.md` — so it could never reach zero. What it
> actually described, deleting the stale version-authority comment from a surviving file,
> is a modification and is already stated by the MODIFIED bullet above (`:10-14`). No
> inventory is lost.

- REMOVED: version = "0.4.30"

  Scoped `sdk/streamlib-python/pyproject.toml` — the independent Python version line; the
  distribution version becomes workspace-derived.

## Wheel composition — RESOLVED by owner, 2026-07-30

One distribution = the binary + the pure-Python SDK import package, one
workspace-derived version. The MVP flow needs both the binary on PATH and
`import streamlib` working in the user's editor/venv (app.py imports decorated
processor classes for go-to-definition and type checking; execution stays in
engine-spawned subprocess venvs either way), and one PyPI name cannot be two
distributions. This matches "PyPI ships exactly one artifact", kills the 0.9.7/0.4.30
skew permanently, and needs exactly one public name (relevant while the rebrand,
#1323, is open). Cost accepted: SDK-only consumers download a ~30 MB platform wheel,
platform-gated where pure Python wasn't (the platform floor is Linux + NVIDIA by
decision). Rejected: a binary-only wheel with a separately-named, separately-versioned
SDK distribution.

## Out of scope (adjacent, separately tracked)

- `run` / `dev` / `new` verbs, `streamlib-runtime` retirement, api-server relocation
  into `runtime/` — implementation deltas of `[single-binary-launch]` /
  `[product-mvp-sentence]`, separate changes. The wheel ships whatever verbs the binary
  has at each release; publishing before `dev` exists is allowed and expected.
- Processor-package (`.slpkg`) distribution and versioning — §Distribution & versioning
  stays OPEN; this change takes no position on package publishing.
- Additional wheel targets (Linux aarch64/Jetson, other platforms) — added targets
  later, beneath architecture; the decided floor is Linux + NVIDIA (x86_64 first).
- Residual consumers of the removed internal pypi-simple tree
  (`sdk/streamlib-idents/src/package_source.rs:118-122`, `python_venv.rs:330-335`,
  Dockerfile `UV_INDEX`) — dead-code cleanup, refactor-tier, no change artifact needed.
- Publishing SDK libraries to crates.io / npm (#1323) — untouched by this change.
- The public-name / rebrand decision itself (#1323, and the name inside the §Product
  MVP sentence) — an owner `/align` item; this change is name-agnostic up to the gated
  PyPI flip.
