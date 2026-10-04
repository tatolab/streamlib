# zenoh-and-moq-wheel-removal

> **Approved by the owner, 2026-10-04**, as written, with its stated assumptions; the
> stamp-clock deletion (assumption 1) confirmed by name.

The rip-out of the moq-on-the-tailnet pivot (owner, 2026-10-04): the removal that comes first.
It deletes, and re-homes the little `tap` still needs. After this change:
- the tree holds no Zenoh: no session, no runtime mesh, no mesh name, no discovery, no peers, no
  link between runtimes, no surface copied across machines, and no `zenoh` dependency;
- a runtime keeps its name, its local registry row, `tap`, `exchange` and every link inside it;
  the name moves out of the mesh onto the runtime, and `graph` renders it at the top level;
- the MoQ extension wheel, its vendored `moq-transport` and its example are gone; the WebRTC
  extension keeps working, its live script following one `graph` key;
- nothing links one machine to another, and the tree holds no MoQ, until the sharing step.

**Scale gate — this skill, plus the existing ADR** (`docs/decisions/moq-on-the-tailnet.md`). The
Python API's public contract moves (remote references, the `mesh_*` arguments, the stamp-clock
reads), the local API's tools and `graph` move, the CLI loses flags, and a wire is deleted whole.

**Precondition.** Every entry built is DECIDED: §Networking `ARCHITECTURE.md:4195-4203` (MoQ off
the machine; Zenoh removed), `:4204-4211` (nothing Tailscale already does; no stream map),
`:4221-4226` (the moq-dev line; the vendored tree deleted), `:4247-4259` (the removal first, as
one change; `tap` and `exchange` stay until the sharing step), `:3830-3839` (pull-only). Not built
against: everything else `[moq-on-the-tailnet]` — the engine's MoQ endpoint, the tailnet name,
the relay, the HTTP, the viewer page and the OPEN at `:4260` are the sharing step's.

**Verified against the tree 2026-10-04 (HEAD 107acbc24)**, three read-only sweeps; `E` =
`runtime/streamlib-engine`, `A` = `runtime/streamlib-api-server`, `W` =
`sdk/streamlib-python-wheel`.

**The mesh, whole**
- `E/src/core/runtime/mesh/`: 30 files, 13,763 lines — the membership and session
  (`runtime_mesh_membership.rs`, 921), configuration, endpoints, keys, description, peer table,
  observation, the duplicate-name check, host identity, link requests (three files), the offer,
  egress and ingress with their tables, the attachment, the clock identity, and five files
  copying a frame's pixels across. All delete.
- Beside it: `core/runtime/runtime_mesh_configuration.rs`,
  `link_requests_applied_into_this_runtimes_graph.rs` (409),
  `output_ports_in_this_runtimes_graph.rs` (334), `linux/host_identity.rs`,
  `apple/host_identity.rs`; six graph pieces for a link from another runtime (333 lines:
  `edges/links_from_another_runtime.rs`, `edges/link_request_unique_id.rs`, three components,
  `mutation_ops/add_link_from_another_runtime_op.rs`).
- Tests: `E/tests/runtime_mesh_two_processes.rs` (1176), `cross_runtime_links_two_processes.rs`
  (1593), `cross_runtime_link_requests_two_processes.rs` (866), three peers under `tests/bin/`,
  `examples/cross_runtime_link_rig.rs`, eight fixtures (four `verify_cross_runtime_*.sh`,
  `cross_runtime_link_python_node.py`, `mesh_linked_audio_probe.py`,
  `wire_two_runtimes_over_mcp.py`, `tap_a_mesh_address.py`);
  the feature `multi-process-mesh-e2e-tests` (`E/Cargo.toml:34-41`, `:275-316`).
- Dependencies: `zenoh` (`:114-127`), dev `zenoh-keyexpr` (`:236-241`); only they pull quinn,
  rustls, webpki-roots, rcgen, x509-parser and 22 `zenoh-*` crates into the engine.

