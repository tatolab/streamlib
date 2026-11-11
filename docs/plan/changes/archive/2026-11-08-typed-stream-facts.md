# typed-stream-facts

> **Asked for by the owner, 2026-11-08:** "Absolutely no caps … we very explicitly did not use
> caps and moved towards concrete types like holoscan and touch designer. My intent is to move
> away from hang since it's moq specific, I wanted general purpose … I want everything very
> intentionally in nodes … The goal is making everything flow through nodes and always be
> visible to end users so we can have the appropriate pins and see things in properties and
> such." Approved as written in the project thread the same day: "Excellent that's exactly what I
> want."

A pivot of one concern: what a port says about the stream it carries. The engine speaks
hang's catalog layout today on every output port. After this change, each family states its
stream facts as typed fields of its own. hang is spoken only inside `tatolab-moq`, whose track
node builds a track's catalog entry from those fields at the network boundary.
Rationale: `docs/decisions/typed-stream-facts.md`.

**The direction, in five sentences:**
1. A port's stream facts are typed fields of its family — never a free JSON entry in another
   project's layout, and never compared or negotiated at `connect`.
2. Image states its frame rate and its static HDR facts (content light level, mastering display);
   Packets states what a decoder needs before its first unit (coded extent, audio rate and channel
   count, and the codec configuration record).
3. The node that owns the port declares the facts it knows from config and replaces them while
   running; a reader asks for the facts of the port feeding its input.
4. `graph`, `/api/registry` and Tato Studio show them on the port, as pins and in Properties.
5. The Packets payload is the codec's unit alone; the hang container and the catalog entry exist
   only inside `tatolab-moq`.

**What a node author writes after this change:**

```python
class Camera(Node):
    video = Output(Image, formats=("bgra8",), rate=from_config("frames_per_second"))

class H264Encoder(Node):
    encoded = Output(Packets, codec="avc1")
    def process(self, ctx):
        ...
        ctx.outputs.set_stream_facts(self.encoded, PacketsStreamFacts(
            rfc6381_codec="avc1.64001f", coded_width=1920, coded_height=1080))

class OpusDecoder(Node):
    encoded = Input(Packets, delivery="ordered", requires_codec=("opus",))
    def setup(self, ctx):
        facts = ctx.inputs.upstream_stream_facts(self.encoded)  # PacketsStreamFacts | None
```

**Scale gate — this skill plus an ADR.** It changes the processor model (what a port declares),
the Python API's public contract (`catalog=`, `set_catalog_entry`, `inbound_link_catalog_entry`,
`tatolab.stream.catalog`), the module C ABI (`output_port_set_catalog_entry`) and the Packets
payload layout every codec node reads.

## Current state (recon, 2026-11-08, `main` `fe88a6459` with the strict-port-families fold; plan anchors are ARCHITECTURE.md after that fold)

- **The catalog entry** is `CatalogEntry { section, entry: serde_json::Value }`
  (`sdk/streamlib-processor-schema/src/catalog_entry.rs:17-87`) on `PortDescriptor.catalog_entry`
  (`descriptors.rs:47-50`) and `PortInfo.catalog_entry` (`graph/nodes/port_info.rs:41`). Typed
  helpers `VideoEntry`, `AudioEntry`, `TextEntry`, `JsonEntry`, `BinaryEntry` spell hang's field
  names (`catalog_entry.rs:110-200`; Python `sdk/tatolab-stream/tatolab/stream/catalog.py`).
- **The engine parses every entry with the `hang` crate**:
  `runtime/streamlib-engine/src/core/output_port_catalog_entries.rs:27-57`, at registration
  (`processor_instance_factory.rs:650`) and on each replace (`:100`). `hang = "=0.21.16"` is an
  engine dependency (`runtime/streamlib-engine/Cargo.toml:98`).
- **Replace and read:** `ctx.outputs.set_catalog_entry` (`_node_context_protocols.py:809`),
  `ctx.inputs.inbound_link_catalog_entry` (`:704`); Rust `iceoryx2/output.rs:404-431`,
  `iceoryx2/input.rs:513-544`; the helper's escalate op
  `compiler_ops/subprocess_escalate/port_catalog_entry.rs:42-90`; C ABI
  `tatolab_module_host_table.h:637-640`.
- **Who writes one:** the owned-device video encoders from the SPS at each sync point
  (`encoded_video_publication.rs:4-56`), the Opus encoder, the WHEP player
  (`whep_player.rs:80-125`), `MoqTrackReceive` (`moq_track_receive.rs:46,175-182`).
  **Who reads one:** the Opus decoder for channel count and OpusHead
  (`encoded_packet_to_audio_block_decoder.rs:55-70,239-294`), the WebRTC sender for each track's
  codec, `MoqTrackSend` (`moq_track_send.rs:131-153`) and `MoqGateway` (`moq_gateway.rs:96-99,457-471`).
