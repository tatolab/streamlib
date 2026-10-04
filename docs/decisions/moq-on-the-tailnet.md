# MoQ on the tailnet: a simple stream SDK for Tailscale users, and Zenoh removed

Rationale for the `[moq-on-the-tailnet]` entries in `docs/plan/ARCHITECTURE.md` and the
2026-10-04 vocabulary in `docs/plan/GLOSSARY.md`, from the owner's pivot of 2026-10-04. The
owner confirmed five sentences verbatim after a grilling of eleven questions.

Annotated in place: `one-runtime-per-machine.md` (its sentences 2 to 4 and its sequence),
`runtime-mesh.md`, `runtime-hosting.md` and `exposure-levels.md`.

## Trigger

Read this before any of the following:
- carrying data between machines over anything but MoQ on QUIC, or adding a second transport;
- building discovery, machine naming, peer identity, access rules or wire encryption that a
  tailnet already provides;
- serving live media over HTTP, WebSocket or any TCP path, in any browser;
- writing a relay, an account, a team directory or a signed link into the open client;
- scoping a feature by what a robot, a vehicle or a ROS stack needs.

## The direction, verbatim (owner-confirmed)

1. Tatolab is a simple SDK for defining live streams as pipes and sharing them with people and
   agents; robotics and physical-machine systems are out of scope, and we never replace a
   team's own ROS or Zenoh stack.
2. Tailscale users are the first customers, and Tatolab builds nothing Tailscale already does —
   reach, machine names, encryption, identity and discovery come from the tailnet, and without
   one a machine shares only with itself and through a relay.
3. On one machine streams share data through shared memory; real-time data that leaves a
   machine travels over MoQ on QUIC, served by each machine's engine on its tailnet address and
   offered through a relay the machine has joined when an internet address is wanted.
4. HTTP carries only what is not real time — listing exposed ports, a read-only MCP, snapshots,
   small samples and the viewer page — never live media, in any browser.
