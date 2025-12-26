# StreamLib

A foundational streaming library built from the ground up for low-latency livestreaming and agentic video development.

**Zero external dependencies on GStreamer, FFmpeg, or similar frameworks.** StreamLib is an entirely new approach to real-time video streaming, designed to be cross-platform from day one.

## Vision

StreamLib is building a truly headless streaming library that requires no display server or window system. Run the same code on headless servers, embedded devices, edge compute, or full GUI applications.

This is an alternative to solutions like NVIDIA DeepStream that require CUDA and lock you into specific hardware. StreamLib uses platform-native APIs (Metal, Vulkan, DirectX) to deliver GPU acceleration without vendor lock-in.

**Inspired by game engine architecture.** Like Unreal Engine enables cross-platform game development, StreamLib applies the same principles to video streaming. A unified development and rendering library that abstracts platform differences, letting you write once and deploy anywhere.

**Target environments:**
- Headless cloud servers (no X11/Wayland required)
- Embedded devices and edge compute
- Desktop applications with full GUI
- Mobile devices (iOS, Android planned)

**Language support (roadmap):**
- Rust (native, available now)
- Python (planned, separate repository)
- TypeScript (planned)

All platforms supported out of the box—Linux, macOS, Windows—without convoluted setup. Every processor is designed to be cross-platform from the start.

## Features

- **Zero legacy dependencies** - No GStreamer, FFmpeg, or libav. Pure Rust with platform-native APIs
- **Graph-based processing pipeline** - Build complex media workflows by connecting processors
- **GPU-accelerated video** - Hardware-accelerated encoding/decoding via Metal and wgpu
- **Real-time audio** - Low-latency audio processing with CLAP plugin support
- **Built for agentic workflows** - Designed for AI-driven video processing and automation
- **Cross-platform** - See platform support table below

## Platform Support

| Platform | Status | Notes |
|----------|--------|-------|
| macOS | ✅ Supported | Primary development platform |
| iOS | 🚧 Partial | Core functionality works |
| Linux | 📋 Planned | Up next |
| Windows | 📋 Planned | Up next |

## Quick Start

See [examples/camera-display](examples/camera-display) for a minimal working example that captures video from a camera and displays it in a window.

## Documentation

