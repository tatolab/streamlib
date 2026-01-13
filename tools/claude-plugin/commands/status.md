---
description: Quick broker and runtime status check
---

# Broker Status

Check the StreamLib broker status using the MCP tools.

Use the `broker_status` MCP tool to get:
- Health status (healthy/unhealthy)
- Version information
- Uptime
- Protocol version

Then use `broker_runtimes` to see registered runtimes.

If the broker is not running, inform the user they need to run:
```
streamlib broker install
```

Present the status in a clear, concise format.
