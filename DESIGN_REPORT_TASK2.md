# Design Report: Link-Level Remote Transport Architecture (Task #2)

**Author**: Design Agent
**Date**: 2026-03-29
**Status**: Ready for Implementation
**Reference**: Issue #217, Task #2

---

## Executive Summary

The correct architecture for MoQ as a link-level transport is:

1. **Rename `moq_fanout` → `remote`** on PortDescriptor (clearer intent)
2. **Apply `remote` flag symmetrically** to both input and output ports
3. **Keep encoding/decoding in the graph** via explicit processor chains (not in link layer)
4. **Remove specialized MoQ processors** (moq_publish, moq_subscribe, moq_decode_subscribe)
5. **Add remote input support to InputMailboxes** (currently only outputs support it)
6. **Update compiler wiring** to handle remote inputs alongside local iceoryx2

---

## Design Question Answers

### Q1: Should `remote` replace `moq_fanout` on PortDescriptor, or on the Link?

**Answer**: On **PortDescriptor**.

**Rationale**:
- A port is either capable of remote transport or it isn't — this is a port property, not a link property
- The link already has `moq_transport_config` for relay URL/namespace/track name — that's the right place for connection details
- Declaration is cleaner: port declares "I can be remote", compiler sees this and wires appropriately
- Symmetry: both input and output ports get the same flag

**Change**:
```rust
// descriptors.rs
pub struct PortDescriptor {
    pub name: String,
    pub description: String,
    pub schema: String,
    pub required: bool,
    pub is_iceoryx2: bool,
    /// Whether this port uses network-transparent remote transport (MoQ).
    #[serde(default)]
    pub remote: bool,  // RENAMED from moq_fanout
}

// Add builder method:
pub fn with_remote(mut self) -> Self {
    self.remote = true;
    self
}
```

---

### Q2: How does an input port receive data from MoQ?

**Answer**: InputMailboxes needs to support a parallel remote input path.

**Current State**:
- InputMailboxes only has `SendableSubscriber` for iceoryx2
- `receive_pending()` receives from iceoryx2 and routes to mailboxes via `route(payload)`
- No mechanism for remote data ingestion

**Required Changes**:
1. Add a new `MoqSubscriber` component type (created during compiler wiring for remote input links)
2. Add `set_moq_subscription()` method to InputMailboxes (parallel to `set_subscriber()`)
3. Modify `receive_pending()` to drain both iceoryx2 **and** MoQ sources
4. Both sources feed the same `route(payload)` logic

**Pseudocode**:
```rust
pub struct InputMailboxes {
    ports: HashMap<String, PortConfig>,
    subscriber: SendableSubscriber,
    #[cfg(feature = "moq")]
    moq_subscriber: Option<Arc<Mutex<MoqSubscriber>>>, // NEW
}

pub fn receive_pending(&self) {
    // Existing iceoryx2 path
    if let Some(subscriber) = self.subscriber.get() {
        while let Ok(Some(sample)) = subscriber.receive() {
            self.route(*sample.payload());
        }
    }

    // NEW: MoQ path
    #[cfg(feature = "moq")]
    if let Some(moq_sub) = &self.moq_subscriber {
        while let Some(payload_bytes) = moq_sub.lock().receive() {
            // Deserialize FramePayload from bytes, then route
            if let Ok(payload) = deserialize_frame_payload(&payload_bytes) {
                self.route(payload);
            }
        }
    }
}
```

**Key Point**: The `route()` method already handles directing payloads to the correct mailbox based on `port_key`. Both iceoryx2 and MoQ feed the same routing logic — the transport is transparent to the processor.

---

### Q3: Encoding Pipeline — Where does encoding happen?

**Answer**: **User is responsible** via explicit processor chains in the graph (Option C).

**Rationale**:
- Keeps codec knowledge **out of the link/transport layer**
- Allows flexible encoder choice (software, GPU, per-stream config)
- Processor sees raw frames, produces encoded frames — that's the processor's job
- Link just moves bytes, agnostic to what the bytes represent

**Pipeline Example**:
```
Camera → [raw VideoFrame] → H.264 Encoder → [encoded H.264 bytes] → [remote output over MoQ]
```

**User's Responsibility**:
- Processor output port produces `EncodedVideoFrame` (schema: `com.tatolab.encodedvideoframe@1.0.0`)
- Link reads from this port and transports bytes over MoQ
- Link doesn't care what the bytes are

**What NOT to Do**:
- ❌ Don't add encoding logic to OutputWriter
- ❌ Don't add encoding logic to the link layer
- ❌ Don't create automatic encoder processors

