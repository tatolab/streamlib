# Consumer trees: showcase examples, optional packages, from-scratch conversion

Rationale for the `[consumer-tree-disposition]` entries in `docs/plan/ARCHITECTURE.md`
§Consumers, decided 2026-08-30. Evidence: the post-windower consumer survey
(both trees enumerated and classified against the shipped engine surface) and a
read-only CI/workspace audit — no workflow or wheel test references either tree;
`packages/test-fixtures` is the sole workspace member under both.

## Trigger

Read this before converting, deleting, editing, or adding anything under `examples/`
or `packages/`; before proposing an examples repository split; or before designing any
distribution mechanism for first-party or third-party processors.

## Why from-scratch conversion, never in-place upgrade

The pre-pivot consumers are written against deleted machinery — the identity grammar,
the schema layer, `streamlib.yaml` manifests, the module system, the plugin ABI. An
in-place upgrade would change every line while wearing a smaller diff's costume, and
reading the old form teaches the model we removed. The logic (wiring topology, codec
specifics, capture edge cases) is the only value; git history preserves it past
deletion. Rejected: traditional upgrade (every line changes anyway); keeping old
directories beside new ones (two idioms in the showcase tree teaches the wrong one).

## Why examples/ stays in-repo

The wheel is pre-rename and pre-PyPI; an external examples repository buys the
holohub-style decoupling before there is a public release channel to decouple from.
In-repo, converted examples double as living documentation and exercise the local
package-linking path. The split remains open as a later move, deliberately undecided.

## Why packages/ is reborn as optional Python packages

The engine tree absorbs what is core (built-ins ship in the wheel); third-party
extension is PyPI/cargo by the shipped extension model. What remained was a home for
first-party *optional* integrations — wanted on PyPI eventually, not wanted in the
wheel. (Superseded 2026-09-04 by `extension-model.md`, which names these *extension wheels*, gives "core" a criterion,
and commissions the first one — networking.) Collocation solves the development loop: an in-repo consumer links the package
as a local path dependency, so no publish cycle sits inside an edit loop; externally
the wheel's own GitHub-hosted PEP 503 index serves them until the rename unlocks PyPI.
Rejected: retiring the tree entirely (leaves optional integrations homeless or bloating
the wheel); publishing every package before examples may use it (a publish loop inside
development).

## Why hold-until-mined for blocked consumers

The codec, networking, plugin, and screen-capture consumers embody working logic their
future aligns must mine (how a codec was wired to the RHI, what a capture path must
handle). A live tree is cheaper to mine than history, and each align's ship is the
natural deletion point — the same PR that folds the domain's decisions retires its
reference material. Rejected: delete-now-mine-history (raises the cost of exactly the
sessions these trees exist to serve).

## Why the delete-now list

Everything on it is superseded by deleted machinery (Deno SDK, plugin ABI, module
system, JSON graph submission) or by shipped pivots (live-mutation verbs removed,
helper placement, converted successors). Holding them serves no future align; the
halftone effect — the one piece with demo value — is recovered and queued as
conversion backlog.

## Why lag-by-design ends for converted consumers

A showcase that rots silently teaches a dead idiom — the exact failure the conversion
exists to end. Tracked backlog keeps breakage visible without giving consumers a veto
over engine work; runtime-first survives because nothing blocks the engine PR. The
canary exception exists because in-flight work occasionally needs a real consumer as
its proving ground — but a canary is normally planned as a separate path an example
adopts later, so in-stream example surgery stays rare by construction. No CI presence:
CI has no GPU, so a launch test cannot run there, and a compile-only gate would prove
staleness late and weakly.

## Why hello-streamlib retires

`streamlib new` scaffolds the minimal app — an entry file, a `processors/` package, a
worked effect — so the scaffold is the hello, and `camera-display` is the canonical
minimal example. A third "minimal" teaches nothing the other two do not. Note for
conversions: there is no entry-point or `pyproject.toml` registration mechanism for
processor classes anywhere in the wheel — plain importability is the whole contract
(side-effect-safe module, import-path identity, `rt.add(Class)`).
> Still true for processors after 2026-09-04 (`extension-model.md`): a processor extension
> is `rt.add(Class)` and nothing more. A *capability* extension is the one thing registered —
> by one explicit line in `app.py`, never by scanning — and its mechanism is the align's.

## Why no further distribution mechanism

The extension model already decided distribution: third-party native ships as an
ordinary Python package exposing transferable handles; Rust processors for Rust apps
are source-compiled cargo dependencies. The perceived gap assumed a replacement for
the deleted cdylib/slpkg path was still owed; it is not — closed-source Rust-for-Rust
is deliberately not a path, because the Python-package route serves closed-source
vendors and a second binary boundary would reopen everything the ABI deletion closed.
> Stands for *distribution* after 2026-09-04. What `extension-model.md` adds is
> *registration* of a capability extension, over the same CPython boundary — no second
> binary boundary.
