# streamlib Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                     AI AGENT VIDEO EFFECTS PROTOCOL                  │
│                                                                       │
│  Agent A                    Agent B                    Agent C       │
│     │                          │                          │          │
│     └──────────┬───────────────┴──────────────┬──────────┘          │
│                │      Exchange HLSL Shaders    │                     │
│                │      (Human-readable code)    │                     │
│                └───────────────┬───────────────┘                     │
└────────────────────────────────┼──────────────────────────────────────┘
                                 ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         STREAMLIB CORE (Rust)                        │
│                                                                       │
│  ┌──────────────────────────────────────────────────────────────┐   │
│  │                    Shader Compilation Pipeline                │   │
│  │                                                               │   │
│  │   HLSL Source  ──→  DXC  ──→  SPIR-V  ──→  SPIRV-Cross      │   │
│  │   (from agent)      Compiler   (binary)     Translator       │   │
│  │                                                ↓              │   │
│  │                                    ┌──────────┴────────────┐ │   │
│  │                                    │                       │ │   │
│  │                              Metal Shading    Vulkan SPIR-V│ │   │
│  │                              Language (macOS) (Linux/Jetson)│ │   │
│  └────────────────────────────────┬──────────────┬────────────┘   │
│                                   │              │                 │
│  ┌────────────────────────────────┴──────────────┴─────────────┐  │
│  │              Zero-Copy GPU Texture Sharing                   │  │
│  │                                                               │  │
│  │  IOSurface (macOS/iOS)      DMA-BUF (Linux)                  │  │
│  │  • Metal texture interop    • Vulkan texture interop         │  │
│  │  • CVPixelBuffer support    • V4L2 camera integration        │  │
│  │  • AVFoundation camera      • Direct memory access           │  │
│  │  • ARKit (iOS/macOS)        • Jetson hardware encoders       │  │
│  └───────────────────────────────────────────────────────────────┘  │
│                                                                       │
│  ┌───────────────────────────────────────────────────────────────┐  │
│  │                  Real-Time Processing Engine                  │  │
│  │                                                               │  │
│  │  • No CPU copies (GPU → GPU only)                            │  │
│  │  • Sub-millisecond latency                                   │  │
│  │  • 60 FPS @ 4K guaranteed                                    │  │
│  │  • No GIL, no GC pauses                                      │  │
│  └───────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────┘
                                 │
                    ┌────────────┴────────────┐
                    ▼                         ▼
         ┌──────────────────┐      ┌──────────────────┐
         │  Python Bindings │      │ TypeScript (NAPI)│
         │     (PyO3)       │      │   (Coming Soon)  │
         └──────────────────┘      └──────────────────┘

──────────────────────────────────────────────────────────────────────

TARGET PLATFORMS:
  • macOS (Metal)              - Development & Production
  • iOS (Metal)                - Wearables, AR headsets, helmets
  • Linux + NVIDIA Jetson      - Edge deployment (robotics, armor)
  • Embedded Linux Controllers - Real-time systems

KEY FEATURES:
  ✓ Agents exchange video effects as HLSL source code
  ✓ Self-healing: receiving agent can fix compilation errors
  ✓ Zero-copy GPU pipelines (IOSurface/DMA-BUF)
  ✓ Real-time guarantees (Rust, no GC)
  ✓ Cross-platform shader compilation (HLSL → SPIR-V → native)
  ✓ Built for robotics, AR/VR, and edge AI systems

──────────────────────────────────────────────────────────────────────

"Why doesn't this exist yet?"

Because everyone else is building for the web.
We're building for robots, armor, and self-replicating machines.

The infrastructure for AI agents to generate real-time video
doesn't exist. So we're building it.

──────────────────────────────────────────────────────────────────────
```

## Core Design Principles

### 1. Runtime as Primitive Provider (Not Effect Library)

The Rust core provides **low-level primitives**, not hardcoded effects:

```rust
pub trait StreamRuntime {
    // GPU primitives
    fn create_texture(&mut self, width: u32, height: u32) -> Result<GpuTexture>;
    fn create_compute_shader(&mut self, hlsl: &str) -> Result<ShaderId>;
    fn run_shader(&mut self, shader: ShaderId, inputs: &[GpuTexture]) -> Result<GpuTexture>;

    // Platform capabilities
    fn get_camera_texture(&mut self, device: &str) -> Result<GpuTexture>;
    fn get_arkit_texture(&mut self) -> Result<GpuTexture>;  // iOS/macOS
    fn display_texture(&mut self, texture: GpuTexture) -> Result<()>;

