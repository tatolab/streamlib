---
whoami: amos
name: '@github:tatolab/streamlib#361'
adapters:
  github: builtin
description: Fix flaky test_concurrent_publish_from_multiple_threads (iceoryx2 teardown race) — 'Test passes in isolation but flakes under parallel test suite due to iceoryx2 service teardown race. Isolate via per-test Iceoryx2Node or serial_test. Pre-existing infrastructure issue; not caused by any particular PR.'
github_issue: 361
---

@github:tatolab/streamlib#361

See the GitHub issue for full context.

## Priority

medium

## Parent

#322 / #319 umbrella (for capability-split-related) or infrastructure.
