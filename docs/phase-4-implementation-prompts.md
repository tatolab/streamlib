# Phase 4 Implementation Prompts

Use these prompts sequentially with Ralph Loop sessions. Each phase builds on the previous.

---

## Pre-Flight Checklist (Before Any Phase)

```bash
# 1. Verify broker is running
./.streamlib/bin/streamlib broker status

# 2. Verify clean git state
git status

# 3. Verify planning doc exists
cat docs/phase-4-xpc-bridge-core-planning.md | head -20

# 4. Verify on correct branch
git branch --show-current  # Should be: phase-4-xpc-bridge or similar
```

---

## Phase 4a: Broker Side

**Max Iterations:** 15
**Completion Signal:** `PHASE_4A_COMPLETE`
**Estimated Scope:** broker.proto, state.rs, grpc_service.rs

### Prompt

```
# Phase 4a Implementation: Broker-Side XPC Bridge

## Your Mission
Implement Phase 4a (Broker Side) from the planning document. You are a CODE IMPLEMENTER, not a designer. The architecture is FINAL.

## Critical Rules - READ FIRST

1. **DESIGN IS FROZEN**: The architecture in `docs/phase-4-xpc-bridge-core-planning.md` is the ONLY source of truth. Do NOT:
   - Propose alternative approaches
   - "Improve" or "simplify" the design
   - Skip steps because they seem unnecessary
   - Add features not in the spec

2. **VERIFY BEFORE EVERY CHANGE**: Before writing code, quote the relevant section from the planning doc that authorizes that change.

3. **NO NEW ABSTRACTIONS**: Per CLAUDE.md, you are BANNED from creating new helper methods, utility functions, structs, or traits beyond what the spec explicitly defines.

4. **INCREMENTAL COMMITS**: After each logical unit of work, create a git commit. Do not batch large changes.

## Phase 4a Tasks (from spec)

1. **Update broker.proto** with new RPCs:
   - AllocateConnection
   - HostAlive / HostXpcReady
   - ClientAlive (NO ClientXpcReady - single connection pattern)
   - GetClientStatus / GetHostStatus
   - MarkAcked
   - GetConnectionInfo
   - CloseConnection

2. **Update state.rs**:
   - Add `HostState` enum (Pending, Alive, XpcReady, Acked, Failed)
   - Add `ClientState` enum (Pending, Alive, WaitingForEndpoint, XpcEndpointReceived, Acked, Failed)
   - Add `Connection` struct (copy EXACTLY from spec)
   - Add `DerivedConnectionState` enum
   - Add `is_timed_out()` method

3. **Update grpc_service.rs**:
   - Implement all new RPC handlers
   - State transitions with proper locking

4. **Add background task**:
   - `monitor_stale_connections()` - cleanup timed out connections
   - Run every 30s

## Verification Checklist (before signaling complete)

- [ ] `cargo build -p streamlib-broker` succeeds
- [ ] `cargo clippy -p streamlib-broker` has no errors
- [ ] All protobuf messages match spec exactly
- [ ] All enums match spec exactly
- [ ] Connection struct has ALL fields from spec
- [ ] No extra abstractions were added
- [ ] Each task has its own commit

## Files You Will Modify

- `libs/streamlib-broker/proto/broker.proto`
- `libs/streamlib-broker/src/state.rs`
- `libs/streamlib-broker/src/grpc_service.rs`
- `libs/streamlib-broker/src/proto/streamlib.broker.rs` (regenerated)

## Starting Point

1. First, read the planning doc: `docs/phase-4-xpc-bridge-core-planning.md`
2. Read current broker.proto to understand existing structure
3. Read current state.rs and grpc_service.rs
4. Begin with Task 1 (broker.proto updates)

## Completion

When ALL tasks are done and verified, output exactly:
PHASE_4A_COMPLETE

If you encounter a blocker that requires design decisions, STOP and output:
BLOCKER: [description of issue]

Do NOT proceed past blockers by making your own design decisions.
```

### Post-Phase 4a Verification

```bash
# After PHASE_4A_COMPLETE, verify manually:
cargo build -p streamlib-broker
cargo test -p streamlib-broker
git log --oneline -10  # Review commits
```

---

## Phase 4b: Host Processor Side

**Max Iterations:** 25
**Completion Signal:** `PHASE_4B_COMPLETE`
**Estimated Scope:** Python host processors, XPC listener setup, bridge task, PyO3 bindings

### Prompt

