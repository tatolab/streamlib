# Connection APIs: Type Safety in streamlib

streamlib provides **two connection APIs** with different trade-offs:

## 1. Type-Safe Rust API (Compile-Time Checking)

**Use when**: Writing Rust code with known processor types at compile time.

```rust
use streamlib::{StreamRuntime, CameraProcessor, DisplayProcessor};

let mut runtime = StreamRuntime::new();

// Create processors
let mut camera = CameraProcessor::new(None)?;
let mut display = DisplayProcessor::new(None, 1920, 1080)?;

// Type-safe connection - compiler enforces VideoFrame → VideoFrame
runtime.connect(
    &mut camera.ports.output.video,  // StreamOutput<VideoFrame>
    &mut display.ports.input.video,  // StreamInput<VideoFrame>
)?;
```

**Type Safety Guarantees:**
- ✅ **Compile-time type checking** - Connecting mismatched types won't compile
- ✅ **Port existence** - Accessing non-existent ports won't compile
- ✅ **Zero runtime overhead** - All checks at compile time
- ✅ **IDE autocomplete** - Navigate processor ports easily

**Example of compile-time error:**
```rust
// ❌ Compiler error: Can't connect AudioFrame to VideoFrame
runtime.connect(
    &mut microphone.ports.output.audio,  // StreamOutput<AudioFrame>
    &mut display.ports.input.video,      // StreamInput<VideoFrame>
)?;
// ERROR: expected StreamOutput<VideoFrame>, found StreamOutput<AudioFrame>
```

## 2. String-Based Runtime API (Runtime Schema Checking)

**Use when**: Dynamically connecting processors where types aren't known at compile time.

**Primary use cases:**
- **MCP servers** - AI agents building pipelines
- **Configuration files** - Loading pipeline topology from JSON/YAML
- **Hot-swapping** - Replacing processors without recompilation
- **Plugin systems** - Connecting third-party processors

```rust
use streamlib::{StreamRuntime, StreamProcessor};

let mut runtime = StreamRuntime::new();

// Add processors (types determined at runtime)
let camera_id = runtime.add_processor_to_running(Box::new(camera)).await?;
let display_id = runtime.add_processor_to_running(Box::new(display)).await?;

// String-based connection - runtime schema validation
let connection_id = runtime.connect_at_runtime(
    &format!("{}.video", camera_id),
    &format!("{}.video", display_id)
).await?;
```

**Type Safety Guarantees:**
- ✅ **Runtime schema validation** - Checks schema compatibility before connecting
- ✅ **Port name validation** - Checks processor actually has the named ports
- ✅ **Descriptive errors** - Returns clear error messages on mismatch
- ❌ **No compile-time checking** - Errors only caught at runtime

### How Runtime Schema Validation Works

Each processor declares its port schemas via `ProcessorDescriptor`:

```rust
impl StreamProcessor for CameraProcessor {
    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new("CameraProcessor", "Captures video from camera")
                .with_output(PortDescriptor::new(
                    "video",
                    SCHEMA_VIDEO_FRAME,  // Schema reference
                    "RGB video frames from camera"
                ))
        )
    }
}
```

When `connect_at_runtime` is called:

1. **Parse port references**: `"processor_0.video"` → processor ID + port name
2. **Look up processors**: Check both processors exist in registry
3. **Get port schemas**: Extract schema from each processor's descriptor
4. **Validate compatibility**: Check `source.schema == destination.schema`
5. **Connect if valid**: Transfer data consumer from output to input

**Example of runtime error:**
```rust
// ❌ Runtime error: Schema mismatch
runtime.connect_at_runtime("mic_0.audio", "display_0.video").await?;
// ERROR: Cannot connect audio → video (AudioFrame schema ≠ VideoFrame schema)
```

## Schema Compatibility Rules

**Same schema = Compatible:**
```rust
// ✅ Both use SCHEMA_VIDEO_FRAME
"camera.video" → "display.video"

// ✅ Both use SCHEMA_AUDIO_FRAME
"microphone.audio" → "speaker.audio"
```

**Different schemas = Incompatible:**
```rust
// ❌ AudioFrame ≠ VideoFrame
"microphone.audio" → "display.video"

// ❌ Custom schema ≠ built-in schema
"custom_source.data" → "display.video"
```

