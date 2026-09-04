# networking-extension-wheels

The first rung of the extension model and the whole of §Networking's decided scope: the
capability-extension mechanism built, and WebRTC and MoQ shipped as the first two extension
wheels — `streamlib-webrtc` and `streamlib-moq` under `packages/` — by moving the code the
tree already holds (`docs/plan/ARCHITECTURE.md` §Packages & extension model `:79-146`,
§Networking `:1737-1821`, §Control plane `:1881-1883`). Zenoh is later work and not here.

**Scale gate — this skill, plus the ADR.** New behavior on the Python API's public contract
(the stubtest-gated class a support hook receives, the `streamlib.extensions` entry-point
group), a control-plane contract change (`graph` gains a third top-level key), and a change
to the helper's startup sequence. `docs/decisions/extension-model.md` gains one section.

**Precondition.** §Networking is DECIDED (`:1737`, flipped to IN-FLIGHT by this change). The §Packages entries this touches — the
direction (`:79`), the two mechanisms (`:87`), the criterion (`:106`), the mechanism as
decided (`:115`), standalone wheels (`:135`) — are DECIDED; its three OPENs (zero-copy
per-frame `:202`, engine-grade capability reach `:146`, app-process extension native code
`:150`) are untouched. §Control plane's "no optional routes" (`:1883`) and §Consumers' held
list (`:349`) and conversion doctrine (`:313`) are DECIDED.

**Verified against the tree 2026-09-04 (HEAD f7114883f)** — two read-only sweeps during the
align, re-anchored today.

- No entry-point or plugin machinery exists in the wheel; the one `importlib.metadata`
  call in the tree is a licence assertion in a test. The engine's sealed `RuntimeInitHook`
  (`runtime/streamlib-engine/src/core/runtime_hooks.rs:34-96`, zero registrations, fails
  runtime creation on error) is the shape precedent, not a mechanism a wheel can reach.
- The two call sites exist. App process: `Runtime.__init__`
  (`sdk/streamlib-python-wheel/python/streamlib/__init__.py:135-139`) already exists solely
  to add a lifecycle side effect. Helper: `_helper.py:771-775` — `bridge.start_reading()`,
  then `log.install_helper_process_sink(...)`, then `load_processor_class(import_path)`; the
  wheel is imported before any of it by CPython's package semantics (`-m streamlib._helper`).
  The helper's registration budget is 60 s (`python_helper_process_spawn_host.rs:42`); its
  teardown reply and exit are bounded at 5 s each (`:45`, `:53`).
- `GraphResponse` is two fields, `nodes` and `links` (`core/json_schema.rs:29-34`), derived
  for `utoipa` and `schemars`. A processor renders by import path; a `@processor` class from
  an installed distribution passes `classify_processor_class` unchanged
  (`python_runtime_lifecycle.rs:55-57`, predicate `python_processor_declaration.rs:62-67`).
- The publish path is single-project by construction: `scripts/build_simple_index.py:26`
  `PUBLISHED_PROJECT_NAME = "streamlib"` filters every other wheel out (`:73-74`);
  `release-please-config.json:5-24` has one package, `"."`, whose bump drives the root
  `Cargo.toml` and the wheel's `pyproject.toml`; the index regenerates from the release
  asset list on GitHub Pages (`release-wheel.yml:246-283`). The wheel is `abi3-py310` on
  `pyo3 0.29` (`sdk/streamlib-python-wheel/Cargo.toml:39`), `requires-python >= 3.10`.
- `runtime/streamlib-moq` is a live workspace member (root `Cargo.toml:23`) on
  `moq-transport 0.14.1`, `quinn 0.11`, `rustls 0.23` (`ring`), `rustls-native-certs`;
  `cargo check --all-targets` is green; its 11 tests run in no CI job. Its only dependent is
  the api-server's `moq` feature (`runtime/streamlib-api-server/Cargo.toml:28-34`,
  `default = []`, enabled by nothing), serving `/api/moq/catalog` (`handlers.rs:144,
  393-401`) off a process-global `RUNTIME_SESSIONS` registry (`moq_session.rs:499-527`).
  It is publishable (no `publish = false`) and named at `deny.toml:159`.
