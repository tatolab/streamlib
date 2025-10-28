> Ok, I'm trying to determine next thing to work on, should we add support for python (including streamlib-python and also in the runtime supporting the add processor with a
> python interpreter), udp sources and sinks for smpte 2110 compatibility, or something else?

⏺ Great question! Let me analyze this strategically based on your vision and current state:

Current State Assessment

Looking at what we just built:

- ✅ MCP server - AI agents can discover processors
- ✅ Auto-registration - Processors automatically advertise capabilities
- ✅ Schema system - Rich metadata for AI understanding
- 🚧 Tool execution - Returns placeholders (no runtime integration yet)

Critical gap: AI agents can discover processors but can't actually USE them yet.

Strategic Options Analysis

Option 1: Complete StreamRuntime Integration 🎯

Make MCP tools actually functional

Impact: 🔥🔥🔥 HIGH - Completes the loop we just started

Why this makes sense:

- AI agents can currently discover processors but can't build pipelines
- The MCP tools (add_processor, connect_processors) return placeholders
- This would make everything we just built immediately useful
- Proves the concept end-to-end: AI discovers → AI builds → pipeline runs

What it unlocks:

- Claude Code can actually build camera → display pipelines
- Testing the full AI agent workflow
- Real validation of the schema/descriptor approach
- Foundation for everything else

Estimated scope: Medium (runtime exists, need to wire up MCP tools)

Option 2: Python Support 🐍

Enable AI agents to write processors

Impact: 🔥🔥🔥 HIGH - Aligns perfectly with "agents that share real-time vision"

Why this is compelling:

- AI agents could generate Python processors on the fly
- "Write a processor that detects faces" → agent writes Python code → processor runs
- Massive ecosystem unlock (ML libraries, rapid prototyping)
- Your CLAUDE.md suggests you had this working: "The decorator API works. The examples work."

What it unlocks:

- AI-generated custom processors
- Rapid experimentation (agents try different approaches)
- Python's ML ecosystem (PyTorch, OpenCV, etc.)
- Users can prototype in Python, deploy in Rust

Complexity:

- PyO3 integration (Rust ↔ Python boundary)
- Python processor lifecycle in Rust runtime
- GIL management for real-time guarantees

Estimated scope: Large (new language binding, runtime integration)

Option 3: SMPTE 2110 UDP Sources/Sinks 📡

Professional broadcast interoperability

Impact: 🔥 MEDIUM - Specialized but critical for pro workflows

Why this matters:

- Real-world professional use case
- Broadcast equipment interoperability
- Validates "real-time video streaming" mission

What it unlocks:

- Integration with professional broadcast gear
- Multi-camera production workflows
- IP-based video distribution

Considerations:

- More specialized audience
- Needs runtime to execute anyway
- Important but not as strategic for AI agent vision

Estimated scope: Medium (network protocol, timing-critical)

My Recommendation 💡

1st Priority: Complete StreamRuntime Integration

Rationale:

- You just invested in MCP server and AI discovery
- Finishing runtime integration completes that story
- Makes everything immediately testable and usable
- Quick win that proves the architecture works
- Foundation for both Python and SMPTE 2110

This means:

- Implement add_processor tool (create processor instances)
- Implement connect_processors tool (build pipelines)
- Implement list_processors tool (show running processors)
- Wire up the StreamRuntime to execute pipelines
- Test: "Claude, build me a camera → display pipeline"

2nd Priority: Python Support

Rationale:

- Aligns with core vision: "agents that share real-time vision"
- Enables AI agents to write custom processors
- Powerful combination: AI discovers + AI generates + Rust executes
- Your CLAUDE.md suggests this is the natural next step

The vision:

# AI agent generates this on the fly:

@streamlib.processor
def face_detector(frame: VideoFrame) -> ObjectDetections: # AI writes ML code here
return detect_faces(frame)

Then Rust runtime executes it with real-time guarantees.

3rd Priority: SMPTE 2110

Rationale:

- Important for professional adoption
- But less strategic for the AI agent vision
- Can come after the agent workflow is proven

Suggested Roadmap

Week 1-2: StreamRuntime Integration
├─ Wire up MCP tools to runtime
├─ Implement processor lifecycle (add/remove)
├─ Implement port connections
└─ Test with AI agent (Claude Code builds pipelines)

