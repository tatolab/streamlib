---
description: Use this skill when debugging StreamLib runtime issues, broker problems, XPC failures, or subprocess errors. Activates on "broker not running", "runtime error", "XPC failed", "subprocess not connecting".
---

# StreamLib Debugging

Use these commands to diagnose issues:

| Command | Purpose |
|---------|---------|
| `/streamlib:status` | Check broker health |
| `/streamlib:runtimes` | List registered runtimes |
| `/streamlib:processors` | List subprocess processors |
| `/streamlib:connections` | List XPC connections |
| `/streamlib:logs` | View broker logs |
| `/streamlib:install` | Install/reinstall broker |

## Diagnostic Flow

1. `/streamlib:status` - Is broker running?
2. `/streamlib:runtimes` - Any runtimes registered?
3. `/streamlib:connections` - Are subprocesses connected?
4. `/streamlib:logs --errors` - What errors occurred?