- `packages/webrtc` (3,760 lines) and `packages/moq` (473) have not built since the plugin
  SDK was deleted: both depend on `streamlib-plugin-sdk` / `-abi` 0.16.0, crates that no
  longer exist, through a `_generated_` shim nothing generates. `webrtc_whip.rs` consumed
  encoded video + audio bags; `webrtc_whep.rs` emitted them, depacketising H.264 with
  `streaming/h264_rtp.rs` (304 lines, unit-tested, no engine dependency); both clients own
  their signalling (`whip_client.rs:472-742`, `whep_client.rs:231-393`) on `webrtc 0.14`
  and `hyper`/`hyper-rustls`. `streaming/session.rs` (1,161 lines) is a second RTP path
  constructed nowhere. The MoQ pair forwarded opaque msgpack envelopes with `read_raw` /
  `write_raw`, which the Python surface does not offer (`_engine.pyi:625-686`), and
  restamped on receive. Their bag types share no key with today's wire contract.
- What a player owes the wire: `encoded_video_frame.rs:146-159` requires `is_sync_point`,
  `group_index`, `sequence_index`, `width`, `height` and refuses a missing one by name; the
  audio bag requires `pre_skip`, `sample_rate`, `channels`, `sample_count`. RTP carries
  none. Writing a bag literal with `bytes` → msgpack `bin` and an explicit stamp is proven
  (`test_encoded_video_frame_cast.py:112-126`, `_engine.pyi:678-686`), and the
  manual-source shape — a processor-owned thread writing through `ctx.outputs` — is proven
  (`capability_context_probes.py:188-213`, `test_capability_contexts.py:152-159`).
- `Mp4Sink` is the many-track precedent: one fan-in input `tracks`, one track per inbound
  link named by channel (`mp4_sink.rs:41-47`), `read_from_inbound_link` and
  `inbound_link_names` on the stub (`_engine.pyi:642-664`).
- A helper-placed encoded link's ceiling is 16 MiB (`streamlib-ipc-types/src/lib.rs:43`);
  a bag over it is dropped at `debug`, not raised (`python_processor_link_data_access.rs:
  494-500`). An access unit at 2.5 Mbps is ~10 KB.
- Every past live proof used public Cloudflare endpoints; no relay or loopback exists in
  the tree. `examples/whep-player` is a printed deferral at HEAD.

No `[NEEDS DECISION]` remains: the align settled the forks, and the two places this delta
narrows a decided clause are MODIFIED entries with the fact that forced them.

---

## ADDED: §Packages — the mechanism, as built

- **DECIDED** — The support hook's contract, spelled. A wheel declares
  `[project.entry-points."streamlib.extensions"] <name> = "<module>:load"`; the engine
  reads `importlib.metadata.entry_points(group="streamlib.extensions")` and calls each
  `load(host)` once per process taking an engine role — from `Runtime.__init__` in the app
  process, and from `_helper.py` between the log sink's installation and the processor
  class's import. `host` is `streamlib.CapabilityExtensionHost`, a `#[pyclass]` with a
  stub entry: `role` (`"app"` or `"helper"`) and `register_capability(name, version)`. In
  the app process a registration lands on the runtime and renders in `graph`; in a helper it
  is recorded for the extension's own reads. A hook that raises fails `Runtime()` with the
  distribution named; in a helper it fails that processor's start through the log channel
  and the parent refuses the processor by name, inside the existing 60 s budget. A second
  registration of one capability name refuses at the second hook, naming both
  distributions. `GraphResponse` gains `extensions: [{name, version, distribution}]`, a
  third top-level key, in the OpenAPI schema and the MCP `graph` tool alike. No opt-out.
  Discovery and the loop are Python; the runtime-side registry and the `graph` key are the
  one engine change. [networking-extension-wheels]
- **DECIDED** — The mechanism's own proof is GPU-free and CI-run: a test-only distribution
  under the wheel's tests, installed into the venv, whose entry point registers a capability
  and whose second variant raises — proving discovery, the app-process and helper call
  sites, hard-fail by name, duplicate refusal, and the `graph` key, with no network and no
  device. [networking-extension-wheels]

## ADDED: §Networking — `streamlib-webrtc`

- **DECIDED** — `packages/streamlib-webrtc/`: a standalone maturin project — own
  `Cargo.toml` (`[workspace]` root, `[lib] name = "_native"`, `crate-type = ["cdylib"]`,
  `pyo3` on `abi3-py310`, `webrtc 0.14`, `tokio`, `hyper` + `hyper-rustls`, `rustls`,
  `bytes`; no engine crate), own lockfile, `pyproject.toml` depending on `streamlib` by
  version, `python/streamlib_webrtc/` with `_native.pyi` and `py.typed`. `src/` carries the
  mined WHIP and WHEP clients, `h264_rtp.rs` with its tests, and `rtp.rs`'s sample
  conversion; `session.rs` and `RtpTimestampCalculator` are left dead. `extension.py:load`
  brings up the tokio runtime and the rustls provider once and registers `webrtc`. The
  move checks current versions of `webrtc`, `hyper-rustls` and `rustls` first, as §Networking
  directs. [networking-extension-wheels]
