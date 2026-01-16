# Phase 4 Integration Test: End-to-End Verification

## Your Mission
Verify Phase 4 implementation works end-to-end by running the `camera-python-display` example. You are a TESTER, not a developer. Do NOT fix bugs by changing the design.

## Critical Rules

1. **NO DESIGN CHANGES**: If something doesn't work, report it as a blocker. Do NOT:
   - Add workarounds
   - Change the architecture
   - "Fix" things by deviating from the spec

2. **REPORT EXACT ERRORS**: Copy full error messages and stack traces.

## Test Procedure

### Step 1: Restart Broker
```bash
# Ensure broker has latest code
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.tatolab.streamlib.broker.dev-*.plist
sleep 2
./.streamlib/bin/streamlib broker status
```

### Step 2: Build Everything
```bash
cargo build -p streamlib -p streamlib-python -p streamlib-broker -p camera-python-display
```

### Step 3: Run Example
```bash
RUST_LOG=debug cargo run -p camera-python-display
```

### Step 4: Verify Connection Flow
Monitor broker logs in another terminal:
```bash
./.streamlib/bin/streamlib broker logs -f
```

Expected log sequence:
1. "Connection allocated: conn-XXX"
2. "Host alive for connection conn-XXX"
3. "Host XPC endpoint stored"
4. "Client alive for connection conn-XXX"
5. "Client received endpoint"
6. "Host acked"
7. "Client acked"
8. "Connection ready"

### Step 5: Verify Frame Flow
- Camera window should appear
- Python processing should be visible (effects applied)
- No "bridge not ready" warnings after initial startup

## Success Criteria

- [ ] Broker starts and shows healthy status
- [ ] camera-python-display builds without errors
- [ ] Example runs without panics
- [ ] Connection reaches "ready" state in broker logs
- [ ] Video frames flow through Python processor
- [ ] Clean shutdown on Cmd+Q

## Completion

If ALL criteria pass, output exactly:
PHASE_4_INTEGRATION_COMPLETE

If any test fails, output:
INTEGRATION_FAILURE: [step that failed]
ERROR: [exact error message]

Do NOT attempt to fix failures. Report and stop.

