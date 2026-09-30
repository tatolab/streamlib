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
   processor as the binding for any processor the wheel supplies — that depend on the `streamlib`
   wheel as a binary and never build it from source.
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
through the wheel's own Rust — without first closing any primitive gap on the Python surface.
Every piece but one: reach into an engine-grade capability an extension introduces stays OPEN
until an extension brings one, and neither of these does. Proving the model on the
capability the owner most wants next is the point. The risk is accepted knowingly: if the
mechanism has a flaw, networking finds it, and that beats finding it on a capability nobody
asked for.

**The built-ins stay, because moving them buys nothing now.** Twelve shipped processors with
tests, rig proofs and a Python surface would cost a rewrite each to become extensions, for no
user-visible gain. The rule changes going forward; the record of what shipped under the old rule
is kept as a record.

## Why the hook runs in both processes and fails hard

The engine lives in the app process; the processor runs in its helper. A tokio runtime or a
device library brought up in one is not up in the other, and "like loading a driver" means
once per process that needs it. The helper already has the slot: the wheel is imported before
the processor's module by CPython's own package semantics, and the helper host installs its
log channel before that import precisely so anything the import raises is reportable — the
hook sits in that gap. Failing hard follows the engine's own sealed init-hook shape, which
fails runtime creation on any hook error and caches the failure: an extension that half
loaded would surface later as a processor that mysteriously cannot do its job, which is the
worse outcome.

## Why the wheels are standalone rather than workspace members

A first-party extension built as a member of the engine workspace would share the engine's
lockfile, pins and release cadence — first-party being special again, which is the shape
the pivot retires. Built standalone, `streamlib-webrtc` is built exactly as a third party
would build one: its own workspace root, its own lockfile, a dependency on the published
`streamlib` wheel by version. What that costs — the engine workspace's gates do not walk it,
and it needs its own CI lane and its own stub gate — is the cost every third party already
pays, and paying it first-party is how those lanes get built.

## Why the control plane carries nothing optional

The api-server grew a `moq` feature and a catalog route because the old MoQ package needed
somewhere observable to publish its state, and the control plane was the nearest surface.
Nothing ever enabled the feature, and under helper placement its handler cannot see the
sessions it reads. The mistake was the coupling — an optional capability wired into a core
surface by feature flag — not the route. The owner named the shape wanted instead: the way a
plugin-friendly API layer lets a plugin contribute endpoints (Better Auth's `better-call`
was the reference), so a capability can offer a route without the engine knowing the
capability exists. That is a door on `host`, and like every door it is built when an
extension needs it; the move deletes the coupling and owes no route.

## Why the engine exposes its bag codec

The first firing of the clause in point 3 above — *what an extension needs and the engine does
not yet expose is engine work done inside the extension's own change*. The MoQ wheel's data
tracks (`docs/plan/ARCHITECTURE.md` §Networking) carry a StreamLib bag across a network the
engine does not own: the publisher turns a Python dict into msgpack bytes and hands them to
its own transport, and the subscriber does the reverse. That is a conversion between a bag and
bytes in the caller's hands. It is not a raw byte port — no link reads or writes bytes, and the
typed media path is untouched.

The engine already held the only answer to it. `encode_bag_to_msgpack` and
`decode_msgpack_to_python_object` were `pub(crate)` in the wheel, and one bag-conversion
function — `decode_tapped_channel_bag_frame_to_python_object` — was already a module-level
export, so the shape needed no invention. They became two more:
`encode_bag_to_msgpack_bytes` and `decode_msgpack_bytes_to_python_object`, with exactly the
codec's rules and no new behavior.

**A copy in the extension was the alternative, and it is the parallel abstraction the doctrine
forbids.** A bag's rules — a named map with string keys at every level, eight value types,
`bytes` as msgpack `bin` at 1×, an integer wider than 64 bits refused — are the wire contract,
not an implementation detail of one wheel. A second encoder would answer them a second time, and
the day the two answers disagreed the disagreement would surface as a bag that crossed a link
fine and arrived wrong over the network. One codec is also what lets a non-StreamLib peer read
what a StreamLib node published: it is the same bytes either way.

The cost of exposing it is the cost of any public surface — the two names are now stubtest-gated
and cannot change silently. That is the intended cost. An extension author who cannot reach a
primitive writes their own; the surface is what makes writing their own unnecessary.

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
- **Extension wheels as engine-workspace members** — gates would walk them for free, but they
  would be built unlike any third party's wheel, and the mechanism would never be proven in the
  shape it is meant for.