- **DECIDED** — `WhipPublisher`: `@processor`, one fan-in input `tracks` (`ordered`), the
  `Mp4Sink` shape — each inbound link is one RTP track, video or audio by the bag's `codec`,
  the session's SDP built from the links `inbound_link_names` reports at `setup()`; config
  `url` and optional `bearer_token`. `process()` reads by inbound link and hands the
  bitstream and stamp to `_native.WhipSession` directly. `WhepPlayer`: `@processor(execution
  = "manual")`, outputs `encoded_video` and `encoded_audio`, config `url` and optional
  `bearer_token`; `setup()` opens `_native.WhepSession`, `start()` hands `ctx.outputs` to a
  processor-owned thread that drains the session and writes bag literals — extent from the
  SPS, `group_index` advancing on each IDR and `sequence_index` within it, `is_sync_point`
  from the access unit, Opus parameters from the SDP answer, the stamp from the RTP clock
  mapped onto the monotonic clock — and `stop()` closes the session inside the 5 s budget.
  [networking-extension-wheels]

## ADDED: §Networking — `streamlib-moq`

- **DECIDED** — `packages/streamlib-moq/`: the same standalone shape on `moq-transport`,
  `quinn`, `rustls` and `rustls-native-certs`, with `src/moq_session.rs` and
  `src/moq_catalog.rs` moved from `runtime/streamlib-moq` — the process-global
  `RUNTIME_SESSIONS` registry and `sessions_for_runtime` do not move, since one processor
  owns one session — and the relay URL becomes config, carrying the relay's auth token in
  its path, with Cloudflare's draft-16 relay as its default. `extension.py:load` brings up
  the runtime and registers `moq`. The version check this clause asked for was made
  (2026-09-04): `moq-transport` 0.16.2, the draft-16 revision, because Cloudflare deploys
  draft-16 and it carries the acknowledgement and namespace machinery draft-14 lacks —
  owner ruling, superseding this clause's original draft-14 default. Draft-16 requires
  authentication, so no credential-free public relay remains.
  [networking-extension-wheels]
- **DECIDED** — `MoqBroadcastPublisher`: `@processor`, one fan-in input `tracks`, one MoQ
  track per inbound link named by its channel, the catalog derived from them; config
  `relay_url` and `broadcast` (default `streamlib/<runtime_id>`). A bag's `is_sync_point`
  opens a subgroup whose MoQ group id is the bag's `group_index`; the object id is
  `moq-transport`'s to assign and cannot name `sequence_index`, which for audio — every
  packet a sync point, so every group one object with id 0 — would pin the index at zero
  and make loss undetectable. `MoqBroadcastSubscriber`: `@processor(execution = "manual")`,
  outputs `encoded_video` and `encoded_audio`, config `relay_url`, `broadcast`,
  `video_track`, `audio_track`; the processor-owned thread writes each received object as a
  bag literal, the producer's ordering pair and stamp preserved from the object rather than
  re-minted or restamped. [networking-extension-wheels]
- **DECIDED** — Two container formats, selected by `container_format` on each processor and
  declared per track in the catalog's own `packaging` field. `"cmaf"` is the default,
  because interop is the point: the broadcast is laid out as `moq-pub` lays one out — a
  `.catalog` track carrying draft-ietf-moq-catalogformat-01 JSON, an init track carrying
  `ftyp` + `moov`, media tracks whose objects are self-contained `moof` + `mdat` fragments —
  so `moq-js` and `moq-sub` can play it. `"streamlib_bag"` is the msgpack envelope, kept
  because CMAF is lossy against the bag contract: the ordering pair becomes container
  timing, `pre_skip` becomes the `dOps` box, colour goes into the VUI, and only the envelope
  can write the producer's pair back unchanged. The wheel builds CMAF on `mp4-atom`, the
  same crate the engine's own fMP4 writer is built on, carrying its own Annex-B conversion
  and sample entries — the shipped precedent being `streamlib-webrtc`'s own SPS parser. It
  is not a port of `Mp4FragmentedFileWriter`, whose growing file, shared `moov` and
  cross-track epoch are file-shaped and wrong here. Owner ruling, 2026-09-04: MoQ had never
  been finished, and finished means interoperable. [networking-extension-wheels]

