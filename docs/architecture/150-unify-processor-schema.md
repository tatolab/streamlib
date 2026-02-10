# #150 — Unify Processor Schema into streamlib.yaml

**Status:** Architecture approved — ready for implementation planning.

---

## Problem

Every Rust processor — built-in, example, and plugin — defines its schema in a standalone YAML file read by the `#[streamlib::processor()]` macro at compile time. Plugin processors additionally duplicate that same content in `streamlib.yaml` for runtime loading. This creates two problems:

1. **Duplication for plugins:** `schemas/processors/grayscale.yaml` (compile-time) and `streamlib.yaml` (runtime) contain nearly identical content
2. **Scattered definitions for built-ins:** 16 individual YAML files spread across `src/apple/processors/` and `src/core/processors/`

Python and TypeScript processors define everything in `streamlib.yaml`. Rust processors should be the same.

---

## Key Discovery

Both paths already deserialize into the **exact same type**: `streamlib_codegen_shared::ProcessorSchema`.

- The macro calls `parse_processor_yaml()` which returns `ProcessorSchema`
- `ProjectConfig.processors` is `Vec<ProcessorSchema>`

No type conversion is needed. The macro just needs to read from `streamlib.yaml` instead of standalone YAML files.

---

## Design Decision

**Drop file path support entirely.** The macro argument is always a processor name:

```rust
// All processors — built-in, example, and plugin:
#[streamlib::processor("com.tatolab.camera")]
pub struct AppleCameraProcessor { ... }

#[streamlib::processor("com.tatolab.grayscale_rust")]
pub struct GrayscaleProcessor { ... }
```

The macro reads `CARGO_MANIFEST_DIR/streamlib.yaml`, finds the processor entry by name, and returns its `ProcessorSchema`. Every crate that defines processors must have a `streamlib.yaml` next to its `Cargo.toml`.

---

## What Changes

### Macro: `libs/streamlib-macros/src/lib.rs`

The `load_processor_schema()` function (lines 116-160) currently reads a standalone YAML file. Replace it entirely:

```
load_processor_schema(processor_name, item)
  → read CARGO_MANIFEST_DIR/streamlib.yaml
  → parse as ProjectConfigMinimal { processors: Vec<ProcessorSchema> }
  → find entry where name == processor_name
  → return that ProcessorSchema
```

The old file-path code path is removed — no heuristic, no branching.

### Shared crate: `libs/streamlib-codegen-shared/src/lib.rs`

Add `ProjectConfigMinimal` struct:

```rust
#[derive(Deserialize)]
pub struct ProjectConfigMinimal {
    #[serde(default)]
    pub processors: Vec<ProcessorSchema>,
}
```

This is a subset of `ProjectConfig` — only the `processors` field. Located in the shared crate since it's used by the macro and may be useful to other tooling.

### New streamlib.yaml files

**`libs/streamlib/streamlib.yaml`** — 19 processors (16 built-in + 3 test mocks):

| Processor | Source |
|-----------|--------|
| `com.tatolab.camera` | `src/apple/processors/camera.yaml` |
| `com.tatolab.display` | `src/apple/processors/display.yaml` |
| `com.tatolab.audio_capture` | `src/apple/processors/audio_capture.yaml` |
| `com.tatolab.audio_output` | `src/apple/processors/audio_output.yaml` |
| `com.tatolab.mp4_writer` | `src/apple/processors/mp4_writer.yaml` |
| `com.tatolab.screen_capture` | `src/apple/processors/screen_capture.yaml` |
| `com.streamlib.api_server` | `src/core/processors/api_server.yaml` |
| `com.tatolab.simple_passthrough` | `src/core/processors/simple_passthrough.yaml` |
| `com.tatolab.audio_channel_converter` | `src/core/processors/audio_channel_converter.yaml` |
| `com.tatolab.audio_mixer` | `src/core/processors/audio_mixer.yaml` |
| `com.tatolab.audio_resampler` | `src/core/processors/audio_resampler.yaml` |
| `com.tatolab.buffer_rechunker` | `src/core/processors/buffer_rechunker.yaml` |
| `com.tatolab.chord_generator` | `src/core/processors/chord_generator.yaml` |
| `com.streamlib.clap.effect` | `src/core/processors/clap_effect.yaml` |
| `com.streamlib.webrtc_whep` | `src/core/processors/webrtc_whep.yaml` |
| `com.streamlib.webrtc_whip` | `src/core/processors/webrtc_whip.yaml` |
| `com.streamlib.test.mock_processor` | `schemas/processors/test/mock_processor.yaml` |
| `com.streamlib.test.mock_input_only_processor` | `schemas/processors/test/mock_input_only_processor.yaml` |
| `com.streamlib.test.mock_output_only_processor` | `schemas/processors/test/mock_output_only_processor.yaml` |

**`examples/camera-python-display/streamlib.yaml`** — 2 Rust processors:

| Processor | Source |
|-----------|--------|
| `com.tatolab.crt_film_grain` | `src/crt_film_grain.yaml` |
| `com.tatolab.blending_compositor` | `src/blending_compositor.yaml` |