```
# Phase 4b Implementation: Host Processor Side + PyO3 XPC Bindings

## Your Mission
Implement Phase 4b (Host Processor Side) from the planning document. This includes the Rust host processors AND the PyO3 XPC bindings. You are a CODE IMPLEMENTER, not a designer. The architecture is FINAL.

## Critical Rules - READ FIRST

1. **DESIGN IS FROZEN**: The architecture in `docs/phase-4-xpc-bridge-core-planning.md` is the ONLY source of truth. Do NOT:
   - Propose alternative approaches
   - "Improve" or "simplify" the design
   - Skip steps because they seem unnecessary
   - Add features not in the spec

2. **VERIFY BEFORE EVERY CHANGE**: Before writing code, quote the relevant section from the planning doc that authorizes that change.

3. **NO NEW ABSTRACTIONS**: Per CLAUDE.md, you are BANNED from creating new helper methods, utility functions, structs, or traits beyond what the spec explicitly defines.

4. **INCREMENTAL COMMITS**: After each logical unit of work, create a git commit. Do not batch large changes.

5. **ALL THREE PROCESSOR TYPES**: You MUST implement the XPC bridge for ALL three host processor types, not just one:
   - `PythonManualProcessor` - defer `start()` until bridge ready
   - `PythonReactiveProcessor` - drop frames until bridge ready
   - `PythonContinuousProcessor` - yield/sleep until bridge ready

## Phase 4b Tasks (from spec)

### Part 1: Host Processor Bridge Logic

1. **Connection allocation in setup()** (same for all three types):
   - Call `AllocateConnection` to get `connection_id`
   - Set `STREAMLIB_CONNECTION_ID` env var for subprocess
   - Set `STREAMLIB_BROKER_ENDPOINT` env var

2. **XPC listener setup**:
   - Create anonymous XPC listener using pattern from spec
   - Store endpoint via XPC to broker
   - Call `HostXpcReady` via gRPC

3. **Spawn subprocess**:
   - Launch Python with env vars set

4. **Bridge task (async, use tokio::spawn)**:
   - Poll `GetClientStatus` until client connects
   - ACK exchange: send ping (0x53 0x4C 0x50), wait for pong (0x53 0x4C 0x41)
   - Call `MarkAcked(side="host")` via gRPC
   - Set `bridge_ready` flag to true

5. **Frame gate per processor type**:
   - `PythonManualProcessor`: defer `start()`, store flag, forward when ready
   - `PythonReactiveProcessor`: drop frames silently if not ready
   - `PythonContinuousProcessor`: yield/sleep in loop if not ready

### Part 2: PyO3 XPC Bindings (streamlib-python)

1. **Create XpcConnection PyO3 wrapper** (`libs/streamlib-python/src/xpc_bindings.rs`):
   - `XpcConnection` class wrapping `xpc_connection_t`
   - `connect_to_endpoint(endpoint_bytes: bytes) -> XpcConnection`
   - Connection state management

2. **Frame I/O methods**:
   - `send_frame(frame_dict: dict, schema: Schema)` - serialize dict → XPC
   - `receive_frame(schema: Schema) -> dict` - receive XPC → dict
   - `try_receive_frame()` - non-blocking variant

3. **Schema support**:
   - `Schema` PyO3 class or JSON-based schema passing
   - Field type mapping per spec's FieldType → XPC Type table

4. **Export in wheel** (`libs/streamlib-python/src/lib.rs`):
   - Add `xpc` submodule to PyO3 module

## Verification Checklist (before signaling complete)

- [ ] `cargo build -p streamlib` succeeds
- [ ] `cargo build -p streamlib-python` succeeds
- [ ] `cargo clippy -p streamlib -p streamlib-python` has no errors
- [ ] All three Python host processors have bridge logic
- [ ] Each processor type has correct "before ready" behavior
- [ ] Anonymous XPC listener pattern matches spec exactly
- [ ] ACK ping/pong uses correct magic bytes
- [ ] PyO3 XpcConnection class is exported
- [ ] No extra abstractions were added
- [ ] Each major task has its own commit

## Files You Will Modify

Host Processors:
- `libs/streamlib-python/src/host/manual.rs` (or equivalent)
- `libs/streamlib-python/src/host/reactive.rs` (or equivalent)
- `libs/streamlib-python/src/host/continuous.rs` (or equivalent)

PyO3 Bindings:
- `libs/streamlib-python/src/xpc_bindings.rs` (NEW - per spec)
- `libs/streamlib-python/src/lib.rs`

Supporting:
- Any existing XPC code in `libs/streamlib/src/apple/subprocess_rhi/`

## Starting Point

1. Read the planning doc: `docs/phase-4-xpc-bridge-core-planning.md`
2. Read the "Host Processor Side (Phase 4b)" section carefully
3. Read the "PyO3 XPC Bindings" section carefully
4. Examine existing Python host processor code structure
5. Begin with connection allocation in setup()

## Completion

When ALL tasks are done and verified, output exactly:
PHASE_4B_COMPLETE

If you encounter a blocker that requires design decisions, STOP and output:
BLOCKER: [description of issue]

Do NOT proceed past blockers by making your own design decisions.
```