- **The Packets payload** is hang's legacy container frame (varint µs stamp, then the unit):
  `runtime/streamlib-engine/src/core/hang_legacy_container_media_frame.rs`, re-exported from
  `streamlib-sdk` (`lib.rs:88`) and copied into the codec nodes, `tatolab-opus` and
  `streamlib-webrtc`. The stamp already rides the envelope's `timestamp_ns`.
- **Frame rate** is on no port and no message: only device formats
  (`video_device_backend.rs:42`), encoder config (`video_encoder_settings.rs:20,33`) and metrics.
  **Static HDR** (MaxCLL/MaxFALL, mastering display) lives only in the retired msgpack
  `VideoFrame` (`streamlib-media-builtins/src/video_frame.rs:38-54,250-259`). The Image
  descriptor body is full: 56 of 60 bytes (`port_family_payload_descriptor_bodies.rs:20-64`).

## ADDED

- **ADDED (§Processor model, DECIDED):** A port's **stream facts** are typed fields its family
  defines, describing the stream rather than one message. `connect` never compares them and the
  engine inserts nothing on their account; a mismatch a node cannot handle is that node's refusal
  in its own words. [typed-stream-facts]
  - `ImageStreamFacts`: `frame_rate` (a rational, `numerator`/`denominator`),
    `content_light_level` (`max_content_light_level`, `max_frame_average_light_level`, cd/m²),
    `mastering_display_colour_volume` (SMPTE ST 2086 in the H.265 SEI's units: three primaries
    and the white point, `max_display_mastering_luminance`, `min_display_mastering_luminance`).
    Every field optional.
  - `PacketsStreamFacts`: `rfc6381_codec` (the codec with profile and level, `avc1.64001f`),
    `coded_width`, `coded_height` (video), `sample_rate`, `channel_count` (audio),
    `codec_configuration_hex` (the codec's out-of-band configuration record, such as `avcC`,
    `hvcC` or `OpusHead`, as lowercase hex, absent when the stream carries it in band). Every
    field optional.
  - Channels, Array and Records have no stream facts beyond their declared keys (`rate=`, `names=`,
    `dtype=`, `residency=`): what they carry is stated per message or by name.
- **ADDED (same section, DECIDED):** `Output(Image, rate=…)` declares the frame rate as a literal
  rational or `from_config(key)`, resolved once the node's config is known, as Channels' `rate=`
  is. It seeds `ImageStreamFacts.frame_rate`. [typed-stream-facts]
- **ADDED (same section, DECIDED):** The node that owns an output replaces its facts while
  running — `ctx.outputs.set_stream_facts(port, facts)` in Python, `OutputWriter::set_stream_facts`
  in Rust, `output_port_set_stream_facts` in the module C ABI — and a reader asks
  `ctx.inputs.upstream_stream_facts(port)` for the facts of the output feeding that input, `None`
  until any are stated. The facts type must be the port's family's; anything else is refused.
  This is the catalog-entry plumbing (output store, helper escalate op, C ABI slot) carrying a
  typed value in place of a JSON one, not a second system. [typed-stream-facts]
- **ADDED (§Control plane, DECIDED):** `graph` and `/api/registry` render each output's
  `stream_facts` in the family's own field names, and each Records port as
  `values_checked: false` — the node catalog's "unchecked" mark. [typed-stream-facts]
- **ADDED (§Networking, DECIDED):** `MoqTrack` builds its track's hang catalog entry from the
  upstream port's family, its declared codec and its stream facts — Image's `frame_rate` and
  HDR facts, Packets' coded extent, rate, channel count and `codec_configuration_hex` as hang's
  `description` — and its optional `catalog` config still overrides it. `MoqTrackSend` wraps each
  unit in hang's container on the way out; `MoqTrackReceive` unwraps it and states the facts it
  read from the remote catalog on its output. hang's words exist only in `tatolab-moq`.
  [typed-stream-facts]

## MODIFIED

- **MODIFIED (§Processor model, "a port carries its producer-written catalog entry" `:1012-1018`,
  and the catalog-entry rendering `:1279`):** replaced by the stream-facts entries above.
- **MODIFIED (§Processor model, the Image body entry `:1389-1390`):** "Stream facts an Image frame
  does not carry — frame rate, content light level, mastering display — belong on the port's
  catalog entry, updated through `set_catalog_entry`" becomes "… are `ImageStreamFacts`". The
  heading's unbuilt item "an Image port's stream facts on its catalog entry" becomes "[typed-stream-facts]".