Week 3-5: Python Support
├─ PyO3 bindings (streamlib-python)
├─ Python processor trait implementation
├─ Runtime support for Python processors
└─ Test: AI generates Python processor, runs in pipeline

Week 6-7: SMPTE 2110
├─ UDP source processor
├─ UDP sink processor
└─ SMPTE 2110 timing/framing

The Killer Feature 🚀

If you do Runtime + Python, you get:

"AI agents that discover processors, generate custom processors, and execute pipelines in real-time"

That's the impossible-sounding thing that becomes real.

---

TL;DR: I'd go StreamRuntime integration first (completes what we started), then Python support (unlocks AI-generated processors), then SMPTE 2110 (professional workflows).

What do you think? Does this align with your vision?

> Don't we need to add support for a language like python in order to add a processor? For example, how would adding a rust processor work in the sense that the agent is
> sending code, we would need to compile it wouldn't we?

⏺ You're absolutely right! I completely missed this critical point. Let me revise my thinking:

The Core Problem

For AI agents to ADD processors, they need to send CODE. And you're right - we have two options:

Option A: Rust Processors (Impractical)

Agent generates Rust → We compile at runtime → Dynamic linking → Execute

Problems:

- Need rustc available at runtime
- Compilation is SLOW (seconds/minutes)
- Security nightmare (executing arbitrary Rust)
- Complex dynamic library loading
- Not suitable for real-time agent workflows

Option B: Python Processors (Practical) ✅

Agent generates Python → Python interpreter executes → PyO3 bridges to Rust

Why this works:

- No compilation step (interpreted)
- Fast execution (milliseconds to start)
- Well-understood security model (sandboxing possible)
- AI agents are excellent at generating Python
- Rich ecosystem (OpenCV, PyTorch, etc.)

Revised Understanding

Pre-compiled processors (built-in):

- CameraProcessor, DisplayProcessor (Rust, compiled into binary)
- AI agents can discover and connect these
- But agents CAN'T create new ones at runtime

Dynamic processors (runtime):

- AI agent generates Python code on the fly
- add_processor tool accepts Python code
- Python interpreter executes via PyO3
- This is how agents create CUSTOM processors

So the REAL Roadmap is:

Phase 1: Python Support (REQUIRED FIRST) 🐍

Why this must come first:

- Without it, add_processor can only use pre-compiled processors
- Agents can't generate custom logic
- The whole "AI agents build pipelines" vision is limited

What we need:

# AI agent sends this code to add_processor tool:

import streamlib

@streamlib.processor(
name="FaceDetector",
inputs={"video": streamlib.VideoFrame},
outputs={"detections": streamlib.ObjectDetections}
)
def process(inputs):
frame = inputs["video"] # AI-generated detection logic here
faces = detect_faces(frame)
return {"detections": faces}

Implementation:

- streamlib-python crate (PyO3 bindings)
- Python processor wrapper (implements StreamProcessor trait)
- Python interpreter lifecycle in Rust runtime
- MCP tool: add_processor(code: str, language: "python")

Phase 2: Runtime Integration 🎯

Now add_processor actually works:
{
"tool": "add_processor",
"arguments": {
"code": "# Python code here",
"language": "python"
}
}

Then connect processors:
{
"tool": "connect_processors",
"arguments": {
"from_processor": "CameraProcessor",
"from_port": "video",
"to_processor": "FaceDetector",
"to_port": "video"
}
}

Phase 3: SMPTE 2110 📡

- Now this is just another processor (can be Rust or Python)

The Corrected Vision 🚀

Without Python:

- AI agents discover: CameraProcessor, DisplayProcessor
- AI agents connect: camera → display
- Limited - can only use built-in processors

With Python:

- AI agents discover: CameraProcessor, DisplayProcessor
- AI agents generate: FaceDetector (Python), ObjectTracker (Python)
- AI agents connect: camera → FaceDetector → ObjectTracker → display
- Unlimited - agents create custom processing logic

Alternative: WASM?

Could also consider WASM for sandboxed execution:

- Agent generates Rust/Python → Compiles to WASM → Execute
- Better security sandboxing
- But more complex toolchain
- Python is simpler for MVP