**`examples/camera-rust-plugin/plugin/streamlib.yaml`** — already exists, no change needed.

### Macro invocation changes

Every `#[crate::processor("path/to/file.yaml")]` and `#[streamlib::processor("path/to/file.yaml")]` becomes `#[...::processor("com.tatolab.X")]`:

| File | Old | New |
|------|-----|-----|
| `libs/streamlib/src/apple/processors/camera.rs` | `#[crate::processor("src/apple/processors/camera.yaml")]` | `#[crate::processor("com.tatolab.camera")]` |
| `libs/streamlib/src/apple/processors/display.rs` | `#[crate::processor("src/apple/processors/display.yaml")]` | `#[crate::processor("com.tatolab.display")]` |
| `libs/streamlib/src/apple/processors/audio_capture.rs` | `#[crate::processor("src/apple/processors/audio_capture.yaml")]` | `#[crate::processor("com.tatolab.audio_capture")]` |
| `libs/streamlib/src/apple/processors/audio_output.rs` | `#[crate::processor("src/apple/processors/audio_output.yaml")]` | `#[crate::processor("com.tatolab.audio_output")]` |
| `libs/streamlib/src/apple/processors/mp4_writer.rs` | `#[crate::processor("src/apple/processors/mp4_writer.yaml")]` | `#[crate::processor("com.tatolab.mp4_writer")]` |
| `libs/streamlib/src/apple/processors/screen_capture.rs` | `#[crate::processor("src/apple/processors/screen_capture.yaml")]` | `#[crate::processor("com.tatolab.screen_capture")]` |
| `libs/streamlib/src/core/processors/api_server.rs` | `#[crate::processor("src/core/processors/api_server.yaml")]` | `#[crate::processor("com.streamlib.api_server")]` |
| `libs/streamlib/src/core/processors/simple_passthrough.rs` | `#[crate::processor("src/core/processors/simple_passthrough.yaml")]` | `#[crate::processor("com.tatolab.simple_passthrough")]` |
| `libs/streamlib/src/core/processors/audio_channel_converter.rs` | `#[crate::processor("src/core/processors/audio_channel_converter.yaml")]` | `#[crate::processor("com.tatolab.audio_channel_converter")]` |
| `libs/streamlib/src/core/processors/audio_mixer.rs` | `#[crate::processor("src/core/processors/audio_mixer.yaml")]` | `#[crate::processor("com.tatolab.audio_mixer")]` |
| `libs/streamlib/src/core/processors/audio_resampler.rs` | `#[crate::processor("src/core/processors/audio_resampler.yaml")]` | `#[crate::processor("com.tatolab.audio_resampler")]` |
| `libs/streamlib/src/core/processors/buffer_rechunker.rs` | `#[crate::processor("src/core/processors/buffer_rechunker.yaml")]` | `#[crate::processor("com.tatolab.buffer_rechunker")]` |
| `libs/streamlib/src/core/processors/chord_generator.rs` | `#[crate::processor("src/core/processors/chord_generator.yaml")]` | `#[crate::processor("com.tatolab.chord_generator")]` |
| `libs/streamlib/src/core/processors/clap_effect.rs` | `#[crate::processor("src/core/processors/clap_effect.yaml")]` | `#[crate::processor("com.streamlib.clap.effect")]` |
| `libs/streamlib/src/core/processors/webrtc_whip.rs` | `#[crate::processor("src/core/processors/webrtc_whip.yaml")]` | `#[crate::processor("com.streamlib.webrtc_whip")]` |
| `libs/streamlib/src/core/processors/webrtc_whep.rs` | `#[crate::processor("src/core/processors/webrtc_whep.yaml")]` | `#[crate::processor("com.streamlib.webrtc_whep")]` |
| `libs/streamlib/src/core/graph/graph_tests.rs` | `#[crate::processor("schemas/processors/test/mock_processor.yaml")]` | `#[crate::processor("com.streamlib.test.mock_processor")]` |
| `libs/streamlib/src/core/graph/graph_tests.rs` | `#[crate::processor("schemas/processors/test/mock_output_only_processor.yaml")]` | `#[crate::processor("com.streamlib.test.mock_output_only_processor")]` |
| `libs/streamlib/src/core/graph/graph_tests.rs` | `#[crate::processor("schemas/processors/test/mock_input_only_processor.yaml")]` | `#[crate::processor("com.streamlib.test.mock_input_only_processor")]` |
| `examples/camera-python-display/src/crt_film_grain.rs` | `#[streamlib::processor("src/crt_film_grain.yaml")]` | `#[streamlib::processor("com.tatolab.crt_film_grain")]` |
| `examples/camera-python-display/src/blending_compositor.rs` | `#[streamlib::processor("src/blending_compositor.yaml")]` | `#[streamlib::processor("com.tatolab.blending_compositor")]` |
| `examples/camera-rust-plugin/plugin/src/lib.rs` | `#[streamlib::processor("schemas/processors/grayscale.yaml")]` | `#[streamlib::processor("com.tatolab.grayscale_rust")]` |

