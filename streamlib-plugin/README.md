# StreamLib Claude Code Plugin

Runtime and broker debugging.

## Commands

| Command | Description |
|---------|-------------|
| `/streamlib:status` | Broker health, version, uptime |
| `/streamlib:runtimes` | List registered runtimes |
| `/streamlib:processors` | List processors |
| `/streamlib:logs` | View broker logs |
| `/streamlib:install` | Install/reinstall broker |

## Skills

| Skill | Triggers On |
|-------|-------------|
| debug | Runtime errors, broker issues |

## Hooks

| Event | Behavior |
|-------|----------|
| Stop | Suggests diagnostics if runtime issues were unresolved |
