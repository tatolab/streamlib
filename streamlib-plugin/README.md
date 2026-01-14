# StreamLib Claude Code Plugin

Runtime and broker debugging.

## Commands

| Command | Description |
|---------|-------------|
| `/streamlib:status` | Broker health, version, uptime |
| `/streamlib:runtimes` | List registered runtimes |
| `/streamlib:processors` | List subprocess processors |
| `/streamlib:connections` | List XPC connections |
| `/streamlib:logs` | View broker logs |
| `/streamlib:install` | Install/reinstall broker |

## Skills

| Skill | Triggers On |
|-------|-------------|
| debug | Runtime errors, broker issues, XPC failures |

## Hooks

| Event | Behavior |
|-------|----------|
| Stop | Suggests diagnostics if runtime issues were unresolved |