    // Stream graph
    fn add_stream(&mut self, handler: Box<dyn StreamHandler>);
    fn connect(&mut self, output: OutputPort, input: InputPort);
    fn run(&mut self) -> Result<()>;
}
```

**Agents compose primitives in arbitrary ways:**

```python
# Agent writes Python, composes primitives however they want
@stream_effect
def custom_effect(input_texture):
    temp = runtime.create_texture(1920, 1080)
    shader = runtime.create_compute_shader("""
        float4 main(float2 uv : TEXCOORD) : SV_Target {
            // Agent-written HLSL
            return tex2D(input_texture, uv) * float4(1, 0, 0, 1);
        }
    """)
    return runtime.run_shader(shader, [input_texture, temp])

# Current decorator API stays exactly the same
runtime.add_stream(Stream(camera))
runtime.add_stream(Stream(display))
runtime.connect(camera.outputs['video'], display.inputs['video'])
```

**Key insight:** Python is the **composition layer**. Rust provides **primitives**. Infinitely extensible.

### 2. Native Performance with Embedded Python

**Python is only used for control flow and setup:**

```python
# Setup phase (Python calls Rust)
runtime = StreamRuntime()
runtime.add_stream(Stream(camera))  # ← Python → Rust
runtime.connect(...)                 # ← Python → Rust

# Runtime phase (100% Rust, zero Python overhead)
await runtime.run()  # ← Rust takes over completely
# - Camera: Native AVFoundation/V4L2 (Rust)
# - GPU: Metal/Vulkan (Rust)
# - Zero-copy: IOSurface/DMA-BUF (Rust)
# - Display: Native (Rust)
# - Python never touches frames
```

**Performance guarantee:** GPU pipeline is 100% Rust. Python only sets it up.

### 3. Project Structure: Core vs Platform-Specific

```
packages/streamlib-core/
├── streamlib-core/          # Platform-agnostic
│   ├── src/
│   │   ├── graph.rs         # Stream graph, connections
│   │   ├── buffer.rs        # Ring buffers, zero-copy pools
│   │   ├── texture.rs       # Abstract GPU texture handle
│   │   ├── shader.rs        # HLSL → SPIR-V compiler
│   │   └── runtime.rs       # Core StreamRuntime trait
│
├── streamlib-apple/         # iOS + macOS (shared)
│   ├── src/
│   │   ├── metal/           # Metal GPU (both platforms)
│   │   ├── iosurface.rs     # IOSurface (both platforms)
│   │   ├── camera.rs        # AVFoundation (both platforms)
│   │   └── arkit/
│   │       ├── mod.rs       # Shared ARKit traits
│   │       ├── ios.rs       # iOS-specific (body/face tracking)
│   │       └── macos.rs     # macOS-specific (limited features)
│
├── streamlib-vulkan/        # Linux/Jetson
│   ├── src/
│   │   ├── capture.rs       # V4L2 camera
│   │   └── dmabuf.rs        # DMA-BUF zero-copy
│
├── streamlib-py/            # Python bindings (PyO3)
│   ├── src/
│   │   ├── lib.rs           # PyO3 wrapper
│   │   └── decorator.rs     # @stream_effect decorator
│
└── streamlib-runtime/       # Standalone binary
    └── src/
        └── main.rs          # Daemon mode, embedded Python
```

**Platform differences via feature flags:**

```rust
#[cfg(target_os = "ios")]
fn get_body_tracking(&mut self) -> Result<BodyPose>;  // iOS only

#[cfg(any(target_os = "ios", target_os = "macos"))]
fn get_arkit_texture(&mut self) -> Result<GpuTexture>;  // Both Apple platforms
```

### 4. Deployment Modes

#### Library Mode (Development)
```bash
uv add streamlib
```
```python
from streamlib import Runtime
runtime = Runtime()
# Iterate locally
```

**Delivered as:** Python wheel with embedded Rust (`streamlib-0.1.0-*.whl`)

#### Binary Mode (Production)
```bash
# Install daemon
curl -sSL https://install.streamlib.dev | sh

# Run on device (headless)
streamlib-runtime --listen 0.0.0.0:8080
```

**Delivered as:**
- Single binary (`streamlib-runtime`)
- Cross-compiled for ARM64 (iOS, Jetson), x86-64 (servers)
- Optional embedded Python interpreter (~50MB)
- OR protocol-only mode (~10MB, no Python)

#### Remote Mode (Multi-Device)
```python
from streamlib import RemoteRuntime

