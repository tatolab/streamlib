---
description: Debug StreamLib runtime and broker issues
---

# StreamLib Debug Mode

You are now in StreamLib debug mode. Use this skill to diagnose issues with the broker, runtimes, and frame transport.

## Available MCP Tools

Use the `streamlib` MCP server tools for structured data:

- `broker_status` - Get broker health, version, uptime
- `broker_runtimes` - List registered runtimes with processor/connection counts
- `broker_connections` - List active XPC connections
- `broker_logs` - Get recent broker log entries

## Diagnostic Workflow

### 1. Check Broker Health
First, verify the broker is running and healthy:
```
Use: broker_status tool
```
If broker is not running, advise user to run `streamlib broker install`.

### 2. Check Registered Runtimes
List all runtimes registered with the broker:
```
Use: broker_runtimes tool
```
No runtimes? The application may not have started, or crashed before registering.

### 3. Check Connections
View active subprocess connections:
```
Use: broker_connections tool
```
This shows which processors have established XPC channels.

### 4. Check Logs
Get recent broker log entries for errors:
```
Use: broker_logs tool with lines=50
```

For real-time log monitoring, use:
```bash
tail -f /tmp/streamlib-broker.log
```
(Run in background with Bash tool's run_in_background option)

## Common Issues

### "Broker not running" Error
```
streamlib broker install
```

### Protocol Version Mismatch
```
streamlib broker install --force
```

### Runtime Not Registering
Check that:
1. Application creates `StreamRuntime::new()`
2. Broker is running before application starts
3. No XPC connection errors in logs

### XPC Connection Failures
Look for in logs:
- "Failed to connect to broker"
- "Endpoint lookup failed"
- "Connection interrupted"

Usually caused by broker restart - runtimes need to reconnect.

## Log Locations

| Log | Path |
|-----|------|
| Broker | `/tmp/streamlib-broker.log` |
| launchd | `log show --predicate 'subsystem == "com.tatolab.streamlib"'` |

## Architecture Reference

```
Application (StreamRuntime)
    ↓ registers endpoint
Broker (launchd service @ com.tatolab.streamlib.broker)
    ↓ provides endpoint to
Subprocess (e.g., Python processor)
    ↓ direct XPC connection
Application (zero-copy frame transfer)
```