- **MODIFIED (§Media I/O, codec entries `:2997`, `:3033-3036`, `:3144`, `:3161`):** the encoders
  state `PacketsStreamFacts` after their first sync point; colour still rides the bitstream's VUI
  into the decoder's `ImageFrame`. The Opus encoder states `sample_rate=48000`, its channel count
  and `OpusHead`; the decoder reads them with `upstream_stream_facts`. A unit is the codec's
  bytes alone; its stamp is the envelope's `timestamp_ns`.
- **MODIFIED (§Networking, WebRTC `:3270`, `:3304`):** each track's codec comes from the upstream
  Packets port's declared codec, its extent from `PacketsStreamFacts`.
- **MODIFIED (§Networking, "The catalog is assembled from port descriptions" `:3527-3547`, the
  track-node entry `:3405`, `MoqTrackReceive` `:3578`):** the catalog is assembled by
  `MoqTrack` from stream facts, as above. The rejected "entry in the runtime's own words" stands
  rejected in its 2026-10-17 sense — a `kind` or a free-text `encoding` — and is answered by typed
  family fields, not by hang's layout.
- **MODIFIED (§Networking, the moq-dev line `:3616-3624`):** the engine and `streamlib-sdk` no
  longer depend on `hang`; `tatolab-moq` and `tatolab-read-track` do.
- **MODIFIED (`changes/gateway-nodes.md` "Stays"):** "the catalog-entry layout on output ports"
  leaves the list; `MoqTrack`'s `catalog` and `MoqTrackSend`'s upstream entry read the facts.
- **MODIFIED (GLOSSARY):** **Catalog entry** — a MoQ track's description in hang's layout, built
  by `MoqTrack`. New **Stream facts** — a port's typed description of its stream, by family.
  _Avoid_: "caps", "metadata", "catalog" for it.
- **MODIFIED (Tato Studio, tatolab-app):** Properties and the Viewer read `stream_facts` and
  `values_checked` from the port. Filed as an app issue that waits on ticket 1.

## REMOVED

- REMOVED: set_catalog_entry
- REMOVED: inbound_link_catalog_entry
- REMOVED: output_port_set_catalog_entry
- REMOVED: with_catalog_entry
- REMOVED: output_port_catalog_entries
- REMOVED: refuse_catalog_entries_hang_does_not_read
- REMOVED: catalog_entry_grammar
- REMOVED: sdk/streamlib-processor-schema/src/catalog_entry.rs
- REMOVED: sdk/tatolab-stream/tatolab/stream/catalog.py
- REMOVED: runtime/streamlib-engine/src/core/compiler/compiler_ops/subprocess_escalate/port_catalog_entry.rs
- REMOVED: runtime/streamlib-engine/src/core/hang_legacy_container_media_frame.rs
- REMOVED: UpstreamCatalogEntry
  Continuation: `catalog=` on `Output` goes with `catalog.py`; the engine's `hang` dependency goes
  with the container in ticket 2 (checked by hand in the ship PR, since the pattern also names
  `tatolab-moq`'s). `tatolab-moq` keeps its own `moq_catalog.rs` and a
  container module named for MoQ.

## Not in this change

- Per-frame dynamic HDR (HDR10+, Dolby Vision) stays a later descriptor kind.
- A node that emits a port's facts as Records you can wire (an Info node, as TouchDesigner's Info
  CHOP) is a natural follow-up, not built here.
- Growing the 60-byte descriptor body. Stream facts change rarely; a per-frame copy buys nothing.

## Tickets (as few tracer bullets as the change needs)

1. **Typed stream facts, end to end** (rescopes #3284). `ImageStreamFacts`, `PacketsStreamFacts`
   and Image's `rate=` in the schema, the macro, `tatolab-stream` and its Protocols; the
   set/read plumbing retyped; `graph`/registry render `stream_facts` and `values_checked`; every
   writer and reader in Current state moves over; `MoqTrack` builds the hang entry; the catalog
   entry is removed. Proven by engine tests for set, read,
   rendering and refusal of the wrong family's facts, a `tatolab-moq` test that the built entry
   parses with hang, and the Opus round trip.
2. **The Packets unit without hang's container** (#3290). Codec nodes, `tatolab-opus` and
   `streamlib-webrtc` read and write the bare unit; `MoqTrackSend`/`MoqTrackReceive` wrap and
   unwrap; the engine's `hang` dependency goes with the container. Proven by the codec round
   trips and the moq send/receive test. Blocked by nothing.
