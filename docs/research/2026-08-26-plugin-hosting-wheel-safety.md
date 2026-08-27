# Research memo: does plugin hosting break the single-wheel pip install?

2026-08-26, for the `[audio-subsystem]` align (feeds the OPEN audio-plugin lane).
Question: is hosting CLAP/VST3/LV2 plugins compatible with the one-artifact
`pip install streamlib` experience and the wheel's nine-entry `DT_NEEDED` gate?
Claims below were verified by inspecting actual artifacts (crates from
static.crates.io, wheels from PyPI, plugin binaries), not documentation prose.

## Verdict (adopted)

**No break.** CLAP and VST3 host crates add **zero** `DT_NEEDED` entries:
`clack-host`/`clack-extensions`/`clap-sys` and coupler-rs `vst3` are pure Rust —
no build.rs, no C compiled, no library linked; plugins load via
`libloading`/dlopen, the wheel's established pattern for Vulkan/libcuda. The gate
passes unchanged. LV2 is the one format with a real, solvable cost: the published
`lilv-sys`/`lilv` crates pkg-config-link system `liblilv-0` (`#[link(name =
"lilv-0")]`) — a gate violation as published; hosting LV2 requires vendoring and
statically linking the small ISC C stack (lilv/serd/sord/sratom/zix).

## The precedent

**Spotify's `pedalboard` ships 15 Linux wheels on PyPI hosting VST3 in-process**
— manylinux_2_27/2_28 x86_64 + aarch64 and musllinux_1_2, cp310–cp314 (verified
via the PyPI API and by ELF-parsing the wheel). It statically compiles JUCE into
one 8.7 MB extension, bundling auditwheel-mangled `libfreetype`/`libasound`
copies for JUCE's own code; a Rust host is strictly cleaner (no JUCE baggage, no
bundling needed). Pedalboard documents the in-process cost honestly: plugins
"may even crash the Python interpreter without warning". Contrast: `dawdreamer`
leaves `libGL.so.1` un-bundled and fails to import in GL-less containers — the
failure mode the gate structurally prevents.

## What plugins themselves need (measured from shipping binaries)

Surge XT 1.3.4: `libasound`, `libfreetype` in NEEDED; X11 only as dlopen strings
(JUCE loads it lazily when a GUI opens). Airwindows Consolidated: `libfontconfig`,
`libfreetype`. LSP LV2: the DSP `.so` needs `libcairo`/`libsndfile`; X11/GL live
in a separate UI `.so` (LV2's spec-level DSP/UI split). So headless hosting is
genuinely cheap but not free: font/graphics support libs are often `DT_NEEDED` on
the plugin and map at dlopen even with no editor. Per-plugin, outside our gate, a
documentation matter.

## In-process vs out-of-process (adopted: out-of-process, always)

In-process, a segfaulting plugin kills CPython, the engine, the Vulkan device,
and every iceoryx2 session; `setlocale`/signal/atexit state is process-global;
FTZ/DAZ is per-thread (containable by save/restore) but nothing else is. Industry
practice at the high end is process isolation: Bitwig's five sandbox modes,
REAPER's dedicated-process firewalling, Carla/yabridge as bridge architectures —
yabridge does the realtime `process()` across processes via shared-memory buffers
+ socket sync at normal DAW buffer sizes. Ardour's in-process counterargument
assumes hundreds of plugins at 1.3 ms buffers; at StreamLib's 5–11 ms blocks with
a handful of chains, two SHM crossings cost ~20–100 µs — at most ~2% of the block
budget at its 5 ms low end. The
shape is StreamLib's existing helper-process doctrine applied to another foreign
binary, over the transport it already owns. Known costs: lifecycle + param/state
plumbing, and scanning in a throwaway subprocess (scanning executes arbitrary
code).

## Graceful absence

Nothing loads until asked: no static init, no import-time dlopen; discovery
touches paths only on request; empty dirs are an empty list. (Under the adopted
project-local declaration rule, machine-global paths are not even the model.)

## Open questions carried

Closed-source plugin linkage (u-he demos) uninspected — affects which plugins
load headless, not the wheel; LV2 static vendoring mechanics (fork `lilv-sys` vs
fresh `-sys` crate with a `cc` build); whether the gate test should also cover a
future plugin-host helper binary in the wheel (it should — same allowlist).

## Sources

Artifact-verified: pedalboard 0.9.24 + dawdreamer 0.9.0 wheels (PyPI);
clack/vst3/livi/lilv crates (static.crates.io); Surge XT 1.3.4, Airwindows
Consolidated, lsp-plugins-lv2 1.2.35 binaries. Docs: pedalboard
https://github.com/spotify/pedalboard ·
https://spotify.github.io/pedalboard/compatibility.html · auditwheel policy:
https://github.com/pypa/auditwheel/blob/main/src/auditwheel/policy/manylinux-policy.json ·
yabridge architecture:
https://github.com/robbert-vdh/yabridge/blob/master/docs/architecture.md · Bitwig
sandboxing:
https://www.bitwig.com/learnings/plug-in-hosting-crash-protection-in-bitwig-studio-20/ ·
REAPER dedicated-process:
https://reaper.blog/2012/02/run-plugin-as-dedicated-process/ · Ardour rationale:
https://ardour.org/plugins-in-process.html