### Files to delete

| Path | Reason |
|------|--------|
| `libs/streamlib/src/apple/processors/camera.yaml` | Consolidated into `libs/streamlib/streamlib.yaml` |
| `libs/streamlib/src/apple/processors/display.yaml` | Consolidated |
| `libs/streamlib/src/apple/processors/audio_capture.yaml` | Consolidated |
| `libs/streamlib/src/apple/processors/audio_output.yaml` | Consolidated |
| `libs/streamlib/src/apple/processors/mp4_writer.yaml` | Consolidated |
| `libs/streamlib/src/apple/processors/screen_capture.yaml` | Consolidated |
| `libs/streamlib/src/core/processors/api_server.yaml` | Consolidated |
| `libs/streamlib/src/core/processors/simple_passthrough.yaml` | Consolidated |
| `libs/streamlib/src/core/processors/audio_channel_converter.yaml` | Consolidated |
| `libs/streamlib/src/core/processors/audio_mixer.yaml` | Consolidated |
| `libs/streamlib/src/core/processors/audio_resampler.yaml` | Consolidated |
| `libs/streamlib/src/core/processors/buffer_rechunker.yaml` | Consolidated |
| `libs/streamlib/src/core/processors/chord_generator.yaml` | Consolidated |
| `libs/streamlib/src/core/processors/clap_effect.yaml` | Consolidated |
| `libs/streamlib/src/core/processors/webrtc_whep.yaml` | Consolidated |
| `libs/streamlib/src/core/processors/webrtc_whip.yaml` | Consolidated |
| `libs/streamlib/schemas/processors/test/mock_processor.yaml` | Consolidated |
| `libs/streamlib/schemas/processors/test/mock_input_only_processor.yaml` | Consolidated |
| `libs/streamlib/schemas/processors/test/mock_output_only_processor.yaml` | Consolidated |
| `examples/camera-python-display/src/crt_film_grain.yaml` | Consolidated into `examples/camera-python-display/streamlib.yaml` |
| `examples/camera-python-display/src/blending_compositor.yaml` | Consolidated |
| `examples/camera-rust-plugin/plugin/schemas/` | Entire directory — processor already in `plugin/streamlib.yaml` |

### What doesn't change

- `codegen.rs` — receives `ProcessorSchema` either way, generates identical code
- `streamlib-codegen-shared` — `ProcessorSchema`, `parse_processor_yaml()` unchanged (only adds `ProjectConfigMinimal`)
- `load_project()` — continues reading `streamlib.yaml` at runtime
- Python/TypeScript — unaffected

### Already-broken test

`libs/streamlib/tests/attribute_macro_test.rs` references non-existent `schemas/processors/test/test_processor.yaml` and `schemas/processors/test/configured_processor.yaml`. This test was already broken before this change. The migration will update it to use name-based lookup with entries added to `libs/streamlib/streamlib.yaml`.

---

## Error Messages

When the processor name is not found in `streamlib.yaml`:

```
error: Processor 'com.tatolab.grayscale_rust' not found in streamlib.yaml
  Expected at: /path/to/plugin/streamlib.yaml
  Available processors:
    - com.tatolab.other_processor
```

When `streamlib.yaml` doesn't exist:

```
error: streamlib.yaml not found at /path/to/plugin/streamlib.yaml
  The #[streamlib::processor("name")] macro requires a streamlib.yaml
  next to Cargo.toml with processor definitions.
```

---

## Migration Order

1. Add `ProjectConfigMinimal` to `streamlib-codegen-shared`
2. Replace `load_processor_schema()` in the macro — name-based lookup only
3. Create `libs/streamlib/streamlib.yaml` with all 19 processor entries
4. Update all 19 `#[crate::processor(...)]` invocations in `libs/streamlib/`
5. Delete 19 standalone YAML files from `libs/streamlib/`
6. Create `examples/camera-python-display/streamlib.yaml` with 2 processor entries
7. Update 2 `#[streamlib::processor(...)]` invocations in `camera-python-display`
8. Delete 2 YAML files from `examples/camera-python-display/src/`
9. Update `camera-rust-plugin/plugin/src/lib.rs` macro invocation
10. Delete `examples/camera-rust-plugin/plugin/schemas/` directory
11. Fix `attribute_macro_test.rs` to use name-based lookup
12. Delete empty `libs/streamlib/schemas/` directory tree
13. Verify: `cargo check`, `cargo test`, `cargo clippy`

---

## Verification

1. `cargo check` — full workspace compiles
2. `cargo test -p streamlib` — library tests pass (including graph_tests with mock processors)
3. `cargo test -p streamlib-codegen-shared` — schema parser tests pass
4. `cargo build -p grayscale-plugin` — plugin compiles with new syntax
5. `cargo check -p camera-rust-plugin` — example still works
6. `cargo check -p camera-python-display` — example still works
7. `cargo clippy` — no warnings
