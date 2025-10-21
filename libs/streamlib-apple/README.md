# streamlib-apple

Metal/IOSurface GPU backend for macOS and iOS.

## Platform Support

- **macOS**: Full Metal support, IOSurface, AVFoundation cameras, limited ARKit
- **iOS**: Full Metal support, IOSurface, AVFoundation cameras, full ARKit (body/face tracking)

## Modules

- `metal.rs` - Metal GPU texture operations (both platforms)
- `iosurface.rs` - Zero-copy texture sharing (both platforms)
- `camera.rs` - AVFoundation camera capture (both platforms, platform-specific features)
- `arkit.rs` - ARKit integration (iOS full, macOS limited)

## Build

```bash
# macOS
cargo build --target x86_64-apple-darwin

# iOS
cargo build --target aarch64-apple-ios
```

## Implementation Status

🚧 **SCAFFOLDED** - Structure created, implementation in progress.
