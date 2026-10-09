# examples-show-streams-only

An example shows a person writing and running a stream. It never shows engine work:
embedding the engine in a host process, driving its executor, or wiring its internals.
After this change:
- `examples/tokio-integration` is gone, deleted rather than ported;
- §Consumers says an example shows streams only, and that an engine-facing example is
  deleted rather than converted;
- nothing a stream author writes, and no engine contract, changes.

**Scale gate — this skill, no ADR.** It changes a consumer disposition that §Consumers
decided ("`tokio-integration` is a plain cargo project", conversion backlog). It touches no
RHI, no IPC wire, no processor model and no Python API contract.

**Scope — examples only.** The owner set the scope on 2026-10-09: this change covers examples
that show engine work, not stream work. §Product's "Rust authoring stays a supported capability"
(`ARCHITECTURE.md:71-74`) and every entry naming the `streamlib` crate for Rust apps (`:3614`,
`:3835`) stay as written. Whether Rust app authoring stays a product path is a separate
question. If it comes up, it goes to `/align`, together with the owner's undecided concern
that built-ins should follow the users' path.

**Precondition.** The one entry this change edits is DECIDED: §Consumers `ARCHITECTURE.md:702-729`
(conversion doctrine and backlog). The section is IN-FLIGHT for jpeg-after-the-robotics-cut.
That change edits the held-consumer entry (`:752-764`) and, in this entry's count sentence, only
the held figure (its `:144`). The two edits touch different words, so they can ship in either order.

**Verified against the tree 2026-10-09 (HEAD 986da6f84).**
- `examples/tokio-integration` tracks 7 files: `Cargo.toml`, `README.md`, `.gitignore`, and
  `src/main.rs` (497 lines), `src/sequenced_tick.rs`, `src/sequenced_tick_source.rs` and
  `src/tick_cadence_reporting_sink.rs`. It has its own `[workspace]` root and path-depends on
  `sdk/streamlib-sdk` (`Cargo.toml:20`). It is not a workspace member, so no workspace
  build, CI lane or xtask gate compiles it.
- What it teaches is engine embedding. Its README opens with "the engine is a guest in it"
  (`README.md:3-4`), and its model section is `Runner::new()` adopting the host's tokio
  runtime and the `*_async` graph ops (`README.md:11-30`). It reaches
  `streamlib::sdk::context::RuntimeContextFullAccess`, `ContinuousProcessor`, `MediaClock`
  and `#[streamlib::sdk::processor]` (`src/sequenced_tick_source.rs:6-17`,
  `src/tick_cadence_reporting_sink.rs:7-8`).
- It is already broken against the tree. It does not compile: it calls
  `Runner::start_and_wait_for_shutdown` (`src/main.rs:94`) and `request_runtime_shutdown`
  (`:267`), which no longer exist on `Runner` since #2704 (#2615's comment). #2615 also lists: `tap_async` is handed the old channel
  name (`src/main.rs:155`, `:172`), and `node["display_name"]` is read (`:352`, `:390`) and
  built (`:404-406`).
- `git grep tokio-integration` outside the directory finds `ARCHITECTURE.md:710` and
  `CHANGELOG.md:1089`. The CHANGELOG line is release history and stays. No `.claude/` file,
  workflow, xtask gate, README or doc under `docs/` names it.
- No other example is engine-facing. Two other examples have a `Cargo.toml`: `jpeg-psnr`
  (held, and deleted by jpeg-after-the-robotics-cut) and `screen-recorder` (held on screen
  capture, and mined by that align). Neither is in this change's scope.

## MODIFIED: §Consumers `ARCHITECTURE.md:702-729` — conversion backlog

The backlog list loses its last item and the count drops by one. Spelled:

> … that backlog is executed in full: `audio-mixer-demo`, `microphone-reverb-speaker`,
> `raytracing-showcase` and `fisheye-object-detection` are Python, and `camera-compute-kernel`
> and `camera-halftone` are kernel examples. `examples/` stands at twelve converted beside two
> held. …

Delete the clause "and `tokio-integration` is a plain cargo project". jpeg-after-the-robotics-cut
counts its own deletion of `jpeg-psnr` in the held figure when it ships. This change edits only
the converted count.

## ADDED: §Consumers — an example shows a stream, never the engine

- **DECIDED** — An example shows a person writing and running a stream: the scaffold's idiom,
  built-ins, nodes, links and exposure. It never shows engine work. That means no engine embedded
  in a host process, no driving the executor or its async runtime, and no reaching into engine
  internals. Showing an engine to a stream author is irrelevant to them and misleads them into
  thinking they have to do engine work, the way a Tailscale user would be misled by an example
  of modifying Tailscale. An engine-facing example is deleted, never converted. Engine contracts
  are proven by engine tests, as the first entry in this section states. Owner, 2026-10-09.
  [examples-show-streams-only]
  Rejected: porting `tokio-integration` to the new graph and tap shape (#2615): it keeps alive
  an example of engine embedding that no stream author needs (owner, 2026-10-09).

## REMOVED

- REMOVED: examples/tokio-integration
  The whole directory, all 7 tracked files. The gate checks it as a path. The directory's
  contents are excluded from the content search (`ship-change-removed-gate.sh:58`), so the path
  bullet is the proof.

## MODIFIED: converted examples — wording that strays into the engine

Under the new entry, three passages in stream examples lose their engine-internal wording. Each
example otherwise stays exactly as it is.

- `examples/raytracing-showcase/README.md:162-163`: delete the sentence "(A Rust processor in the
  app process can escalate, at the cost of a device-idle wait per frame.)". It points readers at
  the Rust app path. Native code reaches a stream inside a Python package (§Packages
  `ARCHITECTURE.md:239-246`), and such a package runs in the node's own process under the same
  boundary. No replacement sentence: per-frame acceleration-structure rebuilds would be an engine
  capability for both languages, which the plan does not decide.
- `examples/fisheye-object-detection/README.md:84-86`: "and a dispatch binding resolves it
  through the surface-share service exactly as it resolves a texture from this process's own
  ring" becomes "and a dispatch binds it exactly as it binds a texture from this node's own
  ring". The following sentence, "Nothing is copied…", stays.
- `examples/raytracing-showcase/processors/split_screen_compositor.py:292-295`: the comment
  becomes "Both upstream ids name textures other nodes wrote, and a dispatch binds them as they
  are, the same as this node's own." This third passage has the same surface-share wording as
  the fisheye one.

## Tracker consequences, applied at /derive-tickets

- #2615 drops its `examples/tokio-integration/src/main.rs` bullet. Its other examples are
  still owed.

## Slice

One ticket: delete the directory, apply the §Consumers edits and the three wording trims above,
and fold this change.
Nothing in it needs the rig.

## Assumptions stated, not asked

- **The Rust-authoring entry stays.** That follows the owner's scope note (2026-10-09). If it
  is retired later, that is its own `/align`, and it decides the `streamlib` crate's
  publication and the `:3614` / `:3835` mentions.
- **No smoke check is added.** §Consumers keeps examples out of CI by convention (`:765-775`),
  and this change doesn't revisit that.
