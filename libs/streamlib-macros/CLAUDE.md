# streamlib-macros - Procedural Macros for Processor Creation

**ALWAYS use the `#[derive(StreamProcessor)]` macro when creating new processors.**

This macro reduces boilerplate from ~90 lines to ~10 lines and ensures MCP compatibility.

## The Vision

**Every processor should be AI-agent discoverable with minimal effort.**

The macro system auto-generates:
- Config structs
- `from_config()` constructor
- `descriptor()` with type-safe schemas
- Descriptions, tags, and examples
- Audio requirements detection
- MCP-compatible metadata

**Without the macro:** 90+ lines of repetitive boilerplate, easy to mess up
**With the macro:** 10-15 lines, compiler-verified correctness

## Usage Patterns

### Level 0: Minimal (Everything Auto-Generated)

```rust
use streamlib::{StreamInput, StreamOutput, VideoFrame};

#[derive(StreamProcessor)]
struct SimpleEffectProcessor {
    #[input()]
    video_in: StreamInput<VideoFrame>,

    #[output()]
    video_out: StreamOutput<VideoFrame>,
}

impl SimpleEffectProcessor {
    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.video_in.read_latest() {
            // Process frame...
            self.video_out.write(frame);
        }
        Ok(())
    }
}
```

**What gets auto-generated:**
- Config: `pub type Config = EmptyConfig;`
- Description: "Simple effect processor"
- Usage context: "Inputs: video_in (VideoFrame). Outputs: video_out (VideoFrame)."
- Tags: `["video", "effect"]`
- Examples: Extracted from `VideoFrame::examples()`
- Full MCP-compatible `ProcessorDescriptor`

### Level 1: With Config Fields

```rust
#[derive(StreamProcessor)]
#[processor(
    description = "Applies blur effect to video streams",
    usage = "Connect video input, adjust blur_radius parameter, connect output"
)]
struct BlurProcessor {
    #[input(description = "Video to blur")]
    video: StreamInput<VideoFrame>,

    #[output(description = "Blurred video")]
    output: StreamOutput<VideoFrame>,

    // Config fields (not ports!)
    blur_radius: f32,
    blur_sigma: f32,
}
```

**What gets auto-generated:**
```rust
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub blur_radius: f32,
    pub blur_sigma: f32,
}

impl StreamProcessorFactory for BlurProcessor {
    type Config = Config;

    fn from_config(config: Config) -> Result<Self> {
        Ok(Self {
            video: StreamInput::new("video"),
            output: StreamOutput::new("output"),
            blur_radius: config.blur_radius,
            blur_sigma: config.blur_sigma,
        })
    }
}
```

### Level 2: Full Control (Custom Config)

```rust
#[derive(Debug, Clone)]
struct AdvancedConfig {
    mode: ProcessingMode,
    intensity: f32,
    gpu_optimized: bool,
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            mode: ProcessingMode::Standard,
            intensity: 1.0,
            gpu_optimized: true,
        }
    }
}

#[derive(StreamProcessor)]
#[processor(
    config = AdvancedConfig,
    description = "Advanced multi-modal processor",
    usage = "High-performance video and audio processing with GPU acceleration"
)]
struct AdvancedProcessor {
    #[input(name = "video_input", description = "HD video stream", required = true)]
    video: StreamInput<VideoFrame>,

    #[input(name = "audio_input", description = "Stereo audio", required = false)]
    audio: StreamInput<AudioFrame>,

    #[output(name = "processed_video")]
    video_out: StreamOutput<VideoFrame>,

    #[output(name = "processed_audio")]
    audio_out: StreamOutput<AudioFrame>,
}
```

## Attribute Reference

### `#[processor(...)]` - Processor-Level

- **`config = MyConfig`** - Use custom config type instead of auto-generating
- **`description = "..."`** - Override auto-generated description
- **`usage = "..."`** - Override auto-generated usage context
- **`tags = ["tag1", "tag2"]`** - Override auto-generated tags (NOT YET IMPLEMENTED)
- **`audio_requirements = {...}`** - Custom audio requirements (NOT YET IMPLEMENTED)

### `#[input(...)]` or `#[output(...)]` - Port-Level

- **`name = "custom_name"`** - Override field name as port name
- **`description = "..."`** - Override auto-generated port description
- **`required = true`** - Mark input as required (inputs only)

## Type Safety Guarantees

**The macro extracts schemas from type parameters at compile time:**

```rust
#[input()]
video: StreamInput<VideoFrame>
```