- [Architecture Overview](docs/) - How StreamLib works
- [API Documentation](https://docs.rs/streamlib) - Rust API reference
- [Examples](examples/) - Working example applications

## Examples

| Example | Description |
|---------|-------------|
| [camera-display](examples/camera-display) | Camera capture and display |
| [microphone-reverb-speaker](examples/microphone-reverb-speaker) | Audio processing with CLAP plugins |
| [camera-audio-recorder](examples/camera-audio-recorder) | Record camera + audio to MP4 |
| [webrtc-cloudflare-stream](examples/webrtc-cloudflare-stream) | WebRTC streaming to Cloudflare |
| [whep-player](examples/whep-player) | WHEP (WebRTC egress) player |

Run an example:
```bash
cargo run -p camera-display
```

## License & Business Model

StreamLib uses an **open-core model** inspired by game engines like Unity and Unreal.

### The Simple Version

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                                                                             │
│   ✅  BUILD PROCESSORS  →  FREE                                            │
│   ✅  SELL PROCESSORS   →  FREE (you keep 100%)                            │
│   ✅  PRIVATE SOURCE    →  FREE (no obligation to share)                   │
│                                                                             │
│   💼  RUN THE RUNTIME IN PRODUCTION  →  Commercial license required*       │
│                                                                             │
│   * Unless you fall under permitted uses (see below)                       │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Build Anything, Own Everything

**We don't own your Processors.** Just like Epic doesn't own games built with Unreal Engine, we don't own what you build with StreamLib.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          YOUR APPLICATION                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│   ┌───────────────┐   ┌───────────────┐   ┌───────────────┐                │
│   │  Your Custom  │   │   Community   │   │  Commercial   │                │
│   │   Processor   │   │  Processors   │   │  Processors   │   YOURS        │
│   │               │   │               │   │               │   100%         │
│   │  (private or  │   │ (open source) │   │  (for sale)   │                │
│   │   commercial) │   │               │   │               │                │
│   └───────┬───────┘   └───────┬───────┘   └───────┬───────┘                │
│           │                   │                   │                         │
│           ▼                   ▼                   ▼                         │
│   ┌─────────────────────────────────────────────────────────────────┐      │
│   │                    Processor API                                 │      │
│   │         ReactiveProcessor • ContinuousProcessor • etc.          │      │
│   │                                                                  │      │
│   │    LinkInput<T> ──────────────────────────────► LinkOutput<T>   │      │
│   │                                                                  │      │
│   │              VideoFrame • AudioFrame • DataFrame                 │      │
│   └─────────────────────────────┬───────────────────────────────────┘      │
│                                 │                                           │
├─────────────────────────────────┼───────────────────────────────────────────┤
│                                 ▼                                           │
│   ┌─────────────────────────────────────────────────────────────────┐      │
│   │                                                                  │      │
│   │                    StreamLib Runtime Engine                      │      │
│   │                                                                  │      │
│   │    Graph Compiler • Scheduler • GPU Context • Thread Pool       │  ←── │
│   │                                                                  │      │
│   │              BUSL-1.1 Licensed (see details below)              │      │
│   │                                                                  │      │
│   └─────────────────────────────────────────────────────────────────┘      │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### What You Can Do (No License Required)

| Activity | Allowed? | Notes |
|----------|:--------:|-------|
| Build custom Processors | ✅ | For yourself, clients, or to sell |
| Sell Processors commercially | ✅ | Keep 100% of revenue |
| Keep Processor source private | ✅ | No obligation to open source |
| Build Processors for clients | ✅ | Contractors/consultants welcome |
| Personal/hobby projects | ✅ | No restrictions |
| Educational/research use | ✅ | Universities, students, researchers |
| Open source projects | ✅ | OSI-approved licenses |
| Commercial apps (StreamLib as component) | ✅ | Video conferencing, security cameras, robotics, etc. |

### What Requires a Commercial License

| Activity | License Required |
|----------|:----------------:|
| Building a competing streaming SDK/framework | ✅ Commercial |
| Offering StreamLib as a managed/hosted SaaS | ✅ Commercial |
| Reselling StreamLib's core functionality as a service | ✅ Commercial |

### Why This Model?

**For the community:** We want a thriving ecosystem of Processors. Whether you're building an AI video analyzer, a custom encoder, or a specialized filter—build it, sell it, keep it private. Your choice.

**For sustainability:** The runtime engine requires significant investment to build and maintain. Commercial licenses from companies building competing platforms fund continued development.

**For trust:** On **January 1, 2029**, StreamLib automatically converts to [Apache License 2.0](LICENSES/Apache-2.0.txt). The code will be fully open source with no restrictions, guaranteed.

### The Game Engine Analogy

| Game Engine | StreamLib |
|-------------|-----------|
| Engine (Unity/Unreal) | Runtime Engine |
| Games you build | Processors you build |
| Asset Store | Processor marketplace (coming soon) |
| You own your games | You own your Processors |
| Engine is licensed | Runtime is BUSL-1.1 |

### Commercial Licensing

Need a commercial license? Two options:

**[Commercial License](docs/license/COMMERCIAL-LICENSING.md)** — For companies building streaming platforms or competing products.

**[Partner License](docs/license/PARTNER-LICENSING.md)** — For consultants, agencies, and integrators. Includes co-marketing, roadmap input, and early access.

**Contact:** fontanezj1@gmail.com

### Full License

StreamLib is licensed under the [Business Source License 1.1](LICENSE). See the LICENSE file for complete terms including the Additional Use Grant that explicitly permits Processor development.

## Contributing

Contributions are welcome! By submitting a pull request, you agree to license your
contribution under the same BUSL-1.1 terms.

See [CLA.md](docs/license/CLA.md) for the Contributor License Agreement.

## Project Structure

```
streamlib/
├── libs/
│   ├── streamlib/           # Core library
│   └── streamlib-macros/    # Procedural macros
├── examples/                # Example applications
└── docs/                    # Documentation
```

## Requirements

- Rust 1.75+
- macOS 13+ (for Apple framework features)
- Metal-capable GPU (for video processing)

## Building

```bash
# Build the library
cargo build -p streamlib

# Run tests
cargo test -p streamlib

# Build all examples
cargo build --workspace
```

## Status

StreamLib is under active development. APIs may change between versions.

## Contact

- **Author:** Jonathan Fontanez
- **Email:** fontanezj1@gmail.com
- **Repository:** https://github.com/tato123/streamlib