**Arms in files that stay**
- `core/runtime/runtime.rs` (the membership, the offer, the ingress table, the GPU cell,
  `new_with_runtime_mesh_configuration`, the remote branch of load and connect);
  `operations.rs:179-214` and `operations_runtime.rs:241-497`, `:707-720`, `:853-936` (four trait
  methods, remote connect, remote tap); `runtime_context.rs` (`mesh_link_ingress_table`,
  `hosted_control_plane`, which `A/processors/api_server.rs:175`, `:209` fills).
- The graph: `OnAnotherRuntime` on both link-port refs, `LinkState::AwaitingRemote`,
  `links_from_another_runtime` threaded through `data_structure.rs`, `traversal_source.rs` and
  the query and mutation ops; `graph_snapshot.rs` loading a remote end;
  `iceoryx2/the_clock_an_inbound_links_stamps_are_taken_on.rs` and the `stamp_clock` wiring key
  (`open_iceoryx2_service_op.rs:1490`, `W/python/streamlib/_helper.py:726`).
- Mesh-only and easy to miss: the notifier that is `None` only for a mesh channel
  (`iceoryx2/output.rs:144-146`); `ChannelSizing` public for the cross-runtime fixture
  (`iceoryx2/node.rs:229-237`); `Error::EveryPixelBufferInThePoolIsInUse`
  (`sdk/streamlib-error/src/lib.rs:125`); `core/observability/snapshots.rs:38-41`; Zenoh wording
  in the channel grammar (`iceoryx2/channel_name.rs:44-408`); and two test names pinned in
  `test.yml` and `xtask/src/main.rs` — `capability_extension_and_mesh_rendering_tests`, whose
  extension tests survive under a new name, and
  `channel_max_subscribers_is_the_fixed_cap_plus_its_two_reservations_and_refuses_past_it`.
- `core/json_schema.rs:48-197` (`GraphResponse.mesh`, the peer, egress and request types),
  `:283-446`, `:726-875` (`awaiting_remote_reason`, `created_by_runtime_name`, the remote link
  end); `processor_metrics.rs` and `iceoryx2/loss_counters.rs` (`mesh_hop_dropped_bags_by_link`).
- `iceoryx2/channel_name.rs:247-272` (`mesh_ingress_channel_name`); `open_iceoryx2_service_op.rs`
  (ingress-table parameters, the mesh-only channel, remote naming); `subprocess_bridge.rs`,
  `subprocess_escalate/` (the ingress table, the `inbound_link_stamp_clock_identity` op);
  `RESERVED_MESH_EGRESS_SUBSCRIBER_SLOTS_PER_CHANNEL` (`streamlib-ipc-types/src/lib.rs:331-341`).