Becomes:

```rust
.with_input("video", VideoFrame::schema(), "Video")
```

**No string-based type references anywhere!**

If `VideoFrame` doesn't implement `PortMessage::schema()`, you get a compile error immediately.
IDE autocomplete works perfectly because everything uses real Rust types.

## Smart Defaults Algorithm

### Description Generation

1. Extract struct name (e.g., "CameraProcessor")
2. Remove "Processor" suffix → "Camera"
3. Split on uppercase → "camera"
4. Analyze ports:
   - No inputs, has outputs → "camera source processor"
   - Has inputs, no outputs → "camera sink processor"
   - One input, one output → "camera effect processor"
   - Otherwise → "camera processor"

### Usage Context Generation

Enumerate all ports with their types:
```
"Inputs: video_in (VideoFrame), audio_in (AudioFrame). Outputs: mixed (AudioFrame)."
```

### Tags Generation

Auto-detect from port types and processor category:
- Has `VideoFrame` ports → add "video" tag
- Has `AudioFrame` ports → add "audio" tag
- Has `DataMessage` ports → add "data" tag
- No inputs → add "source" tag
- No outputs → add "sink" tag
- Both inputs and outputs → add "effect" tag

### Examples Generation

Extracted from `PortMessage::examples()`:
```rust
impl PortMessage for VideoFrame {
    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        vec![
            ("720p video", Self::example_720p()),
            ("1080p video", Self::example_1080p()),
            ("4K video", Self::example_4k()),
        ]
    }
}
```

Macro automatically adds all examples to `ProcessorDescriptor`.

### Audio Requirements Detection

If processor has any `AudioFrame` ports:
```rust
.with_audio_requirements(AudioRequirements::default())
```

Can be overridden with custom requirements via attribute.

## Generated Code Structure

For a processor like:

```rust
#[derive(StreamProcessor)]
struct MyProcessor {
    #[input()]
    input: StreamInput<VideoFrame>,

    #[output()]
    output: StreamOutput<VideoFrame>,

    threshold: f32,
}
```

The macro generates approximately:

```rust
// Config struct
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub threshold: f32,
}

// Constructor trait
impl StreamProcessorFactory for MyProcessor {
    type Config = Config;

    fn from_config(config: Config) -> Result<Self> {
        Ok(Self {
            input: StreamInput::new("input"),
            output: StreamOutput::new("output"),
            threshold: config.threshold,
        })
    }
}

// Descriptor trait
impl DescriptorProvider for MyProcessor {
    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new("MyProcessor", "My effect processor")
                .with_usage_context("Inputs: input (VideoFrame). Outputs: output (VideoFrame).")
                .with_tags(vec!["video", "effect"])
                .with_input("input", VideoFrame::schema(), "Input")
                .with_output("output", VideoFrame::schema(), "Output")
                .with_examples(VideoFrame::examples())
        )
    }
}

// Downcasting trait
impl DynStreamProcessor for MyProcessor {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
```

## Common Patterns

### Source Processor (Output Only)

```rust
#[derive(StreamProcessor)]
struct CameraProcessor {
    #[output()]
    video: StreamOutput<VideoFrame>,

    device_id: Option<String>,
    frame_rate: u32,
}
```

Auto-detected as "source" processor with "source" tag.

### Sink Processor (Input Only)

```rust
#[derive(StreamProcessor)]
struct FileWriterProcessor {
    #[input()]
    video: StreamInput<VideoFrame>,

    output_path: String,
}
```

Auto-detected as "sink" processor with "sink" tag.

### Effect Processor (Transform)

```rust
#[derive(StreamProcessor)]
struct ColorGradeProcessor {
    #[input()]
    input: StreamInput<VideoFrame>,

    #[output()]
    output: StreamOutput<VideoFrame>,

    temperature: f32,
    saturation: f32,
}
```

Auto-detected as "effect" processor with "effect" tag.

### Mixer Processor (Multiple Inputs)

```rust
#[derive(StreamProcessor)]
struct AudioMixerProcessor {
    #[input(description = "First audio track")]
    audio_1: StreamInput<AudioFrame>,

    #[input(description = "Second audio track")]
    audio_2: StreamInput<AudioFrame>,

    #[output(description = "Mixed audio output")]
    mixed: StreamOutput<AudioFrame>,

    mix_ratio: f32,
}
```

Auto-detected as audio processor, adds `AudioRequirements::default()`.

## Implementation Notes