### Post-Phase 4b Verification

```bash
# After PHASE_4B_COMPLETE, verify manually:
cargo build -p streamlib -p streamlib-python
cargo test -p streamlib-python
git log --oneline -15  # Review commits
```

---

## Phase 4c: Client Processor Side (Python Subprocess)

**Max Iterations:** 20
**Completion Signal:** `PHASE_4C_COMPLETE`
**Estimated Scope:** _subprocess_runner.py rewrite, Python gRPC client generation

### Prompt

```
# Phase 4c Implementation: Client Processor Side (Python Subprocess)

## Your Mission
Implement Phase 4c (Client Processor Side) from the planning document. This is the Python subprocess that connects back to the host. You are a CODE IMPLEMENTER, not a designer. The architecture is FINAL.

## Critical Rules - READ FIRST

1. **DESIGN IS FROZEN**: The architecture in `docs/phase-4-xpc-bridge-core-planning.md` is the ONLY source of truth. Do NOT:
   - Propose alternative approaches
   - "Improve" or "simplify" the design
   - Skip steps because they seem unnecessary
   - Add features not in the spec

2. **VERIFY BEFORE EVERY CHANGE**: Before writing code, quote the relevant section from the planning doc that authorizes that change.

3. **COMPLETE REWRITE of _subprocess_runner.py**: The old Unix socket code must be DELETED. New architecture uses:
   - `STREAMLIB_CONNECTION_ID` env var
   - `STREAMLIB_BROKER_ENDPOINT` env var
   - gRPC calls to broker
   - XPC via PyO3 wheel bindings

4. **INCREMENTAL COMMITS**: After each logical unit of work, create a git commit. Do not batch large changes.

5. **ALL THREE EXECUTION MODES**: The subprocess must support all three modes:
   - Manual mode: Wait for `start()` command
   - Reactive mode: Process frames as they arrive
   - Continuous mode: Run processing loop

## Phase 4c Tasks (from spec)

### Part 1: Generate Python gRPC Client

1. **Generate stubs from broker.proto**:
   ```bash
   python -m grpc_tools.protoc \
     -I libs/streamlib-broker/proto \
     --python_out=libs/streamlib-python/python/streamlib/_generated \
     --grpc_python_out=libs/streamlib-python/python/streamlib/_generated \
     broker.proto
   ```

2. **Update pyproject.toml** dependencies:
   - Add `grpcio>=1.60.0`
   - Add `grpcio-tools>=1.60.0` (for regeneration)

### Part 2: Rewrite _subprocess_runner.py

1. **Startup** (same for all modes):
   - Read `STREAMLIB_CONNECTION_ID` from env
   - Read `STREAMLIB_BROKER_ENDPOINT` from env
   - Connect to broker via gRPC

2. **Registration & Connection**:
   - Call `ClientAlive` immediately via gRPC
   - Request host's XPC endpoint from broker via XPC interface (`get_endpoint`)
   - Use PyO3 XPC bindings (from Phase 4b) to connect to host's endpoint
   - NO ClientXpcReady needed - single bidirectional connection

3. **ACK Exchange**:
   - Wait for ACK ping from host (magic bytes: 0x53 0x4C 0x50 "SLP")
   - Send ACK pong back (magic bytes: 0x53 0x4C 0x41 "SLA")
   - Call `MarkAcked(side="client")` via gRPC

4. **Execution Mode Handling**:
   - Manual mode: Wait for `start()` control message, then begin
   - Reactive mode: Process frames as they arrive via XPC
   - Continuous mode: Run processing loop, send/receive continuously

### Part 3: Port Proxies (Transparent Serialization)

1. **InputPortProxy.get()**:
   - Internally calls XPC receive
   - Deserializes to Python dict using schema
   - Python processor sees normal dict

2. **OutputPortProxy.set()**:
   - Takes Python dict from processor
   - Serializes to XPC using schema
   - Sends via XPC connection

## Code to DELETE (Old Unix Socket Architecture)

Remove ALL of these from _subprocess_runner.py:
- `--control-socket` argument handling
- `--frames-socket` argument handling
- `IpcChannel` class
- `socket.socket` imports/usage
- JSON message encoding over sockets

## Verification Checklist (before signaling complete)

- [ ] Python gRPC stubs generated successfully
- [ ] `_subprocess_runner.py` has NO Unix socket code remaining
- [ ] Subprocess reads `STREAMLIB_CONNECTION_ID` from env
- [ ] Subprocess reads `STREAMLIB_BROKER_ENDPOINT` from env
- [ ] `ClientAlive` called on startup
- [ ] XPC endpoint retrieved via broker XPC interface
- [ ] ACK ping/pong uses correct magic bytes
- [ ] `MarkAcked(side="client")` called after ACK
- [ ] All three execution modes supported
- [ ] InputPortProxy/OutputPortProxy use XPC serialization
- [ ] pyproject.toml has grpcio dependencies
- [ ] No extra abstractions were added
- [ ] Each major task has its own commit

## Files You Will Modify

- `libs/streamlib-python/python/streamlib/_subprocess_runner.py` (REWRITE)
- `libs/streamlib-python/python/streamlib/_generated/` (NEW - generated)
- `libs/streamlib-python/pyproject.toml`

## Starting Point

1. Read the planning doc: `docs/phase-4-xpc-bridge-core-planning.md`
2. Read the "Client Processor Side (Phase 4c)" section carefully
3. Read the "Python gRPC Client" section
4. Examine current _subprocess_runner.py to understand what to delete
5. Begin with Python gRPC stub generation

## Completion

When ALL tasks are done and verified, output exactly:
PHASE_4C_COMPLETE

If you encounter a blocker that requires design decisions, STOP and output:
BLOCKER: [description of issue]

Do NOT proceed past blockers by making your own design decisions.
```