My Revised Recommendation

Start with Python support NOW, because:

1. It's required for dynamic processor creation
2. Without it, add_processor is mostly useless
3. It unlocks the AI agent vision you described
4. Your CLAUDE.md suggests you had Python working before

Then wire up runtime integration to use those Python processors.

Does this make more sense? Should we start with Python support as the foundation?

> Hmm, yeah I think wasm might be a future case but there are sooooo many limitations (for example wasm gpu is a whole evolving standard vs python which is just a wrapper on
> top of our rust code + whatever it brings)

⏺ Exactly! You nailed the key constraint:

WASM GPU Problem 🚫

WASM limitations:

- WebGPU in WASM is still experimental/evolving
- Can't directly access Metal/Vulkan/D3D12 textures
- No IOSurface, no DMA-BUF, no zero-copy GPU interop
- Isolated from the host GPU entirely
- Would need to copy data in/out (kills your zero-copy architecture)

Your architecture depends on:

- Direct Metal texture access (IOSurface on macOS)
- wgpu-hal for zero-copy GPU sharing
- Platform-specific GPU primitives
- This is WHY it's fast!

Python Wins 🐍

Python gives you:

- Direct FFI to Rust code (PyO3)
- Pass GPU texture handles across boundary
- Python sees WebGPU textures as opaque handles
- Rust manages actual GPU memory
- Python can still do CPU work (ML inference, logic)
- Brings entire Python ecosystem (NumPy, OpenCV, PyTorch, etc.)

Example flow:
@streamlib.processor
def face_detector(frame: VideoFrame): # frame.texture is opaque handle (Rust manages GPU memory) # But we can read pixels if needed for ML:
pixels = frame.to_numpy() # Only if necessary
faces = detect_faces(pixels)
return ObjectDetections(faces)

For pure GPU operations:
@streamlib.processor
def blur_filter(frame: VideoFrame): # Pass GPU texture directly to shader # Python just orchestrates, GPU does the work
return apply_shader(frame, blur_shader)

The Reality Check ✅

Python processors can:

- Receive WebGPU textures (zero-copy)
- Run ML models (PyTorch/ONNX on CPU/GPU)
- Orchestrate GPU operations (shaders, filters)
- Access full Python ecosystem
- Be generated by AI agents in seconds

WASM processors would:

- Be sandboxed from GPU
- Require data copying (kills performance)
- Have limited tooling
- Be harder for AI agents to generate
- Miss the Python ecosystem

So... Python First? 🚀

Should we start building streamlib-python now? Here's what that would look like:

Phase 1: Core PyO3 bindings

- Wrap StreamProcessor trait
- Expose VideoFrame, AudioBuffer types
- Python decorator: @streamlib.processor
- Register Python processors in global registry

Phase 2: Runtime integration

- Python interpreter lifecycle
- GIL management (minimize locking)
- Error handling across boundary
- Python processor execution in tick system

Phase 3: MCP tool integration

- add_processor accepts Python code
- Dynamic Python processor creation
- AI agents can now generate processors!

Want to start with Phase 1? We could create the streamlib-python crate and basic PyO3 bindings.

> More questions first, so with regard to importing the streamlib-python, so is the agent just coding basically a serverless processor and assuming the environment will inject
> in streamlib for it at runtime? Like how does that work if an agent sends you a chunk of python code with decorators that rely on streamlib-python, I assume you're not
> expecting a whole packaged zip with pyproject.toml and all that stuff?

⏺ Excellent question! This is a critical design decision. Let me break down the options:

The Execution Models

Option 1: Pure Serverless Snippet (Simplest)

# AI agent sends ONLY this code:

@streamlib.processor
def face_detector(frame): # Assumes streamlib is magically available
import cv2 # Also assumes opencv is available
faces = cv2.CascadeClassifier().detectMultiScale(frame.to_numpy())
return streamlib.ObjectDetections(faces)

Agent assumes:

- streamlib is injected/available
- Common packages (numpy, opencv, torch) are pre-installed
- No package management needed

Like:

- Jupyter notebook (assumes packages installed)
- AWS Lambda (code assumes runtime layer)
- Google Colab (pre-installed ML environment)