**Schema Note**: Encoded frame schemas already exist:
- `com.tatolab.encodedvideoframe@1.0.0` (H.264/H.265 NAL units)
- `com.tatolab.encodedaudioframe@1.0.0` (Opus/AAC bitstream)

These are the correct output types for links marked `remote: true`.

---

### Q4: Decoding Pipeline — Where does decoding happen?

**Answer**: Same as encoding — **user includes decoders in the graph**.

**Pipeline Example**:
```
[remote input from MoQ] → [encoded H.264 bytes] → H.264 Decoder → [raw VideoFrame] → Display
```

**Parallel to Encoding**:
- Input port receives `EncodedVideoFrame` over MoQ
- Downstream processor is responsible for decoding
- Keeps link layer codec-agnostic

**What to Remove**:
- `moq_decode_subscribe` processor combines subscribe + decode in one — breaks this model
- Should be split: `moq_subscribe` (raw bytes) + explicit `H264Decoder` processor

---

### Q5: What changes to the compiler's wiring phase?

**Answer**: Update `open_iceoryx2_service_op.rs` to handle remote inputs + link MoQ sessions.

**Current Flow** (iceoryx2 only):
1. Compiler finds a Link in the graph
2. Calls `open_iceoryx2_service()` → creates publisher on source side + subscriber on dest side
3. Attaches OutputWriter to source processor
4. Attaches InputMailboxes to dest processor

**New Flow** (with remote support):

**For Output Ports marked `remote: true`**:
1. Link has `moq_transport_config` → create MoQ publisher session
2. Attach to OutputWriter via `add_moq_connection()` ← Already implemented!
3. No changes needed here — OutputWriter already fans out to MoQ

**For Input Ports marked `remote: true`** (NEW):
1. Link has `moq_transport_config` → create MoQ subscriber
2. Attach to InputMailboxes via new `set_moq_subscription()` method
3. `receive_pending()` drains both iceoryx2 and MoQ sources

**Pseudocode Change**:
```rust
pub fn open_iceoryx2_service(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
    runtime_ctx: &Arc<RuntimeContext>,
) -> Result<()> {
    let link = graph.traversal_mut().e(link_id).first()?;
    let (from_port, to_port) = (link.from_port().clone(), link.to_port().clone());

    // Check if destination port is marked remote
    let dest_port_descriptor = get_port_descriptor(&to_port)?;

    if dest_port_descriptor.remote && link.moq_transport_config.is_some() {
        // NEW: Wire MoQ subscription to InputMailboxes
        let moq_session = create_or_reuse_moq_session(link)?;
        let input_mailboxes = get_input_mailboxes(&dest_proc_id)?;
        input_mailboxes.set_moq_subscription(moq_session);
    }

    // Existing: iceoryx2 wiring continues as before...
}
```

**No Changes Needed To**:
- Link structure (already has `moq_transport_config`)
- OutputWriter (already supports MoQ)
- Schema handling (MessagePack bytes pass through unchanged)

---

### Q6: What changes to InputMailboxes for remote input?

**Answer**: Add parallel MoQ subscription path.

**Required Changes**:

1. **New field**:
   ```rust
   #[cfg(feature = "moq")]
   moq_subscriber: Option<Arc<Mutex<MoqSubscriber>>>,
   ```

2. **New method**:
   ```rust
   #[cfg(feature = "moq")]
   pub fn set_moq_subscription(&mut self, moq_sub: Arc<Mutex<MoqSubscriber>>) {
       self.moq_subscriber = Some(moq_sub);
   }
   ```

3. **Modified `receive_pending()`**:
   ```rust
   pub fn receive_pending(&self) {
       // Existing iceoryx2 path
       if let Some(subscriber) = self.subscriber.get() {
           while let Ok(Some(sample)) = subscriber.receive() {
               self.route(*sample.payload());
           }
       }

       // NEW: MoQ path
       #[cfg(feature = "moq")]
       if let Some(moq_sub) = &self.moq_subscriber {
           let moq_sub_guard = moq_sub.lock();
           // Drain MoQ frames, deserialize FramePayload, route to mailboxes
           while let Some(frame_bytes) = moq_sub_guard.receive() {
               if let Ok(payload) = rmp_serde::from_slice(&frame_bytes) {
                   self.route(payload);
               }
           }
       }
   }
   ```

**Key Design Point**: Both transports (iceoryx2 and MoQ) converge on the same `route()` method. The processor doesn't know or care which transport delivered the data — it just reads from a mailbox.

---

## Implementation Checklist

