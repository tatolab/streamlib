# The local API is reachable only on its own machine

Rationale for the `[local-api]` entries in `docs/plan/ARCHITECTURE.md` §Control plane &
observability, decided 2026-10-01 under the one-runtime-per-machine pivot.

## Trigger

Read this before letting anything off the machine call the runtime's control surface, before
adding a token, account or login to control, and before treating a URL form's listener as a
second door for changing a stream.

## Decision

Control — observing, inspecting and changing the streams a runtime runs — is served on a local
socket that only the owning user can open, carrying the router and vocabulary that shipped over
TCP. No network address serves control. Watching an exposed port from a browser, ffmpeg or curl is
a separate listener that serves only what is exposed and changes nothing; it listens on loopback
by default and on a LAN or tailnet address when the user asks. Changing a stream on another
machine means running the CLI or an agent on that machine — over ssh, the way Docker is driven
remotely — and a fleet-wide path is the external control client's to offer, never the runtime's.

An MCP host reaches the tools by launching the CLI's `mcp` verb as a stdio server. The verb
forwards messages unchanged between its stdin/stdout and the runtime's MCP endpoint on the socket,
interpreting nothing, so the protocol revision is the runtime's alone. The same verb under ssh is
how an agent reaches another machine. The runtime serves MCP's current revision (2026-07-28,
stateless) and no earlier one.

## Rejected alternatives

- **The runtime accepts control from other machines, authenticated.** It needs identities, tokens
  or certificates, their distribution and revocation, and a per-verb authorization model — a
  security surface the size of a product, for a reach that ssh, Tailscale and a control client
  already give. The plan already leaves peer identity and encryption between machines to them.
- **Keep control on a TCP port, loopback-bound and bearer-gated.** Any local process of any user
  can dial loopback, so it needs a token, a place to keep it and a way to hand it out; a socket's
  file permission is that gate with nothing to hand out. It also leaves a port to discover.
- **Serve control on the URL forms' listener.** That listener is the one a user opens to their LAN
  or tailnet; putting control on it makes "open the forms to my network" mean "let my network
  change my streams".
- **MCP on a loopback HTTP port beside the socket.** Claude Desktop cannot dial a URL from its
  local config, so it reaches fewer hosts than a launched command; it brings back a port to
  discover and a token to hand out; and the MCP spec obliges a localhost HTTP server to validate
  `Origin` against DNS rebinding and to authenticate. Cloud-hosted agents reach loopback under
  neither shape.
- **Interpreting MCP in the verb.** A verb that parsed messages would pin a protocol revision in
  the CLI as well as the runtime; a forwarder lets the runtime move to a new revision alone.

Prior art, checked 2026-10-01: Docker Desktop writes `docker mcp gateway run` into an MCP host's
config; Unity's MCP is a launched program that reaches the running Editor over a Unix socket;
Podman's MCP server is launched and talks to Podman's socket; the main Kubernetes MCP servers
default to stdio over the user's kubeconfig; Heroku and Firebase ship `mcp` as a CLI subcommand.
The MCP transports spec says clients SHOULD support stdio whenever possible.

- **Serving the handshake-based revisions beside 2026-07-28.** Two protocol shapes in one
  endpoint, kept alive for hosts that have not moved; the owner chose the current revision only.

## Consequences

- An agent elsewhere on the network cannot change a stream unless it runs on that machine or
  reaches it through ssh or a control client. Accepted: driving a remote machine was never safe
  under the shipped posture (all interfaces, no auth), and the mesh still carries every exposed
  port to whoever may read it.
- The auth and remote-access question closes for control: file permission is the whole gate, and
  what other machines may read is exposure, decided under §Networking.
- The shipped all-interfaces bind, `--host`, `--port`, `--url` and the mesh's announced control
  URLs are legacy, retired by the local API's rip-out change.
- The "no CLI verb, stdio server, or bridge process" clause of the MCP-served-with-the-node
  decision is reversed; it was written when the tools sat on a network port.
- Several MCP hosts mean several short-lived `mcp` processes against one runtime, which already
  serves several callers.
- Agents hosted in a cloud (claude.ai, a web ChatGPT) are not served by the local API; reaching
  them needs a public endpoint, which is the relay's or a control client's concern.
- A host still speaking only the handshake-based revisions cannot connect until it moves.
  Accepted: MCP hosts update themselves, and the local API serves MCP only once its build lands.
  Checked 2026-10-01: Claude Code 2.1.287 negotiates 2026-07-28 (its changelog sends
  `server/discover` to stdio servers before `initialize`, and defaults to 2026-07-28 negotiation);
  Codex had the revision behind a feature flag at the end of August; Claude Desktop, Cursor and
  VS Code had nothing documented.

## Decided 2026-10-02: the local API speaks the graph's words

The graph's one shape re-spelled what `graph` renders in the stream vocabulary — node, name,
port — and left the tools' arguments to the local API, whose change carried the protocol and not
the words. Shipped as it stood, one conversation would mix two vocabularies: an agent reads
`node: "camera"` in `graph` and types `from_processor_display_name: "camera"` to wire it.

Decision: every argument is spelled as `graph` renders the same thing, so whatever an agent reads
it can type back. The mutation tools are `add_node`, `remove_node`, `connect` and `disconnect`;
a link end is a node and a port, with a runtime name for a port on another runtime, in a tool
and in `graph` alike; the catalog, the prompts and the instructions say node. The re-spelling
ships with the graph's one shape, so `graph` and the tools change together. Owner, 2026-10-02.

Rejected:
- *Keep the tools' spellings until runtime hosting re-spells the verbs.* Ships the mixed
  vocabulary for a whole step of the pivot, with agents written against it in the meantime.
- *Re-spell the words but keep addressing a node by its processor id.* The id is the engine's
  word, and the address a person and an agent use is the name; a node's name is unique in its
  stream, so the id adds nothing a tool needs.

Consequences:
- `add_node` answers the name the node received, since a defaulted duplicate gains a suffix;
  that name is what every later call uses.
- `graph` renders no `processor_id` beside a link end; a node's own `id` stays as a live key.
- A name is unique only within its stream. While a runtime holds one stream the name alone is
  the node; once one runtime hosts several, every tool that names or adds a node also names its
  stream, as the address `<machine>/<stream>/<node>/<port>` does, spelled with the stream
  actions by the change that builds them.
- `runtime_name` stays the remote half of an end until one runtime hosts several streams and the
  address gains its machine and stream.
- The engine's Rust identifiers keep "processor" until the rename step.