Option 2: Dependency Declaration

# AI agent sends code + metadata:

"""
requires: - opencv-python==4.8.0 - torch==2.0.0
"""

@streamlib.processor
def face_detector(frame):
import cv2
import torch # ... code

Runtime would:

- Parse dependencies
- pip install at runtime (slow!)
- Then execute code

Problems:

- Installation is SLOW (seconds to minutes)
- Can fail (network, conflicts)
- Security risk (arbitrary packages)

Option 3: Full Package (Most Complex)

# Agent sends zipfile:

# my_processor/

# ├── pyproject.toml

# ├── requirements.txt

# ├── processor.py

# └── models/pretrained_weights.pth

Way too complex for AI agents to generate on-the-fly.

My Recommendation: Pre-installed Environment 🎯

Like a curated Jupyter/Colab environment:

# What AI agents send (just the code):

@streamlib.processor(
name="FaceDetector",
inputs={"video": streamlib.VideoFrame},
outputs={"detections": streamlib.ObjectDetections}
)
def process(inputs): # These are guaranteed available:
import numpy as np
import cv2
import torch

      frame = inputs["video"]
      pixels = frame.to_numpy()
      faces = cv2.CascadeClassifier().detectMultiScale(pixels)
      return {"detections": streamlib.ObjectDetections(faces)}

The streamlib runtime provides:

- Python 3.11+ interpreter
- streamlib package (injected, always available)
- Blessed packages pre-installed:
  - numpy - Array operations
  - opencv-python - Computer vision
  - torch - Deep learning
  - onnxruntime - Model inference
  - pillow - Image processing
  - scipy - Scientific computing
  - scikit-image - Image algorithms

If agent needs something exotic:
Agent: "I need scikit-learn for this"
User: "uv add scikit-learn" # User installs it
Agent: Now I can use it!

The MCP Flow

1. Agent generates code:
   code = """
   @streamlib.processor
   def my_processor(frame):
   import cv2
   return cv2.GaussianBlur(frame.to_numpy(), (5, 5), 0)
   """

2. Agent calls MCP tool:
   {
   "tool": "add_processor",
   "arguments": {
   "name": "BlurFilter",
   "code": "...",
   "language": "python"
   }
   }

3. Runtime executes:
   // streamlib runtime:
   let py_code = params.code;
   let processor = Python::with_gil(|py| {
   // streamlib is already in sys.modules
   py.run(py_code)?;
   // Wrap as Rust StreamProcessor
   PythonProcessor::new(py_code)
   })?;

runtime.add_processor(processor)?;

Practical Example

AI agent workflow:
User: "Add face detection to my camera feed"

Agent thinks:

- Need to generate Python processor
- Can assume streamlib, cv2, numpy are available
- Generate code, send via add_processor tool

Agent generates:
@streamlib.processor
def face_detector(frame):
import cv2
import numpy as np

      classifier = cv2.CascadeClassifier(
          cv2.data.haarcascades + 'haarcascade_frontalface_default.xml'
      )
      gray = cv2.cvtColor(frame.to_numpy(), cv2.COLOR_RGB2GRAY)
      faces = classifier.detectMultiScale(gray, 1.1, 4)

      return streamlib.ObjectDetections([
          streamlib.BoundingBox(x, y, w, h, "face", 1.0)
          for (x, y, w, h) in faces
      ])

Agent calls MCP tool with this code
Runtime loads it, agent connects: camera → face_detector → display
User sees faces detected in real-time!

Dependencies Management Strategy

Tier 1: Pre-installed (blessed)

- Always available
- Part of streamlib Python environment
- Documented for AI agents

Tier 2: User-installed

- User runs: uv add package-name
- Becomes available to all processors
- Agent asks user: "Please install X for this to work"

Tier 3: Not supported

- Agent can't install arbitrary packages
- Security boundary
- Forces agents to use available tools

Does This Make Sense?

So yes, agents code serverless snippets assuming:

- streamlib is injected
- Common ML/vision packages exist
- Just like writing in a Jupyter notebook

Want to move forward with this model? We'd start by:

