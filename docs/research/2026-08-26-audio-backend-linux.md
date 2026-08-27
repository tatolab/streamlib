# Research memo: the Linux audio backend under the wheel's portability gate

2026-08-26, for the `[audio-subsystem]` align. Question: can "PipeWire-native on
Linux" survive the wheel's `DT_NEEDED` gate, and what is the honest chain of
fallbacks? Evidence split **[V] verified** (primary source read, or measured on a
stock Ubuntu 24.04.4 desktop, PipeWire 1.0.5) and **[I] inferred**.

## Recommendation (adopted)

PipeWire-native as primary, loaded via dlopen using the SDL3 pattern — vendored
MIT headers, a small compiled C shim owning the header-only SPA inline layer, ~33
`pw_*` symbols bound at runtime — with a hand-rolled dlopen'd ALSA backend
(`libasound.so.2`, ~40 symbols, the cubeb pattern) as the permanent fallback, and a
null backend last. The widely-cited claim that PipeWire cannot be dlopen'd
(miniaudio: a built-in PipeWire backend "will never happen") holds only under
miniaudio's no-headers-at-build-time philosophy; SPA has no shared object, so it is
a build-time concern, and SDL3 ships exactly this split as its default Linux
configuration [V: `SDL_pipewire.c` includes `<pipewire/…>`/`<spa/…>` headers while
resolving every symbol through `SDL_LoadObject`/`SDL_LoadFunction`].

## Key findings

- **ALSA-the-API is PipeWire on stock Ubuntu.** `pipewire-alsa` is default-seeded
  and rewrites `pcm.!default` to `type pipewire` [V, measured on the rig:
  `/usr/share/alsa/alsa.conf.d/99-pipewire-default.conf`]. Raw `hw:` still
  bypasses the daemon (measured `EBUSY` on the active card). Debian desktops do
  **not** seed `pipewire-alsa` [V, Debian wiki] — an ALSA-only backend grabs raw
  hardware there.
- **The compat plugin throws timestamps away.** Measured through `"default"`:
  `htstamp` is synthesized "now" (341 ns off a direct `clock_gettime`),
  `audio_htstamp` = 0; the plugin implements no timestamp callback [V, grep of
  `pcm_pipewire.c`]. Native `pw_time.now` is `clock_gettime(CLOCK_MONOTONIC)` [V,
  `stream.c`] — the same epoch as V4L2's `V4L2_BUF_FLAG_TIMESTAMP_MONOTONIC` [V,
  kernel docs]. Capture stamp = status/now minus reported delay, on any backend;
  hardware DMA stamps are opportunistic (PipeWire ships
  `api.alsa.htimestamp = false` for driver-trust reasons [V, `alsa-pcm.c`]).
- **dlopen verdicts.** `libasound.so.2`: trivially bindable — decades-stable
  soname, opaque-pointer C API, cubeb's `LIBASOUND_API_VISIT` enumerates the ~40
  needed symbols [V]. `libpipewire-0.3.so.0`: bindable with bounded effort — ABI
  stated compatible 1.0→1.6 [V, NEWS]; bind nothing newer than 0.3.50 without a
  `dlsym` probe (`pw_stream_get_nsec` is 1.1.0; Ubuntu 24.04 ships 1.0.5). What
  must NOT be used: `pipewire-rs` and cpal's PipeWire host — both link via
  pkg-config, straight into `DT_NEEDED` [V, cpal Cargo.toml]. cpal's ALSA host
  links `libasound` — the original gate failure.
- **Headless/container reality.** `ubuntu:24.04` and `manylinux_2_28` images carry
  zero audio libraries; Ubuntu Server's manifest has neither libasound nor
  pipewire [V, checked]. A dlopen miss returns null — the failure mode is ours to
  shape; hence the null backend (processors run, produce silence, discard).
- **Realtime.** PipeWire clients get RT automatically (`libpipewire-module-rt`:
  RLIMIT_RTPRIO → RealtimeKit → portal) [V, `client.conf`]; the ALSA arm uses the
  engine's existing `linux/rtkit.rs`. In containers, degrade to best-effort
  without failing the stream.
- **JACK** is subsumed by pipewire-jack; not a target [I, consistent secondary
  sources].
- **Prior art is uniform**: everything credible shipping portable Linux binaries
  dlopens (SDL3 all backends, cubeb libpulse+libasound); linkers (JUCE, cpal) are
  per-machine builds or fail the gate.

## Fallback chain (adopted): PipeWire → ALSA `default` → null

Each hop is a cheap probe in one native module. Step 2 on a PW desktop still lands
in PipeWire via the seeded plugin (and in PulseAudio on 22.04 holdouts — free
coverage); headless-with-`/dev/snd` gets raw hardware. Do not add JACK or libpulse
arms, and no user-visible backend configuration — two probes and a null, chosen
automatically, logged once.

## Open questions carried forward

1. Engine clock reconciliation — resolved by the align: the device paces; the
   timerfd clock serves deviceless graphs.
2. No rigorous published latency deltas (plugin vs native vs hw-direct); measure
   on the rig if a budget ever becomes contractual.
3. PipeWire floor: Ubuntu 24.04 / PipeWire 1.0.5 assumed oldest supported desktop.
4. Vendored headers vs `pipewire-devel` in the build container — implementation
   choice; both satisfy the gate (`DT_NEEDED` is a runtime property).

## Sources

PipeWire source (`stream.c`, `alsa-pcm.c`, `pcm_pipewire.c`, `module-rt.c`,
`client.conf.in`, NEWS): https://github.com/PipeWire/pipewire · pw_time docs:
https://docs.pipewire.org/structpw__time.html · kernel timestamping:
https://www.kernel.org/doc/html/latest/sound/designs/timestamping.html · V4L2
buffer flags:
https://www.kernel.org/doc/html/latest/userspace-api/media/v4l/buffer.html ·
SDL3: https://github.com/libsdl-org/SDL/blob/main/src/audio/pipewire/SDL_pipewire.c ·
cubeb: https://github.com/mozilla/cubeb/blob/master/src/cubeb_alsa.c · miniaudio
refusal: https://github.com/mackron/miniaudio/discussions/711 · Tuple on static
PipeWire: https://tuple.app/blog/hacking-dlopen-to-statically-link-pipewire ·
cpal: https://github.com/RustAudio/cpal · Debian wiki:
https://wiki.debian.org/PipeWire · Ubuntu Server manifest:
https://releases.ubuntu.com/noble/ubuntu-24.04.3-live-server-amd64.manifest
