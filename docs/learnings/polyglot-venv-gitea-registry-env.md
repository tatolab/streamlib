# Running a polyglot Python example / `pkg install` needs the Gitea registry env vars

## Symptom

Running any polyglot Python example (the `polyglot-*` examples) or
`streamlib pkg install` on a source package fails during the build
orchestrator's `materialize` — specifically the Python venv tail — with
one of:

- A `uv pip install` resolution failure for `streamlib` itself: `uv`
  only knows the public PyPI index, where the `streamlib` SDK does not
  exist, so the install can't find it.
- `failed to generate streamlib wire vocabulary in venv: Failed to
  resolve streamlib.yaml dependency graph` — the in-venv JTD codegen
  installed `streamlib` fine but can't fetch the SDK's schema-package
  deps (`@tatolab/core`, `@tatolab/escalate`) from the generic
  registry.

The Rust half of the package builds cleanly; only the Python venv
provisioning fails, so it reads like a Python-only or example-only
problem. The error surfaces deep inside `materialize` (a codegen / uv
error) with no "you forgot an env var" hint — which is why it gets
rediscovered from the stack trace over and over instead of recognized.

## Cause

The `streamlib` Python SDK and the schema packages it depends on
resolve from the self-hosted Gitea registry, not from public indexes.
Provisioning a package's venv has **two** registry-dependent steps, and
they read **different** environment channels:

1. `uv pip install` of the package pulls `streamlib` from Gitea's
   **pypi** index — driven by `UV_INDEX` (or a `pip.conf`). Without it,
   `uv` only consults public PyPI.
2. The in-venv codegen that regenerates `streamlib/_generated_`
   resolves the SDK's `streamlib.yaml` schema-package deps from Gitea's
   **generic** registry via the resolver's `ResolverOptions::from_env`,
   which reads `STREAMLIB_REGISTRY_URL` (or `GITEA_URL`) and
   `STREAMLIB_REGISTRY_TOKEN`. The generic-registry list/download
   endpoint requires authentication on the streamlib Gitea instance, so
   the token is needed in practice even though the resolver treats it as
   optional.

Setting only `UV_INDEX` gets you past step 1 and straight into the
step-2 `streamlib.yaml dependency graph` failure — which is the more
confusing of the two because the venv exists and `streamlib` imports
fine.

## Fix

Provide all three env vars before running an example or `pkg install`:

- `UV_INDEX` → the Gitea pypi simple index for the `tatolab` org (so
  `uv` finds `streamlib`).
- `STREAMLIB_REGISTRY_URL` → the Gitea base URL (for the codegen
  resolver).
- `STREAMLIB_REGISTRY_TOKEN` → a Gitea `read:package` token (the
  generic registry's read endpoint requires auth).

Keep the actual host/port and token **out of versioned files** — that's
deployment topology, not source. Put them in your gitignored
`*.local.sh` env wrapper (the repo convention for registry access) and
source it, or export them in your shell. Don't hardcode a registry URL
or token into an example, a script committed to the repo, or this
learning.

If you hit either symptom above, set the three env vars first — don't
go debugging the codegen step or the example's Python.

## Reference

- Model (which registry backs which dependency kind, and the env each
  reads): `docs/architecture/gitea-registry-distribution.md` — consume
  side. `UV_INDEX` / `pip.conf` for the pypi install;
  `ResolverOptions::from_env` → `STREAMLIB_REGISTRY_URL` /
  `STREAMLIB_REGISTRY_TOKEN` for the generic registry.
- The provisioning step that emits the symptom:
  `libs/streamlib-build-orchestrator/src/python_venv.rs` (the venv tail
  of `materialize` — `uv venv` → `uv pip install` → `_generated_`
  codegen → `compileall`).
- Registry access topology lives in gitignored `*.local.sh` wrappers,
  not in the repo.