1. Creating streamlib-python crate (PyO3 bindings)
2. Defining the "blessed environment" package list
3. Implementing the Python processor wrapper
4. Testing with hand-written Python processors first
5. Then wiring up to MCP add_processor tool

Sound good?

> Well the user wont be able to install things if its the scenario of ai powered armor. However, the suit may have an internet connection. Could we compromise and add more
> tools, for example tools to say what languages we support, what packages are available, and since its a singular running runtime, the agent can request new dependencies which
> our mcp (and ultimately our agent in a2a) can decide, if we approve it then we install it into the runtime, then the caller would re-request what packages are available for
> python, then submit its processor (trying to think in terms of two agents talking back and fourth)

⏺ BRILLIANT! This is way better than my approach. You're thinking Agent-to-Agent (A2A) negotiation, which is perfect for autonomous systems like armor where there's no human
to run uv add.

The A2A Capability Negotiation Flow

┌─────────────┐ ┌──────────────────┐
│ Agent A │ │ Agent B (Armor) │
│ (External) │ │ + MCP Server │
└─────────────┘ └──────────────────┘
│ │
│ list_supported_languages │
│────────────────────────────────────>│
│ │
│ ["python", "rust"] │
│<────────────────────────────────────│
│ │
│ list_python_packages │
│────────────────────────────────────>│
│ │
│ ["numpy", "opencv", "torch"] │
│<────────────────────────────────────│
│ │
│ request_package("scikit-learn") │
│────────────────────────────────────>│
│ │
│ Agent B decides: │
│ - Internet? ✓ │
│ - Approved list? ✓ │
│ - Disk space? ✓ │
│ - Auto-approve ✓ │
│ [pip install scikit-learn] │
│ │
│ {"status": "approved", │
│ "version": "1.3.0"} │
│<────────────────────────────────────│
│ │
│ list_python_packages (verify) │
│────────────────────────────────────>│
│ │
│ [..., "scikit-learn"] │
│<────────────────────────────────────│
│ │
│ add_processor(code with sklearn) │
│────────────────────────────────────>│
│ │
│ {"processor_id": "..."} │
│<────────────────────────────────────│

New MCP Tools

⏺ Read(libs/streamlib-mcp/src/tools.rs)
⎿ Read 100 lines

⏺ Let me propose the new MCP tools for capability negotiation:

// New tools to add:

1. list_supported_languages
   → ["python", "rust"]

2. list_python_packages
   → ["numpy==1.24.0", "opencv-python==4.8.0", ...]

3. request_package
   → Agent requests, runtime evaluates/approves/installs

4. get_package_status
   → Check if package request is approved/pending/denied

The Security Model

Approval strategies (configurable):

pub enum PackageApprovalStrategy {
/// Auto-approve from curated allowlist
AllowList(Vec<String>),

      /// Auto-approve any package (dangerous, but useful for dev)
      AutoApprove,

      /// Require human approval
      RequireApproval,

      /// Deny all (locked down environment)
      DenyAll,

}

For armor scenario:
// Armor config:
PackageApprovalStrategy::AllowList(vec![
      "numpy",
      "opencv-python",
      "torch",
      "scikit-learn",
      "pillow",
      "onnxruntime",
      // ... curated safe list
  ])

Implementation Plan

Want me to implement this? Here's what it would look like:

Phase 1: Add capability negotiation tools

- list_supported_languages tool
- list_python_packages tool
- request_package tool
- get_package_status tool

Phase 2: Python environment manager

- Track installed packages
- Install packages on approval
- Verify package availability before code execution

Phase 3: Update add_processor

- Change add_processor to accept code + language
- Validate dependencies before execution
- Clear error if missing packages

Should I start implementing this? It's a really elegant solution for A2A scenarios!

Just to make sure, you said the rust runtime in streamlib-core can run python code, we can dynamically add processors
and presumably the frames and such can be passed to the python code? Did I interpret that correctly? IF so, does it also
 support python packages?

⏺ Wait, let me verify this! I was proposing a future design, but I need to check what actually exists in the codebase
  right now.

⏺ Bash(find /Users/fonta/Repositories/tatolab/streamlib/libs/streamlib-core -name "*.rs" -exec grep -l
      "pyo3\|python\|Python" {} \;)
  ⎿  /Users/fonta/Repositories/tatolab/streamlib/libs/streamlib-core/src/registry.rs
     /Users/fonta/Repositories/tatolab/streamlib/libs/streamlib-core/src/gpu_context.rs

