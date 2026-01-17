---
description: View broker logs from /tmp/streamlib-broker.log
allowed-tools:
  - Bash
argument-hint: "[lines] [--follow] [--errors]"
---

View broker logs based on user request. The dev log file is `/tmp/streamlib-broker-dev-streamlib.log`.

- Default: `tail -50 /tmp/streamlib-broker-dev-streamlib.log`
- With line count: `tail -<n> /tmp/streamlib-broker-dev-streamlib.log`
- Errors only: `grep -i error /tmp/streamlib-broker-dev-streamlib.log | tail -50`
- Follow/stream: `tail -f /tmp/streamlib-broker-dev-streamlib.log` with `run_in_background: true`

Common error patterns to look for:
- "Failed to connect" - Network/service issues
- "Endpoint lookup failed" - Broker not advertising
- "Connection interrupted" - Broker restarted
- "Protocol version mismatch" - Run `./scripts/dev-setup.sh --clean`
