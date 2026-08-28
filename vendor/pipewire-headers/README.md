# PipeWire and SPA headers, vendored

Upstream: <https://gitlab.freedesktop.org/pipewire/pipewire>, tag `1.0.5`, MIT
(`COPYING`). This is a header-only drop — no PipeWire source is compiled, and no
PipeWire library is linked. `libpipewire-0.3.so.0` binds at runtime through
`libloading`, so the wheel's `DT_NEEDED` set does not grow.

## Why the headers are here rather than on the build machine

`manylinux_2_28` carries no PipeWire development package, so a system-header
build is not reproducible where the wheel is actually built. Same reasoning as
the `shaderc` `build-from-source` pin in the workspace `Cargo.toml`: what the
artifact is compiled against has to travel with the repository.

SPA's pod builders and parsers are `static inline` C with no shared object
behind them, so they are a build-time dependency no amount of `dlopen` can
remove. `runtime/streamlib-engine/src/linux/pipewire_audio_shim.c` is what
compiles them in, and it calls PipeWire only through function pointers the Rust
side filled with `dlsym`.

## What was taken

- `src/pipewire/**/*.h` → `include/pipewire/`
- `spa/include/spa/**/*.h` → `include/spa/`
- `COPYING`

The `.c` files, `meson.build` files and everything else in the upstream tree
were left behind. Every header is byte-for-byte upstream and carries its own
`SPDX-License-Identifier: MIT` — **never** add a BUSL header here, and never
reformat these sources.

`include/pipewire/version.h` is the one file upstream does not ship as a header:
it is `src/pipewire/version.h.in` with meson's four substitutions applied
(`1`, `0`, `5`, `"0.3"`), which is exactly the file the upstream `-dev` package
installs.

## Licence obligations

`MIT` is already an accepted identifier in `about.toml` and `deny.toml`. The
project is not a package in the `cargo metadata` graph, so `cargo about` cannot
see it: it reaches `THIRD-PARTY-NOTICES.md` through
`VENDORED_CPP_PROJECTS` in `xtask/src/generate_third_party_notices.rs`, which
reproduces `COPYING` out of this directory, and the shipped wheel is checked for
it by `sdk/streamlib-python-wheel/tests/test_third_party_notices.py`.

## Upgrading

Re-run the extraction against a new upstream tag, regenerate `version.h` from
that tag's `version.h.in`, and check that
`sdk/streamlib-python-wheel/tests/test_wheel_portability.py` still passes with
no name added to `LIBRARIES_THE_HOST_MAY_SUPPLY`. Bind no symbol newer than
PipeWire 0.3.50 without a `dlsym` presence probe: the headers state what the API
looks like, not what the host's library actually exports.