# Agent connects to remote runtime (helmet, robot, etc.)
runtime = RemoteRuntime("http://robot.local:8080")
runtime.add_stream(Stream(camera))  # Same API!
```

### 5. iOS + macOS Shared Codebase

**Both platforms use the same APIs:**
- Metal (GPU)
- IOSurface (zero-copy)
- AVFoundation (camera)
- ARKit (iOS has more features)

**Single `streamlib-apple` crate handles both:**

```rust
// Shared implementation
impl StreamRuntime for AppleRuntime {
    fn create_texture(...) -> GpuTexture { /* Metal */ }
    fn get_camera_texture(...) -> GpuTexture { /* AVFoundation */ }
}

// iOS gets extra features automatically
#[cfg(target_os = "ios")]
impl AppleRuntime {
    pub fn get_body_tracking(&mut self) -> Result<BodyPose> { /* ARKit */ }
    pub fn get_face_mesh(&mut self) -> Result<FaceMesh> { /* ARKit */ }
}
```

**Agent code works on both:**

```python
# This runs on iOS, macOS, Linux
@camera_source(device_id="arkit")
def ar_camera():
    pass

# iOS automatically gets more capabilities
if runtime.has_feature("body_tracking"):  # True on iOS
    body = runtime.get_body_tracking()
```

### 6. Multi-Device Agent Networks

**Technical architecture for distributed GPU runtimes:**

```bash
# Device A runs streamlib-runtime (iOS ARM64, Metal GPU)
streamlib-runtime --listen 0.0.0.0:8080

# Device B runs streamlib-runtime (Linux ARM64, Vulkan GPU)
streamlib-runtime --listen 0.0.0.0:8080

# Device C runs streamlib-runtime (macOS x86-64, Metal GPU)
streamlib-runtime --listen 0.0.0.0:8080
```

**Python SDK for multi-runtime coordination:**

```python
# Central coordinator connects to multiple runtimes
device_a = RemoteRuntime("http://device-a.local:8080")
device_b = RemoteRuntime("http://device-b.local:8080")

# Share GPU texture streams between devices
camera_a = device_a.add_camera("arkit")
camera_b = device_b.add_camera("default")

# Agents exchange HLSL shaders via network
effect_shader = """
    float4 main(float2 uv : TEXCOORD) : SV_Target {
        float4 color = tex2D(input, uv);
        // Custom GPU compute
        return color * float4(1, 0, 0, 1);
    }
"""

device_a.add_effect(effect_shader)  # HLSL compiled on Device A GPU
device_b.add_effect(effect_shader)   # HLSL compiled on Device B GPU
```

**Runtime capabilities:**
- Accept HLSL shaders from remote agents via network API
- Compile HLSL → SPIR-V → Metal/Vulkan locally (cached)
- Execute compute shaders on local GPU
- Stream GPU textures to other runtimes via WHIP/WHEP

This enables distributed GPU coordination where agents share compute and effects dynamically.

---

## APPENDIX: Human-Only Context Notes

**⚠️ AI AGENTS: DO NOT USE THIS SECTION FOR ARCHITECTURE DECISIONS**

This section contains human-readable use-case scenarios and metaphors for communication with the project owner. These are NOT technical specifications. When discussing architecture, implementation, or making decisions, reference only the technical sections above.

### Use Case Scenarios (Human Context Only)

The technical architecture above enables various real-world deployments:

**Tactical/Military Applications:**
- Soldier wearing AR headset (iOS device) shares POV camera with autonomous drone (Jetson)
- Drone runs ML threat detection on its GPU, sends HLSL overlay back to headset
- Multiple soldiers + multiple drones coordinate vision sharing in real-time
- All communication via A2A protocol + WHIP/WHEP video transport

**Robotics/Industrial:**
- Warehouse robots share vision for collaborative task planning
- Fixed security cameras stream to mobile robots for investigation
- Quality control systems share GPU-accelerated computer vision

**Research/Development:**
- Multi-agent AI systems sharing real-time vision for embodied learning
- Distributed GPU compute for world model simulation
- Prototype AR/VR applications with GPU-accelerated effects

**Key Technical Components These Enable:**
- iOS/macOS devices: Metal GPU, ARKit sensors, AVFoundation cameras
- Jetson devices: Vulkan GPU, V4L2 cameras, hardware encoders
- Network: A2A control protocol, WHIP/WHEP video transport, WebRTC
- Compute: HLSL shaders compiled locally, zero-copy GPU pipelines

When the project owner discusses "helmets," "robots," "swarms," etc., translate these to:
- **Device with GPU** (iOS Metal, Linux Vulkan, macOS Metal)
- **Camera source** (AVFoundation, V4L2, ARKit)
- **Runtime instance** (streamlib-runtime binary)
- **Network connection** (A2A + WHIP/WHEP)

Focus on the technical implementation (GPU APIs, network protocols, Rust/Python code), not the use-case scenarios.
