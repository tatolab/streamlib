# Telemetry

StreamLib has built-in OpenTelemetry-based observability. All runtimes, the broker, and Python subprocesses produce structured logs and traces that flow through a single pipeline.

## Architecture

```
Runtime ──► gRPC ──► Broker ──► SQLite (~/.streamlib/telemetry.db)
                        │
                        └──► OTLP export (any backend)

CLI ──────► SQLite (read-only queries)
```

- **Broker** is the single telemetry writer — no SQLite lock contention
- **Runtime** sends spans and logs to the broker via gRPC on startup
- **CLI** reads from SQLite for local queries
- **OTLP export** is optional — point the broker at any OTLP-compatible backend

## Quick Start

Telemetry works out of the box after `./scripts/dev-setup.sh`. The broker collects telemetry from all runtimes into `~/.streamlib/telemetry.db`.

### Query logs locally

```bash
# Recent logs from all services
streamlib telemetry logs

# Filter by service and time
streamlib telemetry logs --service streamlib-runtime --since 1h -n 50

# Filter by severity (5=DEBUG, 9=INFO, 13=WARN, 17=ERROR)
streamlib telemetry logs --severity 13

# Query spans
streamlib telemetry spans --since 1h

# Clean up old data
streamlib telemetry prune --older-than 7d
```

## Connecting to an External Dashboard

StreamLib exports telemetry via standard [OTLP](https://opentelemetry.io/docs/specs/otlp/) (OpenTelemetry Protocol). Any OTLP-compatible backend works:

- **Grafana Tempo** (traces) + **Grafana Loki** (logs)
- **Jaeger**
- **Datadog**
- **Honeycomb**
- **New Relic**
- **Splunk**
- Self-hosted OpenTelemetry Collector

### Setup

Set `STREAMLIB_OTLP_ENDPOINT` on the **broker** process. All telemetry ingested from runtimes will be forwarded to that endpoint in addition to being stored in SQLite.

```bash
# Start the broker with OTLP forwarding
STREAMLIB_OTLP_ENDPOINT=http://localhost:4317 ./.streamlib/bin/streamlib-broker

# Or for a remote endpoint
STREAMLIB_OTLP_ENDPOINT=https://otel.example.com:4317 ./.streamlib/bin/streamlib-broker
```

No changes needed on runtimes — they send telemetry to the broker, and the broker handles OTLP forwarding.

### Backfill Historical Data

If you set up a dashboard after telemetry has been collecting in SQLite, you can backfill:

```bash
# Export last 7 days of spans to your OTLP endpoint
streamlib telemetry export --endpoint http://localhost:4317 --since 7d

# Export only a specific service
streamlib telemetry export --endpoint http://localhost:4317 --since 24h --service streamlib-runtime
```

## Environment Variables

| Variable | Set on | Description |
|----------|--------|-------------|
| `STREAMLIB_OTLP_ENDPOINT` | Broker | OTLP gRPC endpoint for forwarding (e.g., `http://localhost:4317`) |
| `STREAMLIB_BROKER_PORT` | Runtime | Broker port for telemetry ingestion (default: `50051`, dev: `50052`) |
| `RUST_LOG` | Any | Log level filter (default: `info`) |

## Database

Telemetry is stored in `~/.streamlib/telemetry.db` (SQLite, WAL mode). Two tables:

- **`spans`** — trace spans with trace_id, span_id, parent, timing, status, attributes
- **`logs`** — log records with timestamp, severity, body, service_name, trace context

The broker prunes records older than 7 days automatically (hourly check).

### Direct SQL access

```bash
sqlite3 ~/.streamlib/telemetry.db

-- Recent errors
SELECT service_name, body FROM logs
WHERE severity_text = 'ERROR'
ORDER BY timestamp_unix_ns DESC LIMIT 10;

-- Slowest spans
SELECT operation_name, service_name, duration_ns / 1000000.0 as ms
FROM spans ORDER BY duration_ns DESC LIMIT 10;

-- Service health summary
SELECT service_name, severity_text, COUNT(*) as cnt
FROM logs GROUP BY service_name, severity_text;
```
