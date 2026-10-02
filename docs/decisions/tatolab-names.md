# The Tatolab names

Rationale for the `[tatolab-names]` entry in `docs/plan/ARCHITECTURE.md` §Packages & extension
model, decided 2026-10-01 under the one-runtime-per-machine pivot.

## Trigger

Read this before naming a distribution, an import path, a binary or a command, before shipping a
package under `tatolab.*`, and before adding a `tatolab/__init__.py` anywhere.

## Decision

Tailscale's shape: the always-on program is `tatolabd` (as `tailscaled`), the CLI `tatolab` (as
`tailscale`), the app Tatolab. The one thing pip ships for writing streams is the `tatolab`
distribution, importing as `tatolab.stream`. `tatolab.*` is a PEP 420 namespace — Google's
`google-cloud-*` pattern — shared only by Tatolab's own distributions: `tatolab.stream`,
`tatolab.runtime` (lent by `tatolabd`, never pip-installed), and optional first-party extensions
`tatolab-<name>` → `tatolab.<name>`. Third-party packs keep their own names.

## Rejected alternatives

- **`tatolab-stream` as the pip name.** It spells its import path, Google's convention, but pip
  only ever ships one Tatolab thing for authoring, so the brand name alone is unambiguous and
  matches the app, the CLI and brew; extensions keep the spelled form.
- **`tatolab-streams` (plural).** Reads as a collection of streams, which is what a pack is.
- **One `tatolab` package owning `tatolab/__init__.py`.** A regular package shadows every
  namespace portion on the path (PEP 420), hiding the runtime's lent portion and every extension
  (#2561's finding).
- **Third parties under `tatolab.*`.** Blurs what Tatolab maintains; Google's namespace is
  Google's alone.

## Consequences

- `import tatolab` alone gives nothing useful; code imports `tatolab.stream` or an extension.
- The names were free on PyPI, crates.io, Homebrew and npm on 2026-10-01; registering them is the
  owner's call.
- Whether the MoQ and WebRTC nodes become pip extensions or ship inside the app is the packs
  decision, not a naming one; their names hold either way.
- The repo, `streamlib` and the internal crate names stay until the rename step builds this.