⏺ Bash(grep -n "python\|Python" /Users/fonta/Repositories/tatolab/streamlib/libs/streamlib-core/src/registry.rs | head -20)
  ⎿  111:    /// the actual implementation (e.g., from Python/TypeScript bindings)
     123:/// - Runtime registration (Python, TypeScript, etc.)
     283:/// Used by Python/TypeScript bindings and AI agent tools.

⏺ Bash(grep -i "pyo3" /Users/fonta/Repositories/tatolab/streamlib/Cargo.toml)
  ⎿  pyo3 = { version = "0.27.0", features = ["extension-module"] }

⏺ Bash(ls -la /Users/fonta/Repositories/tatolab/streamlib/libs/ | grep python)
  ⎿  No matches found

⏺ Bash(ls /Users/fonta/Repositories/tatolab/streamlib/libs/)
  ⎿  streamlib
     streamlib-apple
     streamlib-core
     streamlib-mcp

⏺ Bash(grep -A 5 -B 5 "Python" /Users/fonta/Repositories/tatolab/streamlib/CLAUDE.md)
  ⎿  4. Contribute back when possible

     ### Be Polyglot
     … +58 lines (ctrl+o to expand)

⏺ Bash(ls /Users/fonta/Repositories/tatolab/streamlib/packages/ 2>/dev/null || echo "No packages directory")
  ⎿  No packages directory

⏺ I Need to Correct Myself 🚨

  Currently in streamlib-core: NO Python support exists

  Let me be clear about what's real vs. what I was proposing:

  ❌ What DOESN'T Exist Yet

  1. No PyO3 integration - PyO3 is listed in workspace deps but not used
  2. No streamlib-python crate - Doesn't exist
  3. No Python processor wrapper - Can't execute Python code
  4. No dynamic processor loading - Can only use compiled-in processors
  5. Frames can't be passed to Python - No bridge exists

  ✅ What DOES Exist

  1. ProcessorRegistry - Can register Rust processors at runtime
  2. StreamProcessor trait - Interface processors implement
  3. VideoFrame/AudioBuffer - Message types
  4. GPU context - WebGPU for zero-copy
  5. MCP server - For AI agent discovery

  📜 What Your CLAUDE.md Says

  "Python was for validation. Rust is the real thing."
  "We already proved Python works. The decorator API works."

  This suggests you HAD a Python prototype that's been replaced with Rust.

  So To Answer Your Question:

  NO - The Rust runtime in streamlib-core CANNOT currently:
  - ❌ Run Python code
  - ❌ Dynamically add Python processors
  - ❌ Pass frames to Python
  - ❌ Support Python packages

  This would all need to be built from scratch.

  The Real Question

  Given that Python support doesn't exist, should we:

  Option A: Build Python support (big effort)
  - Create streamlib-python crate
  - PyO3 bindings for all types
  - Python processor wrapper
  - Package management system
  - ~2-3 weeks of work

  Option B: Only support pre-compiled Rust processors
  - Agents can only use built-in processors (Camera, Display)
  - Agents can connect them, but not create new ones
  - Simpler, but very limited

  Option C: WASM approach (you already ruled this out)
  - GPU limitations make it impractical

  I was proposing Option A without being clear it doesn't exist yet.

  What do you want to do?

> The claude.md is referring to the runtime / core, etc... not for the dynamic language extensions we are building (may
need to correct that). \
\
My question wasn't phrased correctly, I know that we don't have python now. I meant can our rust runtime support running
 python code at runtime and can it also use packages / support adding pacakges?

