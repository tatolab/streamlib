# Extension model — optional capabilities ship as extension wheels

Rationale for the `[extension-model]` entries in `docs/plan/ARCHITECTURE.md` §Packages &
extension model, §Consumers, §Media I/O and §Networking, declared by the owner 2026-09-04.
Supersedes the general rule, carried since the 2026-08-02 SDK-shape pivot, that a first-party
native capability is a built-in statically linked into the wheel. The twelve built-ins that
shipped under that rule stay; the rule does not.

## Trigger

Read this before adding any first-party native processor to `streamlib-media-builtins`, before
answering "should this be a built-in or a package", before designing how a pip-installed package
extends what the engine can do, and before proposing that a capability be built into the engine
because it is easier than exposing the primitive it needs.

## The direction, verbatim (owner-stated 2026-09-04)

1. Optional capabilities ship as separate PyPI extension wheels — Rust inside for speed, a Python
   processor as the binding — that depend on the `streamlib` wheel as a binary and never build it
   from source.
2. Two mechanisms and no third: a processor extension (a Python processor talking to native code
   in its own wheel) and a capability extension registered by one explicit line in `app.py`,
   sandboxed so no two packages can unsafely alter engine features — extending capabilities
   only, never rewriting engine pieces, the Vite-plugin shape.
3. The engine keeps and exposes everything that truly belongs in core — primitives, plumbing, the
   handle-shaped surface — as engine code, so an extension never rebuilds plumbing; what an
   extension needs and the engine does not yet expose is engine work done inside the extension's
   own change.
4. Networking — WebRTC and MoQ — is the next work and the first extension, because not every app
   needs it, and a capability with a consumer is what proves the model.
5. The twelve shipped built-ins stay; `JpegDecoder` is frozen — neither built nor retired — until
   its drone consumer returns; no new built-in ships without meeting the criterion.

## Why

**The engine cannot be the home of every capability, because then every capability builds the
engine.** A user who wants WebRTC should `pip install` a wheel that depends on `streamlib`, not
rebuild `streamlib` from source or wait for a release that carries it. The 2026-08-02 pivot
decided this for third parties in so many words — "an ordinary Python package whose native
internals expose capabilities to Python as handles… never links the engine" — and then every
first-party capability that followed took the shortcut of linking the engine directly. Between
the MVP close-out and this pivot the built-in set went from three to twelve, each addition
approved through an align, none of them contestable, because the plan held no criterion: its
placement clause read "what belongs in the engine goes in the engine". The result was a
`packages/` directory the plan describes as the home of first-party optional packages and which
has never held one.

**The criterion is the deadline and the primitive, plus a consumer.** The three built-ins that
were argued for on the record — display, microphone, speaker — were argued from a deadline the
helper hop cannot meet: a vsync-paced present loop, a device audio callback. The video codecs
sit on Vulkan Video sessions, an engine-only primitive. Those are the two honest reasons a
per-frame path must live in the app process, and they are stated as the criterion so a
thirteenth built-in has to pass them. The third clause — a named consumer — is what the JPEG
proposal lacked: a decoder for a stream nothing in the tree produced, carried into the roster
because its backend existed. Under the criterion it would not have been proposed.

**Two mechanisms, because processors and capabilities are different things.** A processor is
a graph node; the plan already lets a pip-installed package supply one, and `rt.add` on the
class is its whole registration — nothing new is needed for the processor half except the
primitives it reaches for. A capability is what the engine can *do* — carry a link over a
network, discover peers, serve a control-plane surface — and a package that adds one is not
adding a node; it is extending the engine. That needs its own door, and the door is explicit:
one line in `app.py` naming the extension, the way a Vite config names its plugins. Discovery
by scanning installed distributions was rejected because it makes the set of things extending
the engine depend on what happens to be in the venv rather than on what the app said.

**Sandboxed, because two packages must not be able to fight.** An extension extends; it never
rewrites. The engine mediates every capability an extension registers, so two packages that both
extend the same area compose or refuse by name — neither reaches engine internals, neither can
leave the engine in a state the other did not expect. This is the same wall built-ins already
live behind, applied to code the engine did not ship.

**Networking first, because it has a consumer and needs no GPU door.** WebRTC and MoQ are CPU
and network work over encoded bags the codec blocks already publish; they exercise every piece
of the model — a wheel with Rust inside, explicit registration, the per-frame path through the
binding — without first closing any primitive gap on the Python surface. Proving the model on the
capability the owner most wants next is the point. The risk is accepted knowingly: if the
mechanism has a flaw, networking finds it, and that beats finding it on a capability nobody
asked for.

**The built-ins stay, because moving them buys nothing now.** Twelve shipped processors with
tests, rig proofs and a Python surface would cost a rewrite each to become extensions, for no
user-visible gain. The rule changes going forward; the record of what shipped under the old rule
is kept as a record.

## Rejected alternatives

- **Keep building capabilities into the engine** — every optional feature makes every user
  build or ship it; the wheel grows without bound; a third party can never do what first-party
  code does, so the extension model is never exercised and never trusted.
- **A narrow native ABI for extensions** — the deleted plugin ABI again: a vtable surface to
  maintain, two engines in one process as a failure mode, build fingerprints. The CPython ABI
  already exists, is already shipped, and pip already handles it.
- **Discovery by scanning dependencies' `pyproject.toml`** — magic: the engine's capabilities
  become a function of the venv rather than the app. The entry-line form is one line more and
  says exactly what extends the engine.
- **Prove the extension model on JPEG first** — a capability with no consumer; it would prove
  the distribution shape while building a decoder nobody would run.
- **Move the twelve built-ins out to extensions as part of this pivot** — churn without gain;
  the criterion governs the next one, not the last twelve.

## Consequences

- The Python surface is the contract for Rust extensions too: a primitive an extension needs
  must exist on the wheel's public surface, typed in the stub, before the extension can use it.
  Known gaps at the time of the pivot: a Python compute dispatch cannot bind a storage buffer,
  and codec sessions are not exported to Python. Each is engine work owed by the first extension
  that needs it.
- A Rust-side extension SDK — typed wrappers over the engine's Python objects, depending on
  `pyo3` and not on the engine — is owed the first time an extension author would otherwise
  hand-write PyO3 against the stub. Its shape is the networking align's to decide.
- Rust apps with no interpreter cannot load extension wheels; they compile extensions as source
  crates. Python is the binding for optional capabilities, by the owner's statement.
- The placement rule is unchanged: a processor extension's Python class runs in its own helper.
  Whether its native code may ever be called in the app process is OPEN and is the owner's
  ruling to make, never a session's inference.
- `packages/` gains its first live entry with networking; the publish path the plan deferred
  "until the first one wants a home" is now owed by that work.
- The helper hop's per-frame cost has never been measured; the plan's "fits within the helper
  hop's budget" is an argument, not a number. The first extension should measure it.
