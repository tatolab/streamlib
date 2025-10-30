# streamlib-macros

Procedural macros for streamlib to reduce boilerplate when writing processors.

## Features

### `#[processor]` Attribute Macro

Automatically discovers input and output ports from struct fields and generates helper methods.

**Example:**

```rust
use streamlib::{processor, StreamInput, StreamOutput, VideoFrame, TimedTick, Result};

#[processor]
struct BlurEffect {
    input: StreamInput<VideoFrame>,
    output: StreamOutput<VideoFrame>,
    radius: f32,
}

impl BlurEffect {
    fn process(&mut self, tick: TimedTick) -> Result<()> {
        if let Some(frame) = self.input.read_latest() {
            let blurred = apply_blur(frame, self.radius)?;
            self.output.write(blurred);
        }
        Ok(())
    }
}

// The macro generates:
// - BlurEffect::input_port_names() -> &["input"]
// - BlurEffect::output_port_names() -> &["output"]
// - Auto-registration with global processor registry
```

## Generated Methods

For a processor with fields:
```rust
#[processor]
struct MyProcessor {
    video_in: StreamInput<VideoFrame>,
    audio_in: StreamInput<AudioFrame>,
    output: StreamOutput<VideoFrame>,
}
```

The macro generates:

```rust
impl MyProcessor {
    pub const fn input_port_names() -> &'static [&'static str] {
        &["video_in", "audio_in"]
    }

    pub const fn output_port_names() -> &'static [&'static str] {
        &["output"]
    }
}
```

## Usage Patterns

### Generator (No Inputs)

```rust
#[processor]
struct CameraProcessor {
    output: StreamOutput<VideoFrame>,
}
```

Generates:
- `input_port_names()` → `&[]`
- `output_port_names()` → `&["output"]`

### Sink (No Outputs)

```rust
#[processor]
struct DisplayProcessor {
    input: StreamInput<VideoFrame>,
}
```

Generates:
- `input_port_names()` → `&["input"]`
- `output_port_names()` → `&[]`

### Filter (Input + Output)

```rust
#[processor]
struct EffectProcessor {
    input: StreamInput<VideoFrame>,
    output: StreamOutput<VideoFrame>,
}
```

Generates:
- `input_port_names()` → `&["input"]`
- `output_port_names()` → `&["output"]`

## Benefits

1. **Less boilerplate** - No need to manually list port names
2. **Compile-time port discovery** - Ports are discovered from struct fields at compile time
3. **Type safety** - Only `StreamInput<T>` and `StreamOutput<T>` fields are recognized as ports
4. **Auto-registration** - Processors are automatically registered with the global registry

## Limitations

- Currently does not auto-implement `StreamProcessor` trait (use manual impl)
- Requires `StreamInput` and `StreamOutput` to be in scope
- Works best with simple struct field patterns

## Future Enhancements

- [ ] Auto-implement `StreamProcessor` trait
- [ ] Generate `ProcessorDescriptor` from doc comments
- [ ] Support for generic processors
- [ ] Custom port discovery attributes

## Testing

Due to circular dependency constraints (streamlib depends on streamlib-macros), integration tests should be run from the streamlib crate:

```bash
cd libs/streamlib
cargo test
```

## License

MIT
