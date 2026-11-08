# Typed stream facts

Rationale for the `[typed-stream-facts]` entries in `docs/plan/ARCHITECTURE.md` §Processor
model, §Control plane and §Networking, decided 2026-11-08. Supersedes the port-metadata half of
`moq-relays.md` (an output port carries a catalog entry in hang's layout) and extends
`strict-port-families.md`.

## Trigger

Read this before adding a field that describes a port's stream (frame rate, HDR, coded extent,
codec configuration), before putting another project's vocabulary on a port, before letting a
port's description change what `connect` does, and before adding the `hang` crate, or any
transport's crate, to the engine.

## Decision

A port's stream facts are typed fields its family defines: `ImageStreamFacts` (frame rate,
content light level, mastering display) and `PacketsStreamFacts` (coded extent, audio rate and
channel count, codec configuration record). An Image output declares its frame rate with `rate=`,
as a Channels output does. The node that owns the port replaces its facts while running, and a
reader asks for the facts of the port feeding its input. `graph`, the registry and Tato Studio
show them on the port. `connect` never compares them. hang's catalog layout is a MoQ concern:
`tatolab-moq`'s track node builds a track's catalog entry from the facts, and the engine holds
no `hang` dependency. The Packets unit is the codec's bytes alone.

## Why

- **Concrete types, as Holoscan, TouchDesigner and Unreal do.** Their frames carry width,
  format, rate and colour as typed fields — a TOP's `pixelFormat`, an `AudioBuffer`'s
  `sampling_rate`, Unreal's `IMediaTextureSample` mastering luminance — never as a negotiated
  capability or a JSON blob in a transport's words.
- **General purpose, not MoQ-shaped.** A port feeds a file writer, a WebRTC sender, a decoder or
  an agent as often as a MoQ track. Writing every port's description in hang's layout made one
  transport's vocabulary the engine's, and made the engine parse it with that transport's crate.
- **Visible on the node.** Typed fields render as named properties on a pin, which a person can
  read and an agent can wire against, the way Houdini's spreadsheet and TouchDesigner's Info CHOP
  show a stream's facts at any node.
- **No negotiation, no converters.** Facts describe; they never select a path. A node that cannot
  take a stream refuses in its own words, and the conversion is a node someone places.
- **One system.** The output store, the helper's escalate op and the C ABI slot that carried the
  catalog entry carry the typed facts; nothing is built beside them.

## Rejected alternatives

- **Facts on hang's catalog entry** (`VideoEntry` gaining frame rate and HDR) — extends a
  MoQ-specific layout into every port and keeps hang in the engine.
- **Neutral facts with a negotiation or intersection step** — that is caps under another name;
  `strict-port-families.md` rejected it.
- **Per-frame copies in the descriptor body** — the Image body has 4 bytes free, and facts that
  change once per session buy nothing by riding every frame. Per-frame dynamic HDR is a later
  descriptor kind.
- **A free key-value metadata map on every message** (Holoscan's `MetadataDictionary`) — open
  keys are exactly what an agent cannot wire against; Records already carries open data.

## Consequences

- `catalog=`, `set_catalog_entry`, `inbound_link_catalog_entry`, `tatolab.stream.catalog`, the
  C ABI's `output_port_set_catalog_entry` and the hang container leave the engine and the SDK in
  one change. Every codec node, `tatolab-opus`, `streamlib-webrtc` and `tatolab-moq` moves with it.
- A new stream fact is a new typed field on its family, a schema change with a test, never an
  ad-hoc key.
- `tatolab-moq` owns the mapping from facts to hang's catalog, so a hang version bump touches
  one module.
