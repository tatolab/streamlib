# XPC Alternatives Research: Cross-Process Frame Sharing Without launchd

**Date:** 2026-01-12
**Status:** Research Complete
**Confidence Score:** 85%

---

## Executive Summary

The current XPC implementation using `xpc_connection_create_mach_service` with `bootstrap_register` fails because `bootstrap_register` is deprecated and modern macOS restricts dynamic service registration without launchd plists. This document analyzes alternatives for cross-process IOSurface sharing between a parent process and spawned subprocesses.

**Recommended Solution:** Unix Domain Socket for control channel + Mach message for IOSurface port transfer (Solution D below).

---

## Problem Statement

### Current Implementation Issues

1. **`bootstrap_register` is deprecated** (since macOS 10.5 Leopard)
2. **Dynamic Mach service registration requires launchd** - Services must be declared in launchd plists
3. **Error observed:** `CONNECTION_INVALID - The named service could not be found in the launchd namespace`
4. **Subprocess scenario:** We spawn Python subprocesses that need to exchange IOSurface-backed VideoFrames with the parent Rust process

### Requirements

1. Share IOSurface references between parent and subprocess
2. Bidirectional communication (parent sends frames to subprocess, subprocess returns processed frames)
3. No launchd plist configuration
4. Support for dynamically spawned subprocesses
5. Low latency (real-time video processing)

---

## Research Findings

### 1. XPC Pipes (`xpc_pipe_create`, `xpc_pipe_routine`)

**Status:** NOT RECOMMENDED

XPC pipes are private/undocumented Apple APIs used internally by system frameworks.

```c
// Private API declarations (from Apple's open-source Libinfo)
__attribute__((weak_import)) xpc_pipe_t xpc_pipe_create(const char *name, uint64_t flags);
__attribute__((weak_import)) void xpc_pipe_invalidate(xpc_pipe_t pipe);
__attribute__((weak_import)) int xpc_pipe_routine(xpc_pipe_t pipe, xpc_object_t message, xpc_object_t *reply);
```

**Problems:**
- Private API - not stable, could break in any macOS update
- Would be rejected from App Store
- Still requires launchd for named pipes
- Limited documentation available