### 1. Rename & Update PortDescriptor
- [ ] Rename `moq_fanout: bool` → `remote: bool` in PortDescriptor
- [ ] Update `with_moq_fanout()` → `with_remote()`
- [ ] Update all processor definitions in streamlib.yaml that use `moq_fanout` → `remote`
- [ ] Update serde default handling

### 2. InputMailboxes Remote Support
- [ ] Add `moq_subscriber` field (gated by `#[cfg(feature = "moq")]`)
- [ ] Implement `set_moq_subscription()`
- [ ] Modify `receive_pending()` to drain both sources
- [ ] Test that both sources feed the same mailbox routing

### 3. Compiler Wiring
- [ ] Update `open_iceoryx2_service_op.rs` to detect remote input ports
- [ ] Create or reuse MoQ session for remote input links
- [ ] Call `set_moq_subscription()` on InputMailboxes for remote inputs
- [ ] Verify iceoryx2 wiring is unaffected

### 4. Remove Specialized MoQ Processors
- [ ] Deprecate or remove `moq_publish` processor
- [ ] Deprecate or remove `moq_subscribe` processor
- [ ] Deprecate or remove `moq_decode_subscribe` processor
- [ ] Remove from streamlib.yaml
- [ ] Remove from PROCESSOR_REGISTRY

**Note**: These processors duplicate work that should be in the graph. Users should instead:
- For publish: `raw frames → [encoder if needed] → [remote output port]`
- For subscribe: `[remote input port] → [decoder if needed] → raw frames`

### 5. Verify Schemas
- [ ] Confirm `com.tatolab.encodedvideoframe@1.0.0` exists ✓
- [ ] Confirm `com.tatolab.encodedaudioframe@1.0.0` exists ✓
- [ ] Document that remote outputs should produce encoded frames

### 6. Documentation
- [ ] Update PortDescriptor docs to explain `remote` flag
- [ ] Update Link docs to explain `moq_transport_config`
- [ ] Add example graphs: `camera → H264Encoder → [remote output]`
- [ ] Add example graphs: `[remote input] → H264Decoder → display`

---

## Key Design Principles

1. **Transport Agnostic**: Processors don't know about MoQ. OutputWriter/InputMailboxes abstract it away.
2. **Link Layer**: MoQ belongs at the same layer as iceoryx2 — as a transport option, not a processor.
3. **Codec Agnostic**: Link layer doesn't encode/decode. Codecs are user's responsibility in the graph.
4. **Symmetric**: Both inputs and outputs support remote transport via the `remote` flag.
5. **Zero-Copy Bytes**: MessagePack bytes pass through iceoryx2 and MoQ unchanged — no re-serialization.

---

## Testing Strategy

- [ ] Unit test: InputMailboxes receives from both iceoryx2 and MoQ, routes correctly
- [ ] Integration test: Graph with remote input/output, data flows end-to-end
- [ ] Regression test: Existing iceoryx2-only links still work
- [ ] Example: camera → H264Encoder → MoQ publish → [remote] → H264Decoder → display

---

## Risks & Mitigations

| Risk | Mitigation |
|------|-----------|
| User forgets to add encoder before remote output | Document in examples. Consider adding compile-time warnings if a raw frame schema flows to remote output. |
| MoQ subscribe path has performance issues | Use non-blocking receive in `receive_pending()`. Drain all pending frames in one call. |
| Backward compatibility with moq_fanout | Deprecation path: support both `moq_fanout` and `remote` for one release. |

---

## Next Steps for Implementor

1. **Start with InputMailboxes** (Q6) — add MoQ subscription path
2. **Update compiler wiring** (Q5) — detect remote input ports and attach MoQ subscriptions
3. **Rename PortDescriptor** (Q1) — moq_fanout → remote
4. **Remove MoQ processors** (Q3/Q4) — after confirming no internal tests depend on them
5. **Test end-to-end** — graphs with both remote inputs and outputs

---

## Files Affected

- `libs/streamlib/src/core/descriptors.rs` — rename `moq_fanout` → `remote`
- `libs/streamlib/src/iceoryx2/input.rs` — add MoQ subscription support
- `libs/streamlib/src/core/compiler/compiler_ops/open_iceoryx2_service_op.rs` — wire remote inputs
- `libs/streamlib/streamlib.yaml` — remove moq_publish/moq_subscribe/moq_decode_subscribe, update existing processor definitions
- `libs/streamlib/src/core/processors/moq_*.rs` — remove files

---

## Approval Status

✅ **Design Ready for Implementation**

This design aligns with the user's stated preference (Option C), maintains codec/transport separation, and leverages existing working code (OutputWriter, Link structure).

**Reviewer**: Awaiting code-reviewer findings before finalizing compiler wiring details.
