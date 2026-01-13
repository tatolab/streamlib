---
description: Install or reinstall the broker service
allowed-tools:
  - Bash
argument-hint: "[--force]"
---

Install the broker service:

- Normal install: `streamlib broker install`
- Force reinstall (for updates or protocol mismatch): `streamlib broker install --force`

After install, verify with `streamlib broker status`.