⏺ Ah, got it! You're asking: Is it technically feasible? Not "does it exist?" but "CAN we build this?"

  Yes! ✅ Rust CAN run Python at runtime

  Via PyO3:

  use pyo3::prelude::*;

  // Embed Python interpreter in Rust
  fn execute_python_code(code: &str) -> PyResult<()> {
      Python::with_gil(|py| {
          // Run Python code
          py.run(code, None, None)?;
          Ok(())
      })
  }

  Yes! ✅ Can use Python packages

  Python's import system works normally:

  Python::with_gil(|py| {
      // Python code can import installed packages
      py.run(r#"
  import numpy as np
  import cv2
  import torch

  # Just works if packages are installed
      "#, None, None)?;
  });

  Yes! ✅ Can add packages dynamically

  Option 1: Call pip from Rust

  use std::process::Command;

  fn install_package(package: &str) -> Result<(), Box<dyn Error>> {
      let status = Command::new("pip")
          .args(&["install", package])
          .status()?;

      if status.success() {
          Ok(())
      } else {
          Err("pip install failed".into())
      }
  }

  // Then the package is available:
  Python::with_gil(|py| {
      py.run("import scikit_learn", None, None)?;
  });

  Option 2: Use uv (faster)

  Command::new("uv")
      .args(&["pip", "install", package])
      .status()?;

  Yes! ✅ Can pass data between Rust ↔ Python

  PyO3 handles the marshalling:

  use pyo3::types::PyDict;

  Python::with_gil(|py| {
      // Create Python dict from Rust
      let locals = PyDict::new(py);
      locals.set_item("width", 1920)?;
      locals.set_item("height", 1080)?;

      // Python can access Rust data
      py.run(r#"
  print(f"Frame size: {width}x{height}")
      "#, None, Some(locals))?;
  });

  Yes! ✅ Can wrap Rust types for Python

  Make VideoFrame accessible in Python:

  use pyo3::prelude::*;

  #[pyclass]
  struct PyVideoFrame {
      width: u32,
      height: u32,
      texture_handle: u64,  // Opaque handle
  }

  #[pymethods]
  impl PyVideoFrame {
      fn to_numpy(&self) -> PyResult</* numpy array */> {
          // Read GPU texture to numpy if needed
      }
  }

  The Architecture That Works

  Rust runtime hosts Python interpreter:

  ┌────────────────────────────────────┐
  │    Rust StreamRuntime              │
  │                                    │
  │  ┌──────────────────────────────┐ │
  │  │  Python Interpreter (PyO3)   │ │
  │  │                              │ │
  │  │  - numpy, opencv, torch      │ │
  │  │  - Can install more packages │ │
  │  │  - Access to Rust types      │ │
  │  └──────────────────────────────┘ │
  │                                    │
  │  GPU Context (WebGPU)              │
  │  - Textures stay in GPU            │
  │  - Python gets handles/metadata    │
  └────────────────────────────────────┘

  Example: Full Flow

  // 1. Rust runtime receives Python code from agent
  let python_code = r#"
  @streamlib.processor
  def blur_filter(frame):
      import cv2
      pixels = frame.to_numpy()  # Rust→Python
      blurred = cv2.GaussianBlur(pixels, (5,5), 0)
      return streamlib.VideoFrame.from_numpy(blurred)  # Python→Rust
  "#;

  // 2. Check if packages available
  if !has_package("cv2") {
      // 3. Install if approved
      install_package("opencv-python")?;
  }

  // 4. Execute Python code
  Python::with_gil(|py| {
      py.run(python_code, None, None)?;
  });

  // 5. Python processor is now part of pipeline
  // 6. When frames arrive, call Python function
  Python::with_gil(|py| {
      let result = py.eval("blur_filter(frame)", None, None)?;
      // result is now Rust VideoFrame
  });

  Performance Considerations

  The GIL (Global Interpreter Lock):
  - Python has a global lock
  - Only one Python thread runs at a time
  - BUT: You release GIL when not executing Python
  - GPU operations happen in Rust (no GIL)
  - Python just orchestrates

  Zero-copy is maintained:
  - GPU textures stay in GPU memory
  - Python sees handles/metadata
  - Only copies when explicitly requested (.to_numpy())

  So YES, It's All Feasible! ✅

  Can Rust runtime run Python? → Yes (PyO3)Can Python use packages? → Yes (normal imports)Can we add packages? → Yes
  (pip/uv from Rust)Can we pass frames? → Yes (PyO3 marshalling)Can we keep zero-copy? → Yes (opaque handles)

  The architecture is solid. It's just a matter of building it.

  Want to start? It's ~2-3 weeks of work, but totally achievable.
