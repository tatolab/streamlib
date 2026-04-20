---
whoami: amos
name: '@github:tatolab/streamlib#168'
description: Linux — Dev environment & CI pipeline
adapters:
  github: builtin
blocks:
- '@github:tatolab/streamlib#166'
---

@github:tatolab/streamlib#168

Phase 6 of the Linux support plan. Dev environment and CI infrastructure.

### AI context (2026-03-21)
- `cargo check -p streamlib` compiles cleanly on Linux (Vulkan auto-detected)
- CI is macOS-only (`macos-14` in `.github/workflows/test.yml`)
- `scripts/dev-setup.sh` explicitly rejects Linux (`uname != Darwin`)
- Pre-existing test issue: `with_env_filter` method not found (not from Linux changes)

### Work
- Update `dev-setup.sh` to detect platform, generate systemd user service instead of launchd
- CI jobs: `cargo build -p streamlib` + `cargo test -p streamlib` on Linux
- Document system deps per distro (Vulkan SDK; FFmpeg when #167 lands)

### Depends on
#166 (Linux processors) — CI needs something to build and test
