---
description: List active XPC connections between runtimes and subprocesses
allowed-tools:
  - Bash
argument-hint: "[--runtime <id>]"
---

Run `streamlib broker connections` to list all active XPC connections.

If the user specifies a runtime ID, use `streamlib broker connections --runtime <id>` to filter.

No connections typically means XPC setup failed - suggest checking broker logs.