### File Structure

```
libs/streamlib-macros/
├── Cargo.toml
├── src/
│   ├── lib.rs          # Macro entry point
│   ├── attributes.rs   # Attribute parsing (#[processor(...)])
│   ├── analysis.rs     # Field classification (ports vs config)
│   └── codegen.rs      # Code generation (quote! macros)
└── tests/
    └── macro_tests.rs  # Integration tests
```

### Key Functions

- **`ProcessorAttributes::parse()`** - Parses `#[processor(...)]` attributes
- **`PortAttributes::parse()`** - Parses `#[input(...)]` / `#[output(...)]` attributes
- **`AnalysisResult::analyze()`** - Classifies all struct fields as ports or config
- **`extract_message_type()`** - Extracts `T` from `StreamInput<T>`
- **`generate_processor_impl()`** - Generates all trait implementations
- **`generate_config_struct()`** - Creates Config type or uses existing
- **`generate_descriptor()`** - Creates MCP-compatible descriptor
- **Smart default functions** - `generate_description()`, `generate_tags()`, etc.

### Type Extraction

The macro uses `syn` to parse generic type parameters at compile time:

```rust
fn extract_message_type(ty: &Type) -> Result<Type> {
    // Extracts T from StreamInput<T> or StreamOutput<T>
    // Returns compile error if not in correct format
}
```

This ensures 100% type safety - no string-based type names anywhere.

## MCP Compatibility

Every processor created with the macro automatically includes:

1. **Name** - Struct name as identifier
2. **Description** - Auto-generated or custom
3. **Usage Context** - When and how to use this processor
4. **Tags** - Semantic categories for search
5. **Port Schemas** - Type-safe input/output specifications
6. **Examples** - Sample configurations
7. **Audio Requirements** - If applicable

This makes processors immediately discoverable by AI agents via MCP protocol.

## Future Enhancements

### Planned Features

- [ ] Support for `tags` arrays in `#[processor()]`
- [ ] Support for custom `audio_requirements` in `#[processor()]`
- [ ] Validation of port connections at compile time
- [ ] Auto-generation of port accessor methods
- [ ] Support for dynamic port counts (Vec<StreamInput<T>>)
- [ ] Integration with `register_processor_type!` macro
- [ ] Compile-time verification of descriptor completeness

### Potential Improvements

- Optional empty parentheses: `#[input]` instead of `#[input()]`
- More flexible tag syntax
- Custom schema overrides per port
- Conditional compilation for platform-specific ports

## When NOT to Use the Macro

**Use manual implementation only when:**
- Processor has complex initialization logic that can't be expressed as config fields
- Dynamic port configuration (number of ports determined at runtime)
- Custom trait implementations that conflict with generated code
- Prototype/experimental processors during development

**In 95% of cases, use the macro!**

## Manual Implementation for Complex Processors

**Even if you can't use the macro, ALWAYS provide a descriptor() for MCP compatibility.**

### Example: Complex Processor with Manual Descriptor

```rust
use streamlib::{StreamProcessor, ProcessorDescriptor, Result, AudioFrame};

/// Complex processor that can't use the macro (dynamic ports, background threads, etc.)
pub struct AudioMixerProcessor {
    // Dynamic ports created at runtime
    input_ports: HashMap<String, Arc<Mutex<StreamInput<AudioFrame>>>>,
    output_port: StreamOutput<AudioFrame>,

    // Complex state
    num_inputs: usize,
    mixing_strategy: MixingStrategy,
    resamplers: HashMap<String, SincFixedIn<f32>>,
    background_thread: Option<JoinHandle<()>>,
}

impl StreamProcessor for AudioMixerProcessor {
    type Config = AudioMixerConfig;

    fn from_config(config: Self::Config) -> Result<Self> {
        // Complex initialization here
        let mut input_ports = HashMap::new();
        for i in 0..config.num_inputs {
            let port_name = format!("input_{}", i);
            input_ports.insert(
                port_name.clone(),
                Arc::new(Mutex::new(StreamInput::new(port_name)))
            );
        }

        Ok(Self {
            input_ports,
            output_port: StreamOutput::new("audio"),
            num_inputs: config.num_inputs,
            mixing_strategy: config.strategy,
            resamplers: HashMap::new(),
            background_thread: None,
        })
    }

    // ⚠️ CRITICAL: Always provide descriptor for MCP compatibility
    fn descriptor() -> Option<ProcessorDescriptor> {
        use streamlib::{SCHEMA_AUDIO_FRAME, AudioRequirements};

        Some(
            ProcessorDescriptor::new(
                "AudioMixerProcessor",
                "Mixes multiple audio streams into a single output with sample rate conversion"
            )
            .with_usage_context(
                "Number of inputs configured at creation time via num_inputs parameter. \
                 Inputs are named 'input_0', 'input_1', etc. \
                 Supports real-time sample rate conversion and channel mixing. \
                 Uses lock-free buffers for audio thread safety."
            )
            // Document dynamic port pattern
            .with_input("input_*", SCHEMA_AUDIO_FRAME.clone(), "Audio input (dynamic count based on num_inputs)")
            .with_output("audio", SCHEMA_AUDIO_FRAME.clone(), "Mixed audio output (stereo)")
            .with_audio_requirements(AudioRequirements {
                preferred_buffer_size: Some(2048),
                required_buffer_size: None,
                supported_sample_rates: vec![44100, 48000, 96000],
                required_channels: None,
            })
            .with_tags(vec!["audio", "mixer", "multi-input", "real-time"])
        )
    }

    fn process(&mut self) -> Result<()> {
        // Complex processing logic
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
```