- **Skip-and-log when a support hook fails** — leaves the engine running with a capability half
  present; the engine's own init hooks already fail hard, and an extension gets no gentler rule.

## Consequences

- The Python surface is the contract for Rust extensions too: a primitive an extension needs
  must exist on the wheel's public surface, typed in the stub, before the extension can use it.
  Known gaps at the time of the pivot: a Python compute dispatch cannot bind a storage buffer,
  and codec sessions are not exported to Python. Each is engine work owed by the first extension
  that needs it.
- A Rust-side extension SDK — typed wrappers over the engine's Python objects, depending on
  `pyo3` and not on the engine — is owed the first time an extension author would otherwise
  hand-write PyO3 against the stub. The first two extensions do not need it: a separately
  compiled wheel cannot downcast the engine's writer object, and the proven manual-source
  shape (Rust receives on its own runtime, a processor-owned thread writes the bag) avoids
  needing to.
- Rust apps with no interpreter cannot load extension wheels; they compile extensions as source
  crates. Python is the binding for optional capabilities, by the owner's statement.
- The support hook runs once per process that takes an engine role — the app process when
  `Runtime()` is constructed, and each helper after the wheel is imported and the log channel
  is up but before the processor's module imports — idempotently, the driver-load shape.
- The placement rule is unchanged: a processor extension's Python class runs in its own helper.
  Whether its native code may ever be called in the app process is OPEN and is the owner's
  ruling to make, never a session's inference.
- `packages/` gains its first live entry with networking; the publish path the plan deferred
  "until the first one wants a home" is now owed by that work.
- `runtime/streamlib-moq` leaves the runtime workspace into the MoQ extension wheel (owner,
  2026-09-04). The one expected exception is a runtime capability the moved code turns out to
  need, which is exposed as engine code — a split of concerns, expected to be rare. WebRTC and
  MoQ are moves of existing code and are the scope; Zenoh is new work and is sequenced
  separately, after them.
- The helper hop's per-frame cost has never been measured; the plan's "fits within the helper
  hop's budget" is an argument, not a number. The first extension should measure it.
- The held networking code pins libraries that have moved — `webrtc`, `moq-transport`,
  `quinn`, `rustls` — and carried patches for TLS and newer MoQ draft versions that may now be
  upstream. The move checks current versions first rather than porting the pins.

## Why the first rung is shaped this way

**The mechanism ships with the wheels that need it, not ahead of them.** A capability-extension
door built before any extension exists would be designed against an imagined consumer. WebRTC and
MoQ each need exactly one thing from the hook — a tokio runtime and a TLS provider brought up once
per process — and that is what the door carries; every other affordance waits for the extension
that asks. The hook's own proof is a test-only distribution installed into the venv, which is the
only honest way to test discovery through pip's registry.

**Publishers take the `Mp4Sink` shape; players take two typed outputs.** One fan-in input with a
track per inbound link is a seam the engine already has — a read that names its link, a port that
lists its links — and it makes a WHIP session or a MoQ broadcast a matter of wiring rather than
config. Players cannot mirror it: output ports are declared statically, and the decoder downstream
wants a port it can name when the graph is wired. So a player exposes one output per media kind
and takes its track names as config; the plan's "one output per track" was narrowed by that fact.

**The MoQ names changed because the shape did.** `MoqPublishTrack` described the old processor,
which published one track and needed one instance per media. A publisher of a whole broadcast is
not that, and a name that says "track" would send a reader looking for a per-track instance.

**The players fill in what the wire does not carry.** RTP has no group index, no extent, no
pre-skip; the old players never had to supply them because their bag types were a different
contract. Taking them from config was rejected: a player that needs the app to tell it the
stream's extent is a player that gets it wrong the day the far end changes resolution. The SPS,
the access unit and the SDP answer already say everything the bag needs.

**The decode-back is the live proof, again.** A PSNR lock through the network is the same argument
the recording rung made through the container: the codec rig already scored the path on both
sides, so anything the wheel does to the bytes shows up as a delta against a baseline that exists.
A liveness check ("frames arrived") would prove much less for the same rig time.

**`runtime/streamlib-moq`'s registry does not move.** `sessions_for_runtime` existed so a dlopen'd
package and the api-server could find one shared session by runtime id. With one processor per
helper and no control-plane route, a session has exactly one owner and the registry has no reader.
