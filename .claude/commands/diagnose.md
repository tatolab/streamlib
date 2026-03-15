# /diagnose — StreamLib Telemetry Diagnostics

Query the StreamLib telemetry database (`~/.streamlib/telemetry.db`) to diagnose runtime issues.

## Instructions

1. **Check if the database exists**:
   ```bash
   ls -la ~/.streamlib/telemetry.db
   ```

2. **Run diagnostic queries** using `sqlite3` CLI:

### Recent errors (last 5 minutes)
```sql
sqlite3 ~/.streamlib/telemetry.db "
SELECT datetime(timestamp_unix_ns/1000000000, 'unixepoch') as time,
       severity_text, service_name, body
FROM logs
WHERE severity_number >= 17
  AND timestamp_unix_ns > (strftime('%s','now') - 300) * 1000000000
ORDER BY timestamp_unix_ns DESC
LIMIT 20;
"
```

### Failed spans
```sql
sqlite3 ~/.streamlib/telemetry.db "
SELECT datetime(start_time_unix_ns/1000000000, 'unixepoch') as time,
       service_name, operation_name, status_message,
       duration_ns/1000000 as duration_ms
FROM spans
WHERE status_code = 'Error'
ORDER BY start_time_unix_ns DESC
LIMIT 20;
"
```

### Service health (log volume by service, last hour)
```sql
sqlite3 ~/.streamlib/telemetry.db "
SELECT service_name,
       COUNT(*) as total,
       SUM(CASE WHEN severity_number >= 17 THEN 1 ELSE 0 END) as errors,
       SUM(CASE WHEN severity_number >= 13 AND severity_number < 17 THEN 1 ELSE 0 END) as warnings
FROM logs
WHERE timestamp_unix_ns > (strftime('%s','now') - 3600) * 1000000000
GROUP BY service_name;
"
```

### Slowest spans (last hour)
```sql
sqlite3 ~/.streamlib/telemetry.db "
SELECT service_name, operation_name,
       duration_ns/1000000 as duration_ms,
       status_code
FROM spans
WHERE start_time_unix_ns > (strftime('%s','now') - 3600) * 1000000000
ORDER BY duration_ns DESC
LIMIT 10;
"
```

### Database size and row counts
```sql
sqlite3 ~/.streamlib/telemetry.db "
SELECT 'logs' as tbl, COUNT(*) as rows FROM logs
UNION ALL
SELECT 'spans', COUNT(*) FROM spans;
"
```

3. **Analyze the results** and report:
   - Any error patterns (repeated errors, specific services failing)
   - Performance issues (slow spans)
   - Missing services (expected services not logging)
   - Suggestions for resolution

## Common failure patterns

| Pattern | Likely cause | Fix |
|---------|-------------|-----|
| No rows in database | Telemetry not initialized | Check runtime/broker started with streamlib-telemetry |
| Broker errors only | Broker crashed or not running | `streamlib broker status` |
| Python service errors | Subprocess FFI issues | Check native lib path, iceoryx2 services |
| High error rate on one service | Processor bug | Check specific error messages |
| No spans, only logs | Tracing not instrumented | Add `#[tracing::instrument]` to key functions |

## Using the CLI

```bash
# Query logs
streamlib telemetry logs --since 5m
streamlib telemetry logs --service runtime --severity 17

# Query spans
streamlib telemetry spans --since 1h --status Error

# Cleanup old data
streamlib telemetry prune --older-than 7d
```