### Post-Phase 4c Verification

```bash
# After PHASE_4C_COMPLETE, verify manually:
cd libs/streamlib-python && uv sync
cargo build -p streamlib-python
git log --oneline -15  # Review commits

# Verify no Unix socket remnants
grep -r "unix\|socket.socket\|control-socket\|frames-socket" \
  libs/streamlib-python/python/streamlib/_subprocess_runner.py
# Should return nothing
```

---

## Phase 4 Integration Test

**Max Iterations:** 10
**Completion Signal:** `PHASE_4_INTEGRATION_COMPLETE`
**Estimated Scope:** End-to-end test with camera-python-display example

### Prompt

```
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
```

---

## Quick Reference: Ralph Loop Commands

```bash
# Start Ralph Loop
/ralph-loop

# When prompted for max iterations, enter the number for that phase:
# Phase 4a: 15
# Phase 4b: 25
# Phase 4c: 20
# Integration: 10

# Then paste the prompt for that phase

# To cancel mid-run:
/cancel-ralph
```

---

## Troubleshooting

### Agent Deviates from Spec
If the agent starts making design decisions or adding abstractions:
1. `/cancel-ralph`
2. Review what was done: `git diff HEAD~5`
3. Revert if necessary: `git reset --hard HEAD~N`
4. Restart with stronger emphasis on following spec

### Agent Gets Stuck in Loop
If iterations are being wasted on the same error:
1. `/cancel-ralph`
2. Check the error manually
3. If it's a spec ambiguity, update the planning doc
4. Restart the phase

### Blocker Reported
If agent outputs `BLOCKER:`:
1. Read the blocker description
2. Make a design decision and update the planning doc
3. Restart the phase with clarification added to prompt

---

## Summary

| Phase | Iterations | Signal | Main Deliverables |
|-------|------------|--------|-------------------|
| 4a | 15 | `PHASE_4A_COMPLETE` | broker.proto, state.rs, grpc_service.rs |
| 4b | 25 | `PHASE_4B_COMPLETE` | Host processors, PyO3 XPC bindings |
| 4c | 20 | `PHASE_4C_COMPLETE` | _subprocess_runner.py, Python gRPC client |
| Test | 10 | `PHASE_4_INTEGRATION_COMPLETE` | camera-python-display works |