**Sources:**
- [Apple Open Source - Libinfo](https://opensource.apple.com/source/Libinfo/Libinfo-406.17/lookup.subproj/ds_module.c)
- [xpc-sys Rust crate](https://lib.rs/crates/xpc-sys)

---

### 2. IOSurface Sharing Mechanisms

IOSurface is a kernel-managed texture memory that can be shared across processes without data copying.

#### 2a. Global ID Approach (DEPRECATED)

```c
// Sender
IOSurfaceID id = IOSurfaceGetID(surface);
// Send `id` via any IPC mechanism

// Receiver
IOSurfaceRef surface = IOSurfaceLookup(id);
```

**Status:** NOT VIABLE

- `kIOSurfaceIsGlobal` flag deprecated in macOS 10.11
- Security vulnerability - any process could access surfaces by guessing IDs
- Sandboxed apps cannot use this
- Will likely be removed entirely in future macOS versions

**Sources:**
- [Apple Developer Forums - kIOSurfaceIsGlobal](https://developer.apple.com/forums/thread/18958)
- [Apple Documentation - kIOSurfaceIsGlobal](https://developer.apple.com/documentation/iosurface/kiosurfaceisglobal)

#### 2b. Mach Port Approach (RECOMMENDED)

```c
// Sender
mach_port_t port = IOSurfaceCreateMachPort(surface);
// Transfer `port` via Mach messaging to remote process

// Receiver
IOSurfaceRef surface = IOSurfaceLookupFromMachPort(port);
// Must call mach_port_deallocate() on both sides when done
```

**Status:** VIABLE - Requires mach port transfer mechanism

**Advantages:**
- Officially supported by Apple
- Zero-copy sharing
- Secure - requires explicit port transfer

**Challenge:** How to transfer the mach port to the subprocess without launchd

**Sources:**
- [fdiv.net - IOSurfaceCreateMachPort example](https://fdiv.net/2011/01/27/example-iosurfacecreatemachport-and-iosurfacelookupfrommachport)
- [Apple Sample Code - MultiGPUIOSurface](https://developer.apple.com/library/archive/samplecode/MultiGPUIOSurface/Introduction/Intro.html)
- [Russ Bishop - Cross-process Rendering](http://www.russbishop.net/cross-process-rendering)

#### 2c. XPC Object Approach (Current Implementation)

```c
// Sender (via XPC connection)
xpc_object_t xpc_surface = IOSurfaceCreateXPCObject(surface);
// Send via xpc_connection_send_message()

// Receiver (via XPC event handler)
IOSurfaceRef surface = IOSurfaceLookupFromXPCObject(xpc_surface);
```

**Status:** Requires working XPC connection first

---

### 3. Mach Port Inheritance and Transfer

#### Key Facts About Mach Ports

- **Mach ports are NOT inherited across `fork()`** (unlike file descriptors)
- Each process has its own port namespace - port names are local to a process
- Special ports (task port, bootstrap port) ARE inherited
- `bootstrap_register()` is deprecated; `bootstrap_register2()` is SPI (system private interface)

**Sources:**
- [Darling Docs - Mach Ports](https://docs.darlinghq.org/internals/macos-specifics/mach-ports.html)
- [HackTricks - macOS IPC](https://book.hacktricks.xyz/macos-hardening/macos-security-and-privilege-escalation/macos-proces-abuse/macos-ipc-inter-process-communication)
- [yo-yo-yo-jbo/macos_mach_ports](https://github.com/yo-yo-yo-jbo/macos_mach_ports/)

#### Mach Port Transfer Methods

1. **Via bootstrap server** - Requires launchd registration (current broken approach)
2. **Via Mach messages** - Requires a mach port to send messages on (chicken-and-egg)
3. **Via special ports** - Limited slots, intended for system services
4. **Via CFMessagePort** - Uses `bootstrap_register2()` SPI internally

---

### 4. Unix Domain Sockets

Unix domain sockets support `SCM_RIGHTS` for passing file descriptors between processes.

```c
// Can pass file descriptors via sendmsg/recvmsg with SCM_RIGHTS
struct cmsghdr {
    socklen_t cmsg_len;    // Data byte count
    int       cmsg_level;  // SOL_SOCKET
    int       cmsg_type;   // SCM_RIGHTS
    // followed by file descriptors
};
```

**CRITICAL LIMITATION:** On macOS, **Mach ports are NOT file descriptors**. SCM_RIGHTS cannot transfer mach ports directly.

However, Unix sockets can be used as a control channel to coordinate mach port exchange via other mechanisms.

**Sources:**
- [Cloudflare Blog - Know your SCM_RIGHTS](https://blog.cloudflare.com/know-your-scm_rights/)
- [Medium - File Descriptor Transfer over Unix Domain Sockets](https://copyconstruct.medium.com/file-descriptor-transfer-over-unix-domain-sockets-dcbbf5b3b6ec)

---

### 5. CFMessagePort / NSMachBootstrapServer

CFMessagePort internally uses `bootstrap_register2()` SPI to register with the bootstrap server without requiring a launchd plist.

```objc
// Create local port (registers automatically)
CFMessagePortRef localPort = CFMessagePortCreateLocal(
    kCFAllocatorDefault,
    CFSTR("com.example.myservice"),
    callback,
    &context,
    NULL
);
```

**Status:** POTENTIALLY VIABLE

**Advantages:**
- Works without explicit launchd plist
- Apple-supported API
- Can extract underlying mach port

**Disadvantages:**
- May have sandbox restrictions
- Higher-level API with overhead
- Subject to `bootstrap_register2()` behavior which could change

**Sources:**
- [Damien Deville - IPC with Mach messages](https://ddeville.me/2015/02/interprocess-communication-on-ios-with-mach-messages/)

---

## Proposed Solutions

### Solution A: CFMessagePort for Bootstrap (Medium Confidence: 70%)

**Architecture:**
```
Parent Process                          Subprocess
      |                                      |
      |-- CFMessagePortCreateLocal() --------|
      |   (registers "com.streamlib.{pid}")  |
      |                                      |
      |-- spawn subprocess with env var ---->|
      |   STREAMLIB_SERVICE_NAME             |
      |                                      |
      |<-- CFMessagePortCreateRemote() ------|
      |   (connects via service name)        |
      |                                      |
      |<==== Exchange mach ports via =======>|
      |      CFMessagePort messages          |
      |                                      |
      |<==== IOSurface mach ports ==========>|
```

**Pros:**
- Uses supported Apple API
- No explicit launchd configuration
- CFMessagePort handles bootstrap registration

**Cons:**
- CFMessagePort is older API, less supported than XPC
- May have sandbox issues in some contexts
- Additional complexity of wrapping

---

### Solution B: Unix Socket Control + Raw Mach Messages (Medium-High Confidence: 75%)

**Architecture:**
```
Parent Process                          Subprocess
      |                                      |
      |-- Create Unix domain socket -------->|
      |   (pass path via env/arg)            |
      |                                      |
      |-- Create receive port ---------------|
      |   mach_port_allocate()               |
      |                                      |
      |-- Send port name via socket -------->|
      |   (just an integer identifier)       |
      |                                      |
      |<-- Subprocess creates port ----------|
      |                                      |
      |-- Exchange ports via task_for_pid -->|
      |   mach_port_insert_right()           |
      |                                      |
      |<==== Bidirectional mach msgs =======>|
```

**CRITICAL ISSUE:** `task_for_pid()` requires special entitlements on modern macOS (SIP, sandbox restrictions). This approach is NOT viable for general use.

---

### Solution C: Inherited Bootstrap Port + Dynamic Registration (Low Confidence: 60%)

**Architecture:**
```
Parent Process                          Subprocess
      |                                      |
      |-- bootstrap_register2() -------------|
      |   (via CFMessagePort)                |
      |                                      |
      |-- fork/exec with inherited --------->|
      |   bootstrap port                     |
      |                                      |
      |<-- bootstrap_look_up() --------------|
      |   (child finds parent's service)     |
      |                                      |
      |<==== Mach messages =================>|
```

**Pros:**
- Leverages inherited bootstrap port
- Standard pattern

**Cons:**
- Relies on `bootstrap_register2()` SPI behavior
- May not work reliably across all macOS versions
- Sandbox restrictions apply

---

### Solution D: Unix Socket + Mach Port Fileport (RECOMMENDED - High Confidence: 85%)

**Key Insight:** macOS provides `fileport_makeport()` and `fileport_makefd()` to convert between file descriptors and mach ports. This allows passing mach ports over Unix sockets!

**Architecture:**
```
Parent Process                          Subprocess
      |                                      |
      |-- Create socketpair() -------------->|
      |   (one fd inherited by child)        |
      |                                      |
      |-- fork/exec subprocess ------------->|
      |   (child inherits socket fd)         |
      |                                      |
      |-- Create mach port for comm ---------|
      |   mach_port_allocate()               |
      |                                      |
      |-- Convert to fd via ----------------->|
      |   fileport_makefd()                  |
      |                                      |
      |-- Send fd via SCM_RIGHTS ----------->|
      |                                      |
      |<-- Child converts fd to port --------|
      |   fileport_makeport()                |
      |                                      |
      |<==== Now have shared mach port =====>|
      |      for IOSurface transfer          |
```

**Detailed Steps:**

1. **Parent creates socketpair:**
   ```c
   int socks[2];
   socketpair(AF_UNIX, SOCK_STREAM, 0, socks);
   // socks[0] for parent, socks[1] for child
   ```

2. **Parent spawns subprocess:**
   - Child inherits `socks[1]`
   - Pass fd number via environment variable or argument

3. **Parent creates communication mach port:**
   ```c
   mach_port_t comm_port;
   mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &comm_port);
   mach_port_insert_right(mach_task_self(), comm_port, comm_port, MACH_MSG_TYPE_MAKE_SEND);
   ```

4. **Parent converts mach port to file descriptor:**
   ```c
   int fd = fileport_makefd(comm_port);
   ```

5. **Parent sends fd via SCM_RIGHTS over Unix socket:**
   ```c
   // Standard sendmsg() with SCM_RIGHTS ancillary data
   ```

6. **Child receives fd and converts back to mach port:**
   ```c
   // recvmsg() to get fd
   mach_port_t port = fileport_makeport(received_fd);
   ```

7. **Both processes now share the mach port** - use Mach messages for communication

8. **For IOSurface transfer:**
   ```c
   // Sender
   mach_port_t surface_port = IOSurfaceCreateMachPort(surface);
   // Send surface_port in mach message via shared comm_port

   // Receiver
   IOSurfaceRef surface = IOSurfaceLookupFromMachPort(received_surface_port);
   ```

**Pros:**
- Uses only public, supported APIs
- Works without launchd
- File descriptors naturally inherited across fork()
- Mach ports can be embedded in mach messages
- Zero-copy IOSurface sharing

**Cons:**
- More complex implementation
- Requires Mach message handling (low-level)
- Need to handle port lifecycle carefully

---

### Solution E: Anonymous XPC Connection via Endpoint (Alternative - High Confidence: 80%)

XPC supports "anonymous connections" via listener endpoints that don't require launchd registration.

**Architecture:**
```
Parent Process                          Subprocess
      |                                      |
      |-- xpc_connection_create() -----------|
      |   (anonymous listener)               |
      |                                      |
      |-- Create listener endpoint ----------|
      |   xpc_endpoint_create()              |
      |                                      |
      |-- Serialize endpoint ----------------|
      |   (to bytes/string somehow)          |
      |                                      |
      |-- Pass to child via env/socket ----->|
      |                                      |
      |<-- Child recreates connection -------|
      |                                      |
      |<==== XPC messages with IOSurface ===>|
```

**ISSUE:** XPC endpoints can only be transferred over existing XPC connections, creating a chicken-and-egg problem. Need to investigate if there's a way to serialize/deserialize endpoints via other mechanisms.

**Sources:**
- [objc.io - XPC](https://www.objc.io/issues/14-mac/xpc/) - mentions listener endpoints

---

## Comparison Matrix

| Solution | Launchd Required | IOSurface Support | Complexity | Stability | Confidence |
|----------|-----------------|-------------------|------------|-----------|------------|
| A: CFMessagePort | No* | Via Mach | Medium | Medium | 70% |
| B: Unix + task_for_pid | No | Via Mach | High | Low | N/A (blocked by entitlements) |
| C: Inherited Bootstrap | No* | Via Mach | Medium | Low | 60% |
| **D: Unix + Fileport** | **No** | **Via Mach** | **High** | **High** | **85%** |
| E: XPC Endpoint | No | Via XPC | Medium | Unknown | 80% |

\* Relies on bootstrap_register2() SPI

---

## Recommended Implementation: Solution D

### Why Solution D?

1. **No launchd dependency** - Uses socketpair and fileport APIs
2. **All public APIs** - `socketpair()`, `fileport_makefd()`, `fileport_makeport()`, Mach messaging
3. **Proven pattern** - Similar to how other IPC mechanisms bootstrap themselves
4. **Full IOSurface support** - Can transfer IOSurface mach ports via established channel
5. **Bidirectional** - Both processes can send and receive

### Implementation Phases

#### Phase 1: Unix Socket Bootstrap
- Create socketpair before forking
- Pass fd to child via command-line argument
- Establish basic message exchange over socket

#### Phase 2: Mach Port Exchange via Fileport
- Parent allocates mach port
- Convert to fd with `fileport_makefd()`
- Send fd via `SCM_RIGHTS`
- Child converts back with `fileport_makeport()`

#### Phase 3: IOSurface Channel
- Use established mach port for Mach messages
- Embed IOSurface mach ports (`IOSurfaceCreateMachPort()`) in messages
- Receiver uses `IOSurfaceLookupFromMachPort()`

#### Phase 4: Higher-Level Protocol
- Define message format for frame metadata (timestamp, dimensions, pixel format)
- Implement acknowledgment/flow control
- Handle cleanup and error cases

### Rust Implementation Notes

```rust
// Key crates needed:
// - nix (for socketpair, sendmsg, recvmsg, SCM_RIGHTS)
// - mach2 (for mach_port_allocate, mach_msg)

// fileport APIs (need bindings):
extern "C" {
    fn fileport_makeport(fd: i32) -> mach_port_t;
    fn fileport_makefd(port: mach_port_t) -> i32;
}
```

### Python Subprocess Side

```python
import socket
import os

# Child receives socket fd from parent
socket_fd = int(os.environ['STREAMLIB_SOCKET_FD'])
sock = socket.socket(fileno=socket_fd)

# Receive fileport fd via SCM_RIGHTS
# Convert to mach port using cffi/ctypes bindings to fileport_makeport()

# Now can receive IOSurface mach ports via mach messages
```

---

## Risk Assessment

### Technical Risks

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| `fileport_*` API changes | Low | High | Document fallback to CFMessagePort |
| Mach message complexity | Medium | Medium | Thorough testing, error handling |
| Memory leaks (port lifecycle) | Medium | Medium | RAII wrappers, comprehensive cleanup |
| Performance overhead | Low | Low | Benchmark against current XPC |

### Platform Compatibility

- **macOS 10.x+**: Should work (fileport APIs available)
- **iOS**: May work but needs testing; sandbox restrictions apply
- **Catalyst**: Untested

---

## Conclusion

**Recommended Approach:** Solution D (Unix Socket + Fileport + Mach Messages)

**Confidence Score:** 85%

This approach:
1. Eliminates launchd dependency entirely
2. Uses only public, documented APIs
3. Provides efficient zero-copy IOSurface sharing
4. Supports bidirectional communication
5. Is the most robust long-term solution

The main complexity is implementing the Mach message protocol correctly, but this is a one-time cost that provides a stable foundation for cross-process GPU buffer sharing.

---

## References

1. [Apple Developer - IOSurface](https://developer.apple.com/documentation/iosurface)
2. [fdiv.net - IOSurfaceCreateMachPort Example](https://fdiv.net/2011/01/27/example-iosurfacecreatemachport-and-iosurfacelookupfrommachport)
3. [fdiv.net - mach_port_t for IPC](https://fdiv.net/2011/01/14/machportt-inter-process-communication)
4. [Apple Sample - MultiGPUIOSurface](https://developer.apple.com/library/archive/samplecode/MultiGPUIOSurface/Introduction/Intro.html)
5. [Russ Bishop - Cross-process Rendering](http://www.russbishop.net/cross-process-rendering)
6. [objc.io - XPC](https://www.objc.io/issues/14-mac/xpc/)
7. [HackTricks - macOS IPC](https://book.hacktricks.xyz/macos-hardening/macos-security-and-privilege-escalation/macos-proces-abuse/macos-ipc-inter-process-communication)
8. [Darling Docs - Mach Ports](https://docs.darlinghq.org/internals/macos-specifics/mach-ports.html)
9. [Damien Deville - IPC with Mach Messages](https://ddeville.me/2015/02/interprocess-communication-on-ios-with-mach-messages/)
10. [Cloudflare - SCM_RIGHTS](https://blog.cloudflare.com/know-your-scm_rights/)
11. [Dennis Babkin - Mach Messages Example](https://dennisbabkin.com/blog/?t=interprocess-communication-using-mach-messages-for-macos)