**Schema evolution:**
```rust
// Future: Semantic versioning for backward compatibility
SCHEMA_VIDEO_FRAME_V1  →  SCHEMA_VIDEO_FRAME_V2  // Compatible
SCHEMA_VIDEO_FRAME_V1  →  SCHEMA_VIDEO_FRAME_V3  // Breaking change
```

## Protecting Against AI Agent Mistakes

**Problem**: AI agents might try arbitrary string connections without understanding types.

**Solution**: Schema validation acts as a **runtime type system** for dynamic connections.

### Example: MCP Agent Trying Invalid Connection

```bash
# AI agent attempts connection via MCP
mcp-client call connect_processors '{
  "source": "microphone_0.audio",
  "destination": "display_0.video"
}'
```

**Response:**
```json
{
  "success": false,
  "message": "Failed to connect microphone_0.audio → display_0.video: \
              Schema mismatch (AudioFrame != VideoFrame). \
              Source port 'audio' has schema 'AudioFrame' but \
              destination port 'video' expects schema 'VideoFrame'.",
  "data": null
}
```

**The connection fails safely without crashing the runtime.**

### Best Practices for AI Agents

1. **Query processor schemas first**:
   ```rust
   // AI agent calls: list_processor_instances
   // Gets back: [{ id: "camera_0", type: "CameraProcessor", outputs: [{ name: "video", schema: "VideoFrame" }] }]
   ```

2. **Validate compatibility before connecting**:
   ```rust
   // Check: source.output.schema == destination.input.schema
   if camera.output.video.schema == display.input.video.schema {
       connect_at_runtime("camera_0.video", "display_0.video")
   }
   ```

3. **Handle errors gracefully**:
   ```rust
   // Don't assume connection succeeds - check result
   match runtime.connect_at_runtime(source, dest).await {
       Ok(conn_id) => { /* connection established */ },
       Err(e) => { /* try alternate connection or report error */ }
   }
   ```

## API Comparison

| Feature | Type-Safe `connect<T>()` | String-Based `connect_at_runtime()` |
|---------|-------------------------|--------------------------------------|
| **Type checking** | Compile-time | Runtime (schema validation) |
| **Port validation** | Compile-time | Runtime (descriptor lookup) |
| **IDE support** | Full autocomplete | String literals (no autocomplete) |
| **Error detection** | Before compilation | During connection attempt |
| **Error messages** | Compiler errors | Runtime errors with details |
| **Performance** | Zero overhead | Schema lookup + validation |
| **Flexibility** | Fixed at compile time | Dynamic at runtime |
| **Use case** | Application code | MCP/config/plugins |

## When To Use Which API

### Use Type-Safe `connect<T>()` when:

- ✅ Writing Rust application code
- ✅ Building fixed pipelines (camera → display)
- ✅ You know processor types at compile time
- ✅ You want IDE autocomplete and refactoring support
- ✅ Performance is critical (zero overhead)

### Use String-Based `connect_at_runtime()` when:

- ✅ Building MCP servers for AI agents
- ✅ Loading pipeline topology from config files
- ✅ Hot-swapping processors at runtime
- ✅ Connecting third-party plugin processors
- ✅ Building visual pipeline editors

## Migration Path

If you're migrating from string-based to type-safe connections:

```rust
// BEFORE: String-based (dynamic)
runtime.connect_at_runtime("proc_0.video", "proc_1.video").await?;

// AFTER: Type-safe (static)
runtime.connect(
    &mut camera.ports.output.video,
    &mut display.ports.input.video
)?;
```

**Benefits of migration:**
- Errors caught at compile time
- IDE autocomplete works
- Refactoring is safe (rename detection)
- Zero runtime overhead

**When NOT to migrate:**
- Pipeline topology loaded from config files
- AI agents building pipelines dynamically
- Plugin systems with unknown types

## Summary

Both APIs provide **type safety**, just at different times:

- **Type-safe API**: Checks types at **compile time** (best for application code)
- **String-based API**: Checks types at **runtime** via schemas (best for dynamic systems)

The string-based API is NOT "unsafe" - it validates schemas before connecting. This protects against AI agent mistakes while still allowing dynamic pipeline construction.

**Recommendation for AI Agents:**
- Always query processor schemas before connecting
- Validate schema compatibility client-side when possible
- Handle connection errors gracefully (don't assume success)
- Use MCP's `list_processor_instances` to get available ports and schemas