### Key Points for Manual Implementation

1. **Always implement `descriptor()`** - This enables MCP discovery
2. **Document dynamic ports** - Use patterns like "input_*" to indicate dynamic count
3. **Be specific in usage_context** - Explain special requirements (threading, sample rates, etc.)
4. **Include audio_requirements** - For audio processors, specify buffer sizes and sample rates
5. **Use meaningful tags** - Help AI agents categorize and find your processor

### Manual vs. Macro Comparison

```rust
// ❌ Complex processor - CANNOT use macro
#[derive(StreamProcessor)]  // Won't work - dynamic ports
struct AudioMixer {
    // This requires HashMap, not fixed fields
    inputs: HashMap<String, StreamInput<AudioFrame>>,
}

// ✅ Complex processor - Manual with descriptor
impl StreamProcessor for AudioMixer {
    fn descriptor() -> Option<ProcessorDescriptor> {
        // Manually document for MCP
        Some(ProcessorDescriptor::new(...))
    }
    // ... other methods
}

// ✅ Simple processor - Use macro
#[derive(StreamProcessor)]  // Perfect use case!
struct BlurEffect {
    #[input()]
    video: StreamInput<VideoFrame>,

    #[output()]
    output: StreamOutput<VideoFrame>,

    blur_radius: f32,
}
```

### Examples of Processors That Need Manual Implementation

- **AudioMixerProcessor** - Dynamic input count via HashMap
- **TestToneGenerator** - Background thread spawning, custom wakeup logic
- **Platform processors** (AppleCameraProcessor, etc.) - Hardware initialization
- **ClapEffectProcessor** - Plugin hosting with complex state management

All of these still implement `descriptor()` manually for MCP compatibility.

## Troubleshooting

### "expected attribute arguments in parentheses"

Use `#[input()]` not `#[input]`:
```rust
// ❌ Wrong
#[input]
video: StreamInput<VideoFrame>

// ✅ Correct
#[input()]
video: StreamInput<VideoFrame>
```

### "Port fields must be StreamInput<T> or StreamOutput<T>"

Ports must use the correct types:
```rust
// ❌ Wrong - raw type
#[input()]
video: VideoFrame

// ✅ Correct - wrapped in StreamInput
#[input()]
video: StreamInput<VideoFrame>
```

### "Processor must have at least one port"

Every processor needs at least one input OR output:
```rust
// ❌ Wrong - no ports
#[derive(StreamProcessor)]
struct MyProcessor {
    config_field: f32,
}

// ✅ Correct - has at least one port
#[derive(StreamProcessor)]
struct MyProcessor {
    #[output()]
    output: StreamOutput<VideoFrame>,
    config_field: f32,
}
```

### Custom config type not found

If using `config = MyConfig`, ensure the type is in scope:
```rust
// Define config before the processor
#[derive(Debug, Clone, Default)]
struct MyConfig {
    value: f32,
}

#[derive(StreamProcessor)]
#[processor(config = MyConfig)]
struct MyProcessor {
    // ...
}
```

## Related Documentation

- `/CLAUDE.md` - Repository-wide architecture
- `libs/streamlib/CLAUDE.md` - Main library usage
- `libs/streamlib/src/core/CLAUDE.md` - Core traits and types
- `examples/*/README.md` - Example processors using the macro