5. Zenoh and the MoQ extension wheel are removed now; the exposure levels and pull-only links
   decided on 2026-10-04 (PR #2624) stand unchanged.

## Decided

- **No tailnet, no direct reach.** A machine with no Tailscale runs every stream and shares
  between streams on itself, and can still join a relay. It cannot be read directly by another
  machine. Nothing is built for a plain LAN.
- **The internet is a machine setting.** Joining a machine to a relay offers every public port
  of that machine there, sent only while someone subscribes. There is no fourth exposure level
  and no per-port internet flag. Who reads at the relay is the relay's access rules, as who
  reads on the tailnet is Tailscale's.
- **The open client joins any relay from an address and a credential,** and knows nothing else
  about relays. No relay is written here; a person may run an existing open-source one.
  Accounts, a team directory, signed links, hosted relays and billing belong to a separate
  private service.
- **The engine stands on the moq-dev line** (`moq-net`, `moq-tokio`) at exact pinned versions
  and serves MoQ itself. The vendored draft-16 `moq-transport` is deleted, not repaired.
- **A machine's name is its tailnet name.** The engine reads the local Tailscale's status for
  its own name and to list the tailnet's machines. With no Tailscale the name is the hostname
  and matters only locally. The name-claiming rules built for the mesh are deleted.
- **Who may read a public port on the tailnet is Tailscale's access rules.** The engine adds no
  per-reader check, so the stream map and peer authentication are deleted.
- **`tatolabd` sets up `tailscale serve`** for its HTTP listener on a port of its own the first
  time a port goes public, and says the one command to run when Tailscale refuses for lack of
  rights. It never alters any other serve setting.
- **The viewer page ships with the first version of sharing,** built on the stack's own
  player. One path for every browser: QUIC and MoQ. No TCP fallback is built or enabled.
- **The repo's live verification reads a port the way a user does.** The fixture stream
  exposes the port private, and the check fetches exact, full-resolution snapshots from the
  machine's local HTTP listing. `tap` and `exchange` therefore stay until the sharing step
  builds the snapshot and sample forms, and are deleted in that change, not in the removal.
- **The WebRTC extension is untouched.**
- **Order.** The removal of Zenoh and the MoQ extension wheel comes first. The one-runtime-per-machine sequence continues through
  runtime hosting, then accelerators optional, then one sharing step that replaces that
  sequence's steps 8 and 9, then resources, packs and the app.

## Open

Each is the sharing step's align to decide; none is built against until then.

- How groups are cut for data that is not video, without the engine reading a bag.
- What a public port whose bags name a surface serves off the machine.
- The URL grammar, the forms that survive sentence 4, and the certificate a browser is shown.
- How a relay is joined, and how a machine's ports are named there.
- The read-only MCP: its tools and who may call it.
- What a control client hands the runtime beyond a relay address and a credential.
- The verbs that list machines and their public ports.
- Which versions of `moq-net` and `moq-tokio` are pinned; the change that builds the endpoint
  names them.

## Why

- **The product had become a parallel robotics stack.** Discovery, naming, peer authentication,
  a pushed policy map, a router role and NAT answers were each being designed for machines the
  owner no longer targets. Robotics teams already run ROS and Zenoh and build inside their own
  engines.
- **A tailnet fills the gaps that made MoQ costly a week earlier.** MoQ has no discovery, no
  names, no peer-to-peer reach and needs certificates. On a tailnet: status lists machines,
  MagicDNS names them, WireGuard authenticates and encrypts every address, and
  `tailscale serve` terminates HTTPS. What is left for the engine is carrying data.
- **TCP is wrong for live media.** One lost packet stalls everything behind it and stale data is
  retransmitted. The owner chose QUIC for everything real time, without waiting for a
  measurement, and keeps HTTP for what is not.
- **One transport off the machine.** Zenoh between machines plus MoQ for browsers meant two
  network stacks, a translation between them at a relay, and two security passes.
- **The IETF line could not serve from inside the engine.** As researched on 2026-10-04:
  `cloudflare/moq-rs` speaks draft-16 only while the working group is at draft-22, serving
  means running its separate relay program, a late or superseded group ends the whole
  subscription, and browser players stop at draft-18. The moq-dev line embeds a server, skips a
  stale group and continues, negotiates its own dialect and the IETF drafts 14 to 22, and ships
  a maintained browser player. Its costs are accepted: one maintainer, frequent releases and
  two crate renames this year, contained by exact pins; and its compatibility with
  Cloudflare's hosted relays is claimed upstream and unverified here.
- **A relay is the only internet path for live data.** Tailscale Funnel carries TCP only.

## Rejected alternatives (by the owner)

- **Direct reach on a plain LAN,** by address or with discovery. Either makes the engine mint
  and pin certificates, find machines and decide who may read — the work just cut.
- **A fourth exposure level, or a per-port share to the relay.** One more state on every
  listing, for a distinction the relay's own access rules already draw.
- **Relay joining only through an account.** It makes the internet edge unusable without the
  hosted service; self-hosting is how adoption stays cheap.
- **Staying on the IETF line** and repairing the vendored copy. See Why.
- **Keeping Zenoh until MoQ lands,** or keeping the MoQ extension wheel until then. Open work in
  the earlier steps would carry mesh code and a draft-16 wheel into the new package shape only
  to delete them.
- **A machine name that is merely whatever resolves,** with the engine never asking Tailscale.
  Tatolab could then not answer which machines and ports can be reached.
- **A documented one-liner for `tailscale serve`,** always run by hand. Every new user would
  meet a manual step before a browser shows anything.
- **A WebSocket fallback for Safari and iOS.** The stack's player keeps WebKit on one; the owner
  ruled for a single path so there is one thing to maintain.
- **A test-only node writing frames to disk,** so that `tap` and `exchange` could go at once.
  Every fixture would test a graph with one more node in it, and nothing in the repo's tests
  would exercise the path a user or an agent reads through.

## Consequences

- **Between the removal and the sharing step, nothing links one machine to another,** and no
  MoQ exists in the tree. Everything on one machine keeps working.
- **This reverses two earlier rulings.** 2026-09-28 and 2026-09-30: Zenoh between machines and
  MoQ only as a browser form. 2026-10-02, said in a session and written in no plan entry:
  Tailscale is never a requirement — reversed for direct reach between machines, and only for
  that.
- **HTTP forms that carry live media are gone:** MPEG-TS, HLS, raw H.264 and a WHEP form are not
  built. A tool that wants live data speaks MoQ or runs a stream.
- **The relay stops being a role of the runtime.** It is a separate program the machine joins.
  The bundled `moq-relay-ietf` is not built.
- **The sequence in `one-runtime-per-machine.md` changes:** its step 8 dissolves, its step 9 is
  reshaped, and both become the one sharing step after accelerators optional.
- **`tap` and `exchange` outlive the removal.** The local-API and native-CLI steps carry both
  verbs into their new shapes before the sharing step deletes them.
- **Safari and iOS viewers depend on WebKit's QUIC support holding up in long sessions.** As of
  2026-10-04 the player's own default keeps WebKit on a WebSocket fallback, citing stalls.
