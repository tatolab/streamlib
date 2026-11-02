# Media built-ins become modules

## Trigger

Reach for this when a capability is proposed as a built-in in the runtime, when a first-party
node needs something from the engine that a third-party extension cannot reach, or when a
module's native code needs to touch ports or surfaces without Python.

## Decision

The runtime ships no media nodes. Camera, microphone, speaker, the video and audio codecs, the
test pattern and the virtual camera are first-party extension wheels, built exactly the way a
third party builds one: a Python node whose per-frame work runs in the wheel's native code, in
that node's own processor interpreter. Anything a first-party module needs from the engine
becomes public API a third party can use too.

A module's native half reaches the data plane through one host table: a `#[repr(C)]`,
append-only function table, versioned by major, minor and size, which the lent
`tatolab.runtime` implements once and hands to the wheel as a named capsule. It carries
data-plane verbs and plain handles only — ports, log, clock, stop flag, host capabilities, and
later surface slot sets — never an RHI object. A module built against minor N loads on any
runtime at minor N or later.

Video codecs run on a Vulkan device the module owns, on the runtime's GPU matched by UUID; the
engine keeps no video queues. The engine's audio device arms stay in the engine and are exported
through the host table, so there is one audio device path. The relay reader stays in the
runtime as transport.

The owner's reasons, 2026-11-02: a capability built into the engine disproves that others can
build on their own hardware; it skips the consumer path, so the product is never dogfooded; it
hides what extensibility is missing; and it assumes every machine wants a camera, where a
general-purpose engine delivers one as a module when a stream needs it.

## Rejected alternatives

- **Built-ins by criterion** (a deadline the helper hop cannot meet, an engine-only primitive,
  an OS-facing device) — every criterion turned out to be an engine gap a module could not get
  past, not a property of the capability. A device callback already only fills a ring that a
  separate thread publishes over shared memory; JACK, PipeWire, Chromium's audio service and
  CoreAudio's driver host all run it in a process apart from the producer.
- **No host table: native threads take the GIL for every write** — adequate for a camera,
  but it puts Python's lock on every audio block.
- **A module opens its own shared-memory and surface-share connection** — a second data
  plane that must match the runtime's layout byte for byte.
- **The deleted plugin ABI** — it mirrored the RHI across 22 vtables, accepted only an exact
  build, carried a second mode through the engine, and dlopen'd module code into the shared
  runtime. The host table has none of those properties: the engine never calls module code and
  nothing is loaded into the runtime.
- **Codec sessions as an engine service behind the host table** — keeps 52,000 lines and
  the most driver-fragile code in the shared runtime, and adds a round trip per frame.

## Consequences

- A crash in a device or codec node ends that node's stream, not every stream on the machine.
- A stream that uses no camera loads no camera code; device permission prompts follow the
  nodes a stream actually uses.
- The host table is a second binary boundary beside the CPython ABI. A lint keeps RHI types
  out of it, and its version rules are what keep it narrow.
- Each codec helper pays for a second Vulkan device (memory, startup, GPU timeslicing).
- Built-in type strings leave `tatolab.stream`; recorded graphs naming them refuse by name and
  name the module to install.
- The Vulkan Video port, `streamlib-consumer-rhi` and the vendored vulkanalia become published
  crates, so a third party can build a GPU module.
