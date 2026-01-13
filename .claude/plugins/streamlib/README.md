# StreamLib Claude Code Plugin

This plugin provides Claude Code with native access to StreamLib broker management, runtime inspection, and debugging tools.

## Installation

This plugin is located in `.claude/plugins/streamlib/` and is automatically discovered by Claude Code when working in the StreamLib repository. No manual installation required.

## Prerequisites

The plugin requires the `streamlib` CLI to be built:

```bash
cargo build --release -p streamlib-cli
```

Or use the dev setup script which builds and installs everything:

```bash
./scripts/dev-setup.sh
```

## Available Tools (MCP)

| Tool | Description |
|------|-------------|
| `broker_status` | Get broker health, version, uptime |
| `broker_runtimes` | List registered StreamLib runtimes |
| `broker_processors` | List subprocess processors |
| `broker_connections` | List active XPC connections |
| `broker_logs` | Get recent broker log entries |
| `broker_install` | Install/reinstall broker service |

## Available Skills

| Skill | Description |
|-------|-------------|
| `/debug` | Enter StreamLib debug mode with guided diagnostics |

## Available Commands

| Command | Description |
|---------|-------------|
| `/status` | Quick broker and runtime status check |

## Usage in Claude Code

Once installed, Claude Code will automatically have access to the StreamLib MCP tools. You can:

- Ask "What's the broker status?" - Uses `broker_status` tool
- Ask "Show me the registered runtimes" - Uses `broker_runtimes` tool
- Use `/debug` to enter debug mode for troubleshooting
- Use `/status` for a quick health check
