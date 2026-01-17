---
description: Install or reinstall the broker service
allowed-tools:
  - Bash
argument-hint: "[--clean]"
---

Install the dev broker service:

- Normal install: `./scripts/dev-setup.sh`
- Clean reinstall (for updates or fixes): `./scripts/dev-setup.sh --clean`

After install, verify with `./.streamlib/bin/streamlib broker status`.