- `A/src/mcp.rs:356-379`, `:660-850` (`connect`'s `from_runtime_name` and `to_runtime_name`,
  `disconnect`'s `input_runtime_name` and `link_request_id`, three response keys, the
  instructions' mesh prose); `control_plane_stub_support.rs:96-140`, `:296-331`.
- `W`: `src/python_runtime_mesh_observation.rs`; `python_added_processor.rs:106-200` and
  `python_runtime_lifecycle.rs:597-730` (remote references, four `mesh_*` arguments);
  `_engine.pyi:85-93`, `:550-680`, `:847-948`, `:2338-2430`; `_stream_graph_builder.py:205-235`,
  `:354-360` and `__init__.py:91-94`, `:181-182` (`remote_output`, `remote_input` and their
  reference classes); `cli.py:688-756`, `:952-1090` (the peers table), `:1497-1600` (the flags);
  `tests/test_runtime_mesh_configuration.py` and arms in fourteen other test files, among them
  `test_third_party_notices.py:38-64`, which asserts Zenoh's notice is present.
- `xtask/src/main.rs` (five test-name lists, the rig build, the `--no-run` step);
  `generate_third_party_notices.rs:338-350`; `.github/workflows/test.yml` (ten sites), and
  `STREAMLIB_MESH_MULTICAST_DISCOVERY` in `python-wheel.yml`, `macos-wheel.yml`,
  `release-extension-wheel.yml`; `deny.toml:51-56`, `about.toml:17-18`, `vendor/zenoh-notice/`.

**Mesh-shaped: what stays, and what does not**
- **The runtime name** lives in `RuntimeMeshMembership`; the registry, `--node`, `tap`'s channel,
  MCP `connect` and `graph` all read it there. Only the mesh refuses a duplicate name today.
- **`MeshPortAddress`** (`core/graph/edges/mesh_port_address.rs`, 273) and
  `core/runtime/mesh_address_chunk.rs` are what `tap` parses its channel with
  (`open_iceoryx2_service_op.rs:875-910`, whose local arm needs only the parser and the runtime
  name); the chunk grammar's tests use `zenoh-keyexpr` as their oracle. They stay, renamed.
- **`MachineClockIdentity`** is read by `json_schema`, `iceoryx2/input.rs`, the Mp4 writer in
  `streamlib-media-builtins`, and `W`'s two stamp-clock functions — every use is about another
  machine's stamps, and nothing else is built on it. It goes (below).
- **`graph`'s `mesh.runtime_name`** is how four engine fixtures, the WebRTC live script,
  `A/src/mcp_prompts.rs:602`, the `tap` tool's description (`A/src/mcp.rs:285`),
  `E/tests/graph_snapshot_round_trip_test.rs:276`, `W/tests/test_live_graph_mutation.py:201` and
  three skills spell a `tap` channel.
- `ResolvedSurfaceBacking` is shared with the exchange and the surface copy and stays.

**The MoQ wheel**
- `packages/streamlib-moq/`: 143 tracked files — `src/` 16.1k lines, `python/` 1.7k, tests 4.2k,
  `vendor/moq-transport/` 98 files and 25.1k. `examples/moq-broadcast-roundtrip/` depends on it.
- Named by: `xtask/src/check_vendored_trees.rs:37-40`, `:136-137` and three other xtask sites;
  `scripts/check-license-headers.sh:71-87` and its CI-run test
  `.claude/scripts/tests/license-header-gate.test.sh:167-193`, which plants a file under the
  vendored path and expects the exclusion; `python-wheel.yml:208-209`, `:329-330`;
  `release-wheel.yml` (four sites); `release-please.yml`; `release-please-config.json`;
  `.release-please-manifest.json`; `scripts/build_simple_index.py:31` (its test asserts the tuple
  equals the wheels under `packages/`); `docs/architecture/vendored-moq-transport.md`;
  `.gitignore:63`; `verify-live`'s networking arm; and the licence carve-out lines in
  `CLAUDE.md:17-18`, `.claude/rules/licensing.md:15-16`, `.claude/agents/review-pr.md:52-53`.
- The WebRTC wheel shares no code with it, only the workflows' matrix and the index discovery.

---

## MODIFIED: §Networking — Zenoh and the runtime mesh are gone

- Everything under **The mesh, whole** is deleted, and every arm under **Arms in files that
  stay** with it. No Zenoh call, type, key, feature or dependency remains; `Cargo.lock` loses the
  closure, and `THIRD-PARTY-NOTICES.md` is regenerated.
- **A link's ends are both on this runtime.** `OutputLinkPortRef` and `InputLinkPortRef` lose
  their other-runtime variant, `LinkState` loses `AwaitingRemote`, and the graph loses the second
  link collection, so every traversal walks one. A snapshot whose link end names a runtime is
  refused by name at load: built here with its test, since the loader ignores unknown keys
  today and would load that end as a local one (`json_schema.rs:343-344`).
- **Python.** `Runtime.remote_processor_output`, `remote_processor_input`, the builder's
  `remote_output` and `remote_input`, all four remote reference classes and the four `mesh_*`
  keyword arguments are deleted, with their stub entries; `Runtime()` keeps `runtime_name`.
- **The CLI.** `run` and `dev` lose `--mesh-name`, `--mesh-peer`, `--mesh-listen` and
  `--no-mesh-multicast-discovery`; `nodes` loses its three and prints the registry table
  alone. `STREAMLIB_MESH_*` is read nowhere; `STREAMLIB_RUNTIME_NAME` and `--runtime-name` stay.
- **One reserved subscriber slot per channel** remains, `tap`'s; `channel_max_subscribers` and
  the tests that sum it follow.
- **The surface exception ends.** The engine reads no bag key for a link; the audio window
  contract is the one carve-out left (§Processor model).

## MODIFIED: §Networking — what stays, re-homed

- **The runtime name** is a field of `Runner`, resolved as today (`runtime_name.rs`,
  `app_directory.rs`, `stated_configuration_value.rs` keep their non-mesh halves).
  `this_runtimes_name_on_the_mesh` becomes `this_runtimes_name`.
- **The address type** is renamed for what it is: `mesh_port_address.rs` → `port_address.rs`
  (`PortAddress`), `mesh_address_chunk.rs` → `address_chunk.rs`. The chunk grammar is unchanged
  and its tests assert against the rule's own table, the `zenoh-keyexpr` oracle going with the
  dependency. `tap` resolves a channel naming this runtime only; another runtime's name is
  refused by name.
- **The stamp-clock read surface is deleted**: `stamp_clock_identity` on a link in `graph`,
  `inbound_link_stamp_clock_identity`, `this_machines_stamp_clock_identity`, the escalate op
  behind the first, the `stamp_clock` wiring key, `MachineClockIdentity` and its platform reads,
  and `Mp4Sink`'s another-machine refusal. Every stamp in the tree is this machine's until the sharing step,
  whose OPEN owns how a stamp from another machine is marked (assumption 1).

## MODIFIED: §Control plane & observability — `graph`, the tools, the fixtures

- **`graph`** loses `mesh` and gains `runtime_name` at the top level, in the OpenAPI schema, the
  MCP tool, the generated schema and the prompt fixture. A link renders `{node, port}` at both
  ends and loses `awaiting_remote_reason`, `created_by_runtime_name` and `stamp_clock_identity`;
  a node's metrics lose `mesh_hop_dropped_bags_by_link`.
- **`connect`** takes `from_node`, `from_port`, `to_node`, `to_port` and answers a `link_id`;
  **`disconnect`** takes a `link_id`. The instructions drop the mesh states.
- **`tap` and `exchange` are unchanged.** `tap`'s channel stays `<runtime_name>/<node>/<port>`;
  the name is read from `graph`'s top-level `runtime_name`. Following that key:
  `A/src/mcp_prompts.rs:602`, `cli.py:1632`, `E/tests/fixtures/e2e_fixture_psnr.sh`,
  `e2e_fixture_psnr_vivid.sh`, `e2e_fixture_recording.sh`, `verify_audio_channel.sh`,
  `packages/streamlib-webrtc/tests/live/whip_whep_roundtrip.sh`.
- **`docs/testing-hardware.md`** loses the multi-process mesh tier; **`README.md:188`** loses
  "on the runtime mesh".

## MODIFIED: §Packages, §Distribution — the MoQ wheel is gone

- `packages/streamlib-moq/` and `examples/moq-broadcast-roundtrip/` are deleted whole, with
  `docs/architecture/vendored-moq-transport.md`.
- `check-vendored-trees` guards the vulkanalia fork alone; the licence-header script loses its
  exclusion and its test loses the MoQ cases in the same PR, the test being CI-run; the
  extension-wheel matrix in `python-wheel.yml` and `release-wheel.yml`, the
  release-please package entry and manifest line, and `build_simple_index.py`'s published names
  hold `streamlib-webrtc` alone. `release-extension-wheel.yml` is generic and stays.
- `verify-live`'s networking arm keeps the WHIP and WHEP fixture and loses the MoQ one.
- Wording only: "MoQ-mappable" in the engine's encoded-frame docs.

## MODIFIED: the in-flight changes and their tickets

Each file carries its dated amendment already (the pivot's plan PR). What that means per ticket
is the tracker batch's to apply once this change is approved:
- **local-api** — #2578 has nothing left to edit: this change deletes the announcement whole.
- **package-split-and-lend** — #2591 renames the WebRTC wheel alone; #2592 and #2593 delete no
  mesh test or observation file, those being gone.
- **runtime-hosting** — #2604 keeps no mesh membership and no per-machine ingress; #2605 writes
  no mesh settings, takes no `set --mesh-*` flag and renders no `graph.mesh`; #2607 builds the
  address's machine part with no Zenoh token, claim or suffix.
- **stream-graph** — #2566 is already rewritten against the three levels on one machine.

## Companion PRs

- **Operating model** (dedicated, per the flow rule): skills `inspect-live-graph`,
  `tap-live-channel` and `verify-live` read `graph`'s top-level `runtime_name`;
  `discover-running-nodes` drops the mesh peers table and `verify-live` the MoQ arm. It lands
  after S3 and before the ship gate, which searches `.claude/`.
- **The licence carve-out.** `CLAUDE.md` §Licensing, `.claude/rules/licensing.md` and
  `.claude/agents/review-pr.md` name `packages/streamlib-moq/vendor/moq-transport`. Those lines
  are the owner's to change through `/propose-rule`; until they are, the bullets below that
  name the wheel or `moq-transport` cannot pass the gate. Nothing else here waits on it.

## Left to later changes

| Not here | Because | Lands with |
|---|---|---|
| The engine's MoQ endpoint, the relay, the HTTP and the viewer page | their OPEN | the sharing step |
| A link to another stream or machine; `awaiting` states for one | runtime-hosting; the sharing step | steps 4 and after 5 |
| `tap`, `exchange`, the reserved tap slot | the snapshot form must exist first | the sharing step |
| The machine part of an address; reading Tailscale's status | runtime-hosting (#2607) | step 4 |
| `runtime_name`, the registry, `--node` | runtime-hosting | step 4 |

## Assumptions stated, not asked

1. **The stamp-clock surface goes rather than answering "this machine" for ever.** Nothing in
   the tree can produce another machine's stamp after this change, and the doctrine bans a
   method that can only say one thing. It bites if the sharing step wants the same spelling
   back: it re-adds what its align decides.
2. **`graph` keeps the runtime name**, moved to the top level, because `tap`'s channel is spelled
   from it and `tap` stays. It goes with the runtime name at step 4.
3. **`PortAddress`** is the address type's name until runtime hosting gives it four parts.
4. **Nothing refuses a duplicate runtime name any more.** The mesh's check was the only one. Two
   runs from one directory both start; a `--node` name matching two registry rows is refused
   naming both, added here if the resolver does not already do it.
5. **`examples/moq-broadcast-roundtrip` is deleted**, not converted: its one purpose was the
   wheel.
6. The cross-runtime fixtures and rigs are deleted, never rewritten for one machine; links
   inside a runtime are already covered.
7. **The package index stops listing `streamlib-moq`.** Dropping it from
   `build_simple_index.py`'s published names removes the index page for the MoQ wheels already
   released; the release assets themselves stay on their GitHub releases.

## Expected slices

- **S1 — the MoQ wheel.** The package, the example, the vendored-tree gate entry, the
  licence-header exclusion with its test, the workflow matrix, release-please, the index names,
  the provenance doc. Independent.
- **S2 — links between runtimes.** Remote references in Python, MCP and the graph, link
  requests, `awaiting_remote`, the offer, ingress and egress, the surface copy across, hop-loss
  counts, the stamp-clock surface, remote `tap`, the two cross-runtime suites, their rig and
  fixtures; the address type's rename. Independent of S1.
- **S3 — the session.** The membership, discovery, peers, the duplicate-name check, host
  identity, the configuration and its flags and variables, `graph.mesh` giving way to
  `runtime_name`, the peers table, the observation session, the mesh suite and its feature, the
  `zenoh` dependencies, notices, CI. Blocked by S2.
- **S4 — the operating-model PR.** Blocked by S3.

## REMOVED

- REMOVED: zenoh
- REMOVED: Zenoh
- REMOVED: runtime/streamlib-engine/src/core/runtime/mesh
- REMOVED: vendor/zenoh-notice
- REMOVED: runtime_mesh
- REMOVED: RuntimeMesh
- REMOVED: mesh_name
- REMOVED: STREAMLIB_MESH_
- REMOVED: --mesh-name
- REMOVED: --mesh-peer
- REMOVED: --mesh-listen
- REMOVED: --no-mesh-multicast-discovery
- REMOVED: multi-process-mesh-e2e-tests
- REMOVED: MeshPortAddress
- REMOVED: mesh_port_address
- REMOVED: mesh_address_chunk
- REMOVED: mesh_ingress_channel_name
- REMOVED: MeshLinkIngress
- REMOVED: mesh_link_ingress
- REMOVED: mesh_port_egress
- REMOVED: mesh_hop_dropped_bags_by_link
- REMOVED: MeshHopDroppedBagCountsByRemoteInboundLink
- REMOVED: RESERVED_MESH_EGRESS_SUBSCRIBER_SLOTS_PER_CHANNEL
- REMOVED: this_runtimes_name_on_the_mesh
- REMOVED: _observe_the_runtime_mesh
- REMOVED: mesh_peer_endpoints
- REMOVED: mesh_listen_endpoints
- REMOVED: mesh_multicast_discovery
- REMOVED: hosted_control_plane
- REMOVED: capability_extension_and_mesh_rendering_tests
- REMOVED: mesh_linked_audio_probe
- REMOVED: EveryPixelBufferInThePoolIsInUse
- REMOVED: ["mesh"]
- REMOVED: 'mesh'
- REMOVED: mesh.runtime_name
- REMOVED: links_from_another_runtime
- REMOVED: add_link_from_another_runtime
- REMOVED: OnAnotherRuntime
- REMOVED: on_another_runtime
- REMOVED: LinksFromAnotherRuntime
- REMOVED: RemoteLinkResolution
- REMOVED: RemoteInboundLink
- REMOVED: AwaitingRemote
- REMOVED: awaiting_remote
- REMOVED: link_request
- REMOVED: request_link_on_remote_input_runtime
- REMOVED: request_disconnect_on_remote_input_runtime
- REMOVED: created_by_runtime_name
- REMOVED: from_runtime_name
- REMOVED: to_runtime_name
- REMOVED: input_runtime_name
- REMOVED: reader_runtime_names
- REMOVED: egress_ports
- REMOVED: remote_processor_output
- REMOVED: remote_processor_input
- REMOVED: RemoteProcessorOutputPortReference
- REMOVED: RemoteProcessorInputPortReference
- REMOVED: RemoteNodeOutputPortReference
- REMOVED: RemoteNodeInputPortReference
- REMOVED: remote_output
- REMOVED: remote_input
- REMOVED: cross_runtime_link
- REMOVED: tap_a_mesh_address
- REMOVED: wire_two_runtimes_over_mcp
- REMOVED: duplicate_runtime_name
- REMOVED: HostIdentity
- REMOVED: host_identity
- REMOVED: stamp_clock
- REMOVED: StampClock
- REMOVED: StampsAreTakenOn
- REMOVED: MachineClockIdentity
- REMOVED: machine_clock_identity
- REMOVED: packages/streamlib-moq
- REMOVED: streamlib-moq
- REMOVED: streamlib_moq
- REMOVED: moq-transport
- REMOVED: moq_transport
- REMOVED: docs/architecture/vendored-moq-transport.md
- REMOVED: examples/moq-broadcast-roundtrip
