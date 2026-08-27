# Research memo: audio plugin formats for a BUSL-1.1 Linux host

2026-08-26, for the `[audio-subsystem]` align (feeds the OPEN audio-plugin lane).
Question: if StreamLib ever hosts third-party audio plugins on Linux, which format
first, and is any legally closed to a closed-source, commercially-sold BUSL-1.1
host? Licensing claims below were verified against actual licence text.

## Headline findings

1. **The VST3 licensing wall is gone.** Since VST3 SDK 3.8 (~Oct 2025) the SDK is
   plain MIT — verified from `LICENSE.txt` ("Copyright (c) 2026, Steinberg Media
   Technologies GmbH", unmodified MIT). The old dual regime (GPLv3 or a
   countersigned proprietary Steinberg agreement) is withdrawn. Trademark use
   (VST name/logo in marketing) is separate and optional; "loads VST3 plugins"
   descriptively is fine. Do NOT build on `vst3-sys` (GPLv3, pre-MIT derivative);
   use coupler-rs `vst3` (MIT/Apache, regenerated from the MIT headers).
2. **Bridging does not collapse the problem.** `clap-wrapper` runs the wrong
   direction (wraps CLAP plugins *for* VST3/AU hosts); no mature VST3→CLAP
   adapter exists. `yabridge` (GPL-3, user-installed, arm's-length via the
   standard plugin ABI) makes Windows plugins appear as native Linux files of the
   same format — a VST3 host + yabridge reaches essentially the whole Windows
   commercial catalog. DISTRHO Ildaeil (GPL, user-installed) is a
   plugin-that-hosts-plugins escape hatch.
3. **CLAP first** (adopted as the OPEN entry's direction): `clack-host` 0.1.1 is
   MIT/Apache on crates.io (verified; published 2026-05, updated 2026-07) and is
   the only mature, safe, maintained Rust host library for any format; the repo's
   dead macOS host (~1,800 lines on clack) transfers as CLAP wiring logic — its
   Linux search paths were already written under the platform gate. VST3 is the
   legally-clean second when reach demands it (raw COM bindings, no host
   framework, ~3–5k lines inferred). LV2 (ISC; `livi` MIT but thin,
   single-maintainer) third, for the Linux-studio long tail. LADSPA: skip (LGPL
   header, legacy catalog).

## Licence table (all verified from licence text)

| | CLAP | VST3 (≥3.8) | LV2 | LADSPA |
|---|---|---|---|---|
| SDK licence | MIT | MIT | ISC (lilv/serd/sord/zix same family) | LGPL-2.1 header |
| BUSL host legal | Yes | Yes | Yes | Yes in practice; only copyleft on the list |
| Rust host crate | `clack-host` (MIT/Apache, crates.io, safe API) | `vst3` coupler-rs (MIT/Apache, raw COM) — never `vst3-sys` (GPLv3) | `livi` 0.7.5 (MIT, headless-only) | trivial FFI |

## Catalog reality (Linux, 2026)

CLAP: 394 plugins / 93 vendors, ~⅔ of recent clapdb entries listing Linux — u-he's
full line (Linux CLAP+VST3, VST2 dropped 2024), Surge XT, LSP, Airwindows
Consolidated (~350 effects), TAL, Audio Damage. VST3: all of the above plus the
yabridge-reachable Windows world (Valhalla, Arturia, NI). LV2: the Linux-studio
canon (LSP 100+, Calf, x42, Dragonfly, ZynAddSubFX, Guitarix, sfizz, MOD).
Trajectory: CLAP growing (Reaper, FL Studio, Bitwig host it) but VST3 remains the
commercial default; Ardour stays LV2+VST3. Headless params-only hosting is proven
product practice (MOD Audio's hardware line runs a headless LV2 host).

## Smallest useful host

Params-only, GUI-less CLAP effect host: scan → instantiate → activate → process
deinterleaved f32 blocks → param list/set/get + state save/load. With
`clack-host` + `clack-extensions`: ~1.5–2.5k lines (calibration: the repo's dead
macOS host did scan/instantiate/activate/process/params in ~1,800 lines, minus
state save/load). Wayland GUI embedding is unsolved for every format —
X11/XWayland if ever wanted; GUI is separable and deferred.

## Open questions carried

CLAP denormal (FTZ/DAZ) ownership convention — read `clap/plugin.h` notes at
implementation time; `livi` bus factor (an LV2 lane might sit on C `lilv` FFI
directly); exact FabFilter/Valhalla format matrices before naming them in
user-facing claims; whether pre-3.8 header generations are retroactively MIT
(low risk; one-time legal read if VST3 ships).

## Sources

CLAP licence: https://github.com/free-audio/clap/blob/main/LICENSE · VST3
LICENSE.txt: https://raw.githubusercontent.com/steinbergmedia/vst3sdk/master/LICENSE.txt ·
Steinberg portal: https://steinbergmedia.github.io/vst3_dev_portal/pages/VST+3+Licensing/Index.html ·
KVR announcement: https://www.kvraudio.com/news/steinberg-moves-vst-3-sdk-to-mit-open-source-license-asio-now-gplv3-65179 ·
LV2/lilv: https://github.com/lv2/lv2 · https://raw.githubusercontent.com/lv2/lilv/master/COPYING ·
LADSPA: https://www.ladspa.org/ · clack-host:
https://crates.io/crates/clack-host · vst3: https://crates.io/crates/vst3 ·
vst3-sys (GPLv3): https://github.com/RustAudio/vst3-sys · livi:
https://crates.io/crates/livi · clap-wrapper:
https://github.com/free-audio/clap-wrapper · yabridge:
https://github.com/robbert-vdh/yabridge · Ildaeil:
https://github.com/DISTRHO/Ildaeil · mod-host:
https://github.com/mod-audio/mod-host · Libre Arts CLAP retrospective:
https://librearts.org/2024/11/clap-api-two-years-later/ · clapdb:
https://clapdb.tech/ · Surge XT: https://surge-synthesizer.github.io/downloads/ ·
LSP: https://lsp-plug.in/
