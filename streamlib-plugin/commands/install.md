---
description: Install or reinstall the streamlib dev environment
allowed-tools:
  - Bash
argument-hint: "[--clean]"
---

Install the dev environment (CLI + runtime proxy scripts):

- Normal install: `./scripts/dev-setup.sh`
- Clean reinstall (rebuilds proxies): `./scripts/dev-setup.sh --clean`

After install, verify with `./.streamlib/bin/streamlib --help`.
