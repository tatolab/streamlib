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

