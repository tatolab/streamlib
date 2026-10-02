# The Tatolab names

Rationale for the `[tatolab-names]` entry in `docs/plan/ARCHITECTURE.md` §Packages & extension
model, decided 2026-10-01 under the one-runtime-per-machine pivot.

## Trigger

Read this before naming a distribution, an import path, a binary or a command, before shipping a
package under `tatolab.*`, and before adding a `tatolab/__init__.py` anywhere.

## Decision

Tailscale's shape: the always-on program is `tatolabd` (as `tailscaled`), the CLI `tatolab` (as
`tailscale`), the app Tatolab. "Tatolab" names the product; every pip distribution names
what it is, `tatolab-<what>` importing as `tatolab.<what>`, so the one pip ships for writing
streams is `tatolab-stream`, importing as `tatolab.stream`. `tatolab.*` is a PEP 420 namespace — Google's
`google-cloud-*` pattern — shared only by Tatolab's own distributions: `tatolab.stream`,
`tatolab.runtime` (lent by `tatolabd`, never pip-installed), and optional first-party extensions
`tatolab-<name>` → `tatolab.<name>`. Third-party packs keep their own names.

## Rejected alternatives

- **`tatolab` as the pip name for the stream library** (briefly recorded, reversed the same day
  by the owner). The bare name means the whole product everywhere else — the app, brew, the CLI —
  so a pip `tatolab` holding only the stream-building part reads as the product and is not; the
  spelled `tatolab-stream` matches every extension (`tatolab-moq`). The bare PyPI name is
  held by a placeholder that installs nothing (`tatolab` 0.0.0, published 2026-10-02), so a
  guessed `pip install tatolab` meets a pointer instead of a stranger's package.
- **`tatolab-streams` (plural).** Reads as a collection of streams, which is what a pack is.
- **One `tatolab` package owning `tatolab/__init__.py`.** A regular package shadows every
  namespace portion on the path (PEP 420), hiding the runtime's lent portion and every extension
  (#2561's finding).
- **Third parties under `tatolab.*`.** Blurs what Tatolab maintains; Google's namespace is
  Google's alone.

## Consequences

- `import tatolab` alone gives nothing useful; code imports `tatolab.stream` or an extension.
- The names were free on PyPI, crates.io, Homebrew and npm on 2026-10-01. PyPI reserves no name
  before an upload, so `tatolab-stream` and the extensions are claimed by their first real release;
  the owner held the bare PyPI name with the placeholder on 2026-10-02.
- Whether the MoQ and WebRTC nodes become pip extensions or ship inside the app is the packs
  decision, not a naming one; their names hold either way.
- The repo, `streamlib` and the internal crate names stay until the rename step builds this.

## Decided 2026-10-02: no public Python name says "processor"

The stream vocabulary retired "processor" on every user surface, and the change that re-spelled
the stream-building names left the per-node classes — the windows a node owns, its output
texture ring, its link access — for a rename no change owned. The move from `streamlib` to
`tatolab.*` makes every user re-import everything, so it re-spells every public Python name
still saying "processor" in the same break: the four classes, the module holding the ring, the
two output-pool methods on the GPU capabilities, and the contexts' `processor_id`, which becomes
`node_id`, the id `graph` renders on the node. Owner, 2026-10-02.

Rejected:
- *A rename change of its own after the move.* A second break for every author, and every
  ticket between the two writes the old names into new code.
- *Renaming only the four classes first named.* Leaves the rest to be found and orphaned the
  same way.

Consequences:
- The engine's Rust identifiers and the wire between runtime and processor interpreter keep
  "processor" until the rename step; a Python-visible name can differ from the Rust type behind it.
- Names an earlier change deletes — `@processor`, the added-processor handle, the port
  references — are removed, not renamed.
