---
whoami: amos
name: "@github:tatolab/streamlib#198"
description: Linux — widen CLAP plugin host to cross-platform
dependencies:
  - "down:@github:tatolab/streamlib#192"
adapters:
  github: builtin
---

@github:tatolab/streamlib#198

CLAP plugin host, scanner, and effect processor are macOS-gated but CLAP is cross-platform. The scanner already has Linux search paths built in.

### Key work
- Remove `#[cfg(any(target_os = "macos", target_os = "ios"))]` from `core/clap/mod.rs` (host, scanner modules + re-exports)
- Remove gates from `core/processors/clap_effect.rs` (ClapPluginHost import, ClapPluginInfo/ClapScanner re-exports)
- Remove gates from `core/processors/mod.rs` (clap_effect module + re-exports)
- Remove gates from `lib.rs` (ClapEffectProcessor, ClapPluginInfo, ClapScanner re-exports)
- Investigate `clack_host::bundle::PluginBundle` — was missing on Linux, may need clack crate update or conditional compilation
- Test loading a CLAP plugin on Linux

### AI context (2026-03-24)
- Gates were added in fix/linux-compilation PR as temporary scaffolding
- `clack_host::bundle` API wasn't available on Linux build — root cause needs investigation (git dependency version? platform-specific module?)
- `clack-host` and `clack-extensions` are unconditional Cargo.toml dependencies (not platform-gated)

### Depends on
- #192 (widen cfg gates) — complete