## ADDED: §Networking — the publish path and the CI lane

- **DECIDED** — `python-wheel.yml` gains an `extension-wheels` job over a matrix of the two
  directories: install the just-built `streamlib` wheel into the venv, `maturin develop` the
  extension, `cargo test` its crate, `mypy.stubtest` over its `_native`, pyright over its
  Python, pytest with `-m "not requires_gpu"`, and the portability gate over its `.so`.
  `release-please-config.json` gains a package entry per wheel (independent versions and
  tags); the release workflow builds and attaches each wheel on its own tag;
  `build_simple_index.py` becomes multi-project — a set of published names, one PEP 503
  directory each — with its tests. [networking-extension-wheels]

## ADDED: the proof

- **DECIDED** — CI-run, GPU-free, endpoint-free, owned by each wheel: the RFC 6184
  packetise/depacketise round trip (the carried tests plus STAP-A and FU-A cases), SDP offer
  construction and answer parsing, MoQ catalog and object encoding, and each player's bag
  literal checked against the wire contract on the `wired_link` fixture pattern. Live,
  rig-only, under `/verify-live` with a networking arm: WHIP publish of the vivid camera
  and the known signal to Cloudflare Stream and WHEP play-back of the same stream, and MoQ
  publish and subscribe through a Cloudflare draft-16 relay — credentials read from the
  environment (Cloudflare secrets, per the owner) for MoQ as well as WHIP, since draft-16
  provisions relays per account and carries the token in the URL path; absent ones reported
  as cannot-run, never as pass. The CMAF arm additionally proves interop by shape: the
  catalog and init segment match what `moq-pub` writes. The decode-back is the lock: `WhepPlayer` / `MoqBroadcastSubscriber` →
  `H264Decoder` → tap and exchange → `xtask psnr channel-means` against the per-codec vivid
  baseline within ±0.05, the argument `Mp4Sink` made — the network sits inside a path the
  codec rig already scored, so a mismatch is the wheel's. [networking-extension-wheels]

## MODIFIED: §Networking — two names and one port shape

- **MODIFIED** — `MoqPublishTrack` and `MoqSubscribeTrack` (`:1756`) become
  `MoqBroadcastPublisher` and `MoqBroadcastSubscriber`: under the `Mp4Sink` shape a
  publisher carries a broadcast of many tracks, so a name saying one track fails the
  zero-context test. And "one output port per track" (`:1768`) becomes one output per
  media kind — `encoded_video`, `encoded_audio` — with track names in config: ports are
  declared statically by decorator, and a decoder downstream wants a port it can name at
  wiring time. [networking-extension-wheels]

## MODIFIED: §Control plane, §Consumers

- **MODIFIED** — §Control plane `:1883`: the `moq` feature, `/api/moq/catalog`, the
  `runtime_id` plumbing it carried and its test stubs are deleted; `graph` gains the
  `extensions` key. §Consumers `:349`: the held networking consumers resolve —
  `packages/{moq,webrtc}` mined, `examples/moq-roundtrip` converted as the MoQ showcase
  (publish and subscribe in one app through the relay to a `DisplayWindow`),
  `examples/webrtc-cloudflare-stream` replaced by `examples/camera-webrtc-publish` (camera
  and microphone through the codec blocks to `WhipPublisher`, credentials from the
  environment), `examples/whep-player` deleted; `examples/` then stands at thirteen
  converted beside two held (`jpeg-psnr`, frozen; `screen-recorder`). The dead
  `add_module` comments at `runtime.rs:186-190` and `check_no_inventory_submit.rs:11-13`
  are corrected in passing. [networking-extension-wheels]

## REMOVED: the moved and the dead

- REMOVED: runtime/streamlib-moq
  Leaves the workspace whole: the member line, the api-server's optional dependency, the
  `deny.toml` clarification, and the lockfile entries go with it.
- REMOVED: dep:streamlib-moq
- REMOVED: get_moq_catalog
- REMOVED: api/moq/catalog
- REMOVED: try_sessions_for_runtime
  The process-global registry and its accessors; a session belongs to one processor.
- REMOVED: packages/moq
- REMOVED: packages/webrtc
- REMOVED: examples/moq-roundtrip
  Deleted and rewritten from scratch under the same name, per the conversion doctrine; the
  gate passes once the rewrite lands in the same PR as the deletion.
- REMOVED: examples/webrtc-cloudflare-stream
- REMOVED: examples/whep-player
