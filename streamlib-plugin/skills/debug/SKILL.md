---
description: Use this skill when debugging StreamLib runtime issues or broker problems. Activates on "broker not running", "runtime error", "gRPC error".
---

# StreamLib Debugging

Use these commands to diagnose issues:

| Command | Purpose |
|---------|---------|
| `/streamlib:status` | Check broker health |
| `/streamlib:runtimes` | List registered runtimes |
| `/streamlib:processors` | List processors |
| `/streamlib:logs` | View broker logs |
| `/streamlib:install` | Install/reinstall broker |

## Diagnostic Flow

1. `/streamlib:status` - Is broker running?
2. `/streamlib:runtimes` - Any runtimes registered?
3. `/streamlib:logs --errors` - What errors occurred?
