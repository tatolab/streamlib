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

This records the current best understanding of the shape. The direction is settled; the
mechanism's specifics are expected to move during the align and implementation, and nothing
below is worded to prevent that.

1. Optional capabilities ship as separate PyPI extension wheels — Rust inside for speed, a Python
   processor as the binding — that depend on the `streamlib` wheel as a binary and never build it
   from source.
2. Two mechanisms: a processor extension (a Python processor calling native code in its own
   wheel directly — no engine round trip on the data path) and a capability extension — support
   code declared by a standard entry point that pip records and the engine runs once at startup,
   like loading a driver — sandboxed so no two packages can unsafely alter engine features,
   extending rather than rewriting engine pieces.
3. The engine keeps and exposes what belongs in core — primitives, plumbing, the handle-shaped
   surface — as engine code, so an extension does not rebuild plumbing; and an extension may
   introduce engine-grade capabilities the engine does not provide — graphics processing,
   networking, a device class — the Unreal-module shape. What an extension needs and the engine
   does not yet expose is engine work done inside the extension's own change.
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

**Two mechanisms, because processors and support are different things.** A processor is a
graph node; the plan already lets a pip-installed package supply one, and `rt.add` on the class
is its registration — nothing new is needed for the processor half except the primitives it
reaches for, and it calls its own package's Rust directly rather than asking the engine to call
back into the wheel. A capability extension is the support that has to exist before such a
processor runs — a device library brought up, a network stack initialised, an engine-grade
capability the engine does not itself carry (a specialised graphics pass, a transport, a device
class) made available — the way a driver is loaded before the code that uses it. That needs its
own door, and the door is the one the Python ecosystem already has: a standard entry point in
the wheel's `pyproject.toml`, recorded by pip at install and read by the engine through
`importlib.metadata` at startup. `pip install streamlib-webrtc` is then the whole of enabling
it, the way a pytest plugin enables itself. This is not a file scan; it is pip's own registry.
The analogy the owner named is Unreal Engine modules: specialised, engine-grade, optional.

**Sandboxed, because two packages must not be able to fight.** An extension extends; it never
rewrites. The engine mediates every capability an extension registers, so two packages that both
extend the same area compose or refuse by name — neither reaches engine internals, neither can
leave the engine in a state the other did not expect. This is the same wall built-ins already
live behind, applied to code the engine did not ship.

**Networking first, because it has a consumer and needs no GPU door.** WebRTC and MoQ are CPU
and network work over encoded bags the codec blocks already publish; they exercise every piece
of the model — a wheel with Rust inside, the entry-point support hook, the per-frame path
through the wheel's own Rust — without first closing any primitive gap on the Python surface. Proving the model on the
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
- **Explicit activation in `app.py` as the only door (the Vite form)** — considered, and it
  remains a reasonable per-app opt-out. The owner chose the entry-point registry pip already
  maintains (the pytest form): the sandbox is what makes automatic activation safe, and an
  opt-out for one app is cheaper than an opt-in for every app. Walking `pyproject.toml` files
  at runtime was never on the table — the entry point is pip's registry, read once.
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
- Where the support hook runs — the app process at startup, each helper at spawn, or both — is
  the align's to settle; the driver analogy suggests once per process that takes an engine
  role, idempotently.
- The placement rule is unchanged: a processor extension's Python class runs in its own helper.
  Whether its native code may ever be called in the app process is OPEN and is the owner's
  ruling to make, never a session's inference.
- `packages/` gains its first live entry with networking; the publish path the plan deferred
  "until the first one wants a home" is now owed by that work.
- The helper hop's per-frame cost has never been measured; the plan's "fits within the helper
  hop's budget" is an argument, not a number. The first extension should measure it.
