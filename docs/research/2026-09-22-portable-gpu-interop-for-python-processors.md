# Research memo: portable GPU interop for Python processors

2026-09-22, for the macOS capability-parity delta (`docs/plan/changes/macos-capability-parity.md`
§Product and §Packages & extension model; ticket #2404, the `kDLMetal` capsule). Host for every
probe: Apple Silicon, macOS 26.3 arm64, Python 3.12.10.

Evidence is split into **[V] verified** (primary source read, URL or source line given),
**[P] probed** (command run on this Mac, output quoted) and **[I] inferred**. Nothing here comes
from a blog or a secondary write-up. The probe venv lived at `/tmp/research-venv`, outside the
repo. Probe versions: numpy 2.5.3, torch 2.14.0, mlx 0.32.2, array-api-compat 1.15.0,
jax/jaxlib 0.11.2, ruff 0.16.8, pyright 1.1.414.

## Question

Can DLPack, plus the Python Array API standard through `array-api-compat`, plus a copy the engine
provides, plus a static portability check, let users bring their own tools (torch, cupy, MLX,
jax, numpy) and still keep processor code portable across Linux/NVIDIA and Apple Silicon? Agents
write much of the user code, so the mechanism has to be machine-checkable rather than a
convention.

Today's GPU examples depend on `cupy-cuda13x` only to copy a frame into a kernel texture
(`cupy.from_dlpack(texture)[...] = cupy.from_dlpack(frame)`; `examples/camera-compute-kernel`,
`camera-halftone`, `camera-python-effects`, `camera-virtual-camera` pyprojects). The
fisheye-object-detection example hardcodes `"cuda"`
(`examples/fisheye-object-detection/processors/undistorting_object_detector.py:214,216`).

## Answer

**Partly. Each of the four pieces works, but none of them solves the problem alone, and the
cross-library part is narrower than it sounds.**

1. **The copy that makes the examples non-portable is not an array problem. It is an engine
   copy.** Every engine surveyed ships texture-to-texture copy as a first-class call (Unity,
   Unreal RDG, Godot, WebGPU). The Rust `GpuContext` already has `copy_pixel_buffer_to_texture`
   and `blit_copy` (`runtime/streamlib-engine/src/core/context/gpu_context.rs:1625, :2764`), and
   neither is reachable from Python. Exposing a copy removes CuPy from four of the five examples
   outright. No third-party library is needed, and it is portable by construction.
2. **DLPack interop is real, but it is typed by device, and no library except torch reads the
   capsule on both floors.** On macOS, torch-MPS and MLX import `kDLMetal` zero-copy, and each
   aliases the other's buffers (probed, with writes landing both ways). numpy and jax refuse
   `kDLMetal`. CuPy does not exist on macOS. On Linux, torch-CUDA, CuPy and jax-CUDA take
   `kDLCUDA`. MLX's CUDA backend refuses CUDA DLPack in both directions. **torch is the only
   library that reads the engine's capsule as a GPU tensor on both floors.**
3. **The Array API through `array-api-compat` gives portable math over torch (CPU, CUDA, MPS),
   numpy, CuPy and jax.** One `xp`-only function ran identically on numpy, torch-cpu, torch-mps,
   MLX and jax (probed). The standard deliberately does not cover custom kernels, device
   identity, streams or default devices. It does include in-place slice assignment, but jax
   refuses it. MLX is the weak spot. MLX has had `__array_namespace__` since v0.16.2, but it is
   partial: there is no `.device`, no `to_device`, no `mT`, no `unique_values`, no
   `std(correction=)` and no `asarray(device=)`. So `array_api_compat.device(mlx_array)` raises.
   compat does not wrap MLX yet (draft PR, open tracking issues).
4. **A static check can catch the common non-portable shapes, but not every one.** Ruff's
   built-in `banned-api` (TID251) catches `import cupy`, `from cupy import …`, `torch.cuda.*`
   and `numba.cuda` (probed). It misses `torch.device("cuda")`, `.cuda()`,
   `__cuda_array_interface__` and dynamic imports. A small stdlib-`ast` pass catches the
   literals and attributes (probed). Ruff has no plugin system. No check sees a dependency's
   transitive device choice, a runtime-built string, or what a third-party model does
   internally. pyproject `sys_platform` markers are the standard way to declare per-floor
   dependencies, and MLX's own metadata uses `platform_system` markers.

**The realistic guarantee:** a processor is portable when it uses only the wheel's surface plus
an engine copy, and does its array work either through `array_namespace()` over torch or numpy,
or through torch with `torch.accelerator` (which reports `mps` here). That can be checked
mechanically: TID251 plus a small AST rule plus a dependency-marker rule, with the same test
suite on both CI lanes as the backstop. "Bring any library" is not portable. CuPy is Linux-only,
MLX is macOS-only at the GPU-DLPack level, and jax reads the Linux capsule (by source) but
cannot read the macOS one without a copy. The honest promise is interop with any DLPack consumer on the floor it supports, and
portability through one named path.

## Evidence

### 1. The Python Array API standard

- Current revision is **2025.12** [V] https://data-apis.org/array-api/latest/ (header). numpy
  2.5.3 reports `np.__array_api_version__ == '2025.12'` [P].
- Out of scope: "Execution semantics are out of scope. This includes single-threaded vs.
  parallel execution, task scheduling and synchronization, eager vs. delayed evaluation,
  performance characteristics…"; also I/O, "methods of binding compiled code (e.g., `cffi`,
  `ctypes`)", subclassing, ufuncs [V]
  https://data-apis.org/array-api/latest/purpose_and_scope.html. Custom GPU kernels have no
  place in the standard.
- In-place work is included: "given that both in-place operators and item/slice assignment are
  very widely used … this standard chooses to include them". It also says "Array API consumers
  are _strongly_ advised to avoid _any_ mutating operations when an array object may either be
  a 'view' or own memory of which one or more other array objects may be views" [V]
  https://data-apis.org/array-api/latest/design_topics/copies_views_and_mutation.html. A frame
  imported over DLPack is exactly such a view.
- Devices: `x.device`, a `device=` keyword on creation functions, and `x.to_device()`. Out of
  scope: "Identifying a specific physical or logical device across libraries", global default
  devices, stream/queue management, and context managers. The standard recommends raising on
  mixed-device operations instead of transferring implicitly [V]
  https://data-apis.org/array-api/latest/design_topics/device_support.html. So there is no
  portable way to spell "the GPU". A device can only be taken from an existing array.
- Interchange: DLPack is "the primary/recommended protocol", with "Zero-copy semantics where
  possible, making a copy only if needed", and it requires "a strided, in-memory layout on a
  single device" [V] https://data-apis.org/array-api/latest/design_topics/data_interchange.html.

### 2. array-api-compat

- Wraps NumPy, CuPy, PyTorch and Dask. JAX, sparse, ndonnx, dpnp (and mparray) provide their
  own support [V] https://data-apis.org/array-api-compat/ and
  https://data-apis.org/array-api-compat/supported-array-libraries.html. The shipped package
  has `numpy/`, `cupy/`, `torch/` and `dask/` submodules [P] (`ls array_api_compat` in
  1.15.0). The latest release is 1.15, from 2026-06-07 [V]
  `gh api repos/data-apis/array-api-compat/releases`.
- `array_namespace(*xs, api_version=None, use_compat=None)` returns the compat wrapper when one
  exists ("the wrapped namespace is always preferred if it exists"). Otherwise it falls back to
  `x.__array_namespace__()`, and it raises `TypeError` when the arrays come from more than one
  namespace [V] `array_api_compat/common/_helpers.py:582-680` (1.15.0, installed source).
  Probe: numpy → `array_api_compat.numpy`, torch-mps → `array_api_compat.torch`, MLX →
  `mlx.core` (native fallback), jax → `jax.numpy` [P].
- Documented torch deviations: 0-D type promotion, no negative-step slices, `std`/`var` do not
  accept float `correction`, no `unique_all`, and no `stream` on device transfer [V]
  supported-array-libraries page. There is no MPS-specific deviation in the docs, and the
  probes found none. `flip`, `mean`, `astype`, `asarray(device=device(x))`, `zeros_like` plus
  slice assignment, `matmul`/`permute_dims` and `to_device(x, "cpu")` all worked on `mps:0`
  [P]. The torch `_info.py` lists `mps` among devices (`array_api_compat/torch/_info.py:337`)
  [V].
- MLX in compat: tracking issue https://github.com/data-apis/array-api-compat/issues/450
  (opened 2026-07-18) lists "missing `.device` attribute of the array object : nothing to do
  about it" as a known blocker. Sub-issues #451/#452/#462/#463/#465/#473 are open. The draft PR
  "Add initial MLX support with the creation wrappers"
  https://github.com/data-apis/array-api-compat/pull/453 is open, draft, and unmerged [V].

### 3. Per-library facts

- **numpy 2.x**: native namespace (`__array_namespace__` present, `__array_api_version__`
  2025.12) [P]. `from_dlpack(x, /, *, device=None, copy=None)`, where `device` "must be `"cpu"`
  if passed" [V] https://numpy.org/doc/stable/reference/generated/numpy.from_dlpack.html. Probe:
  `np.from_dlpack(torch_mps)` gives `BufferError: Unsupported device in DLTensor.`, the same as
  for MLX. `np.from_dlpack(torch_mps, device='cpu')` and `np.from_dlpack(mlx, device='cpu')`
  both succeed, with the exporter doing the copy [P].
- **CuPy**: "Like NumPy, CuPy is compatible with the Array API. However, we recommand using the
  Array API compatibility library" [V] https://docs.cupy.dev/en/stable/reference/array_api.html.
  Import accepts only `kDLCUDA`/`kDLCUDAManaged` (ROCm builds accept `kDLROCM`) [V]
  https://github.com/cupy/cupy/blob/v14.2.0/cupy/_core/dlpack.pyx#L335-L343. It is tested on
  Ubuntu, CentOS and Windows Server, not macOS [V] https://docs.cupy.dev/en/stable/install.html.
  `cupy-cuda13x` 14.2.0 ships only manylinux and win wheels [V]
  `https://pypi.org/pypi/cupy-cuda13x/json`.
- **torch**: no native `__array_namespace__` (`hasattr(t, '__array_namespace__') → False`), so
  it goes through compat [P]. `from_dlpack` accepts kDLCPU, kDLCUDA, and kDLMetal → `MPS` [V]
  https://github.com/pytorch/pytorch/blob/v2.14.0/aten/src/ATen/DLConvertor.cpp#L162-L197.
  `__dlpack_device__` on an MPS tensor returns `(kDLMetal: 8, 0)` [P]. It supports the DLPack
  1.x `max_version=(1,0)` capsule and `from_dlpack(copy=)` [P]. The macOS wheel has MPS and no
  CUDA (`torch.backends.mps.is_available() → True`) [P].
- **MLX**: partial native Array API. `__array_namespace__` returns `mlx.core` and rejects any
  explicit `api_version` [V]
  https://github.com/ml-explore/mlx/blob/v0.32.2/python/src/array.cpp#L404-L413. It was added in
  "Array api" https://github.com/ml-explore/mlx/pull/1289 (merged 2024-07-26, first tag
  v0.16.2) [V]. Probe on 0.32.2: `flip`, `permute_dims`, `astype`, `concat`, `matmul`, `clip`,
  `where` and `__array_namespace_info__().devices()` work. `x.device`, `x.to_device`, `x.mT`,
  `unique_values`, `linalg.vector_norm`, `std(correction=1)` and `asarray(device=…)` fail, and
  `mx.__array_api_version__` is absent [P]. The upstream RFC
  https://github.com/ml-explore/mlx/issues/48 is closed (completed 2026-05-05). The full
  conformance-suite work https://github.com/ml-explore/mlx/issues/3514 is open.
  `ddof`→`correction` https://github.com/ml-explore/mlx/issues/4325 closed as completed on
  2026-08-29, after v0.32.2 (2026-08-25) [V]. `__dlpack_device__` returns `(8, 0)` when Metal
  is available, otherwise `(1, 0)` (`array.cpp#L528-L538`) [V][P]. On the CUDA side, 0.32.2
  refuses both directions: `"[convert] CUDA import is not supported."` and `"CUDA DLPack export
  is not supported."` [V]
  https://github.com/ml-explore/mlx/blob/v0.32.2/python/src/convert.cpp#L319 and `#L336`. Wheels
  exist for macOS arm64, manylinux and Windows. The Linux GPU backend is the `mlx[cuda12|cuda13]`
  extra (`mlx-cuda-12/13 0.32.2`) [V] `https://pypi.org/pypi/mlx/json`.
- **jax**: "starting with JAX v0.4.32, `jax.Array` and `jax.numpy` are compatible with the
  Python Array API Standard", and "JAX arrays are immutable, in-place updates are not supported"
  [V] https://docs.jax.dev/en/latest/jax.numpy.html. Import device map: CPU, CUDA, CUDAHost,
  ROCm, ROCmHost, TPUHost, OneAPI, with no Metal [V]
  https://github.com/jax-ml/jax/blob/jax-v0.11.2/jax/_src/dlpack.py#L84-L92. Probe:
  `jnp.from_dlpack(mlx)` and `jax.dlpack.from_dlpack(torch_mps)` both give `TypeError: … unsupported
  device type (DLDeviceType: 8 …)`, and `x[...] = y` raises the `.at[]` TypeError [P]. The only
  macOS GPU path, `jax-metal`, is at 0.1.1 and was last uploaded 2024-10-08 [V]
  `https://pypi.org/pypi/jax-metal/json`. On this Mac, jax is CPU-only (`jax.devices() →
  ['cpu:0']`) [P].

### 4. Empirical probe (this Mac)

Scripts `/tmp/probe1.py` and `/tmp/probe2.py`, run with `/tmp/research-venv/bin/python`. These
are verbatim excerpts:

```
numpy 2.5.3 torch 2.14.0 mlx 0.32.2 array_api_compat 1.15.0 ; mps available True
OK   array_namespace(torch mps): 'array_api_compat.torch'
OK   array_namespace(mlx): 'mlx.core'
OK   mlx __dlpack_device__: (8, 0)
OK   torch mps __dlpack_device__: (<DLDeviceType.kDLMetal: 8>, 0)
OK   numpy __dlpack_device__: (1, 0)
OK   torch-mps xp.zeros_like+setitem slice: 27.0
FAIL aac.device(mlx): AttributeError: 'mlx.core.array' object has no attribute 'device'
OK   torch.from_dlpack(mlx array): (device(type='mps', index=0), 66.0)
OK   mx.from_dlpack(torch mps): ('array', 66.0)
FAIL np.from_dlpack(torch mps): BufferError: Unsupported device in DLTensor.
OK   alias mlx->torch write-through: ('torch dev', 'mps:0', 'mlx sees', [7.0, 0.0, 0.0, 0.0])
OK   alias torch->mlx write-through: ('torch sees', [5.0, 0.0, 0.0, 0.0], 'mlx', [5.0, 0.0, 0.0, 0.0])
OK   portable() on numpy|torch-cpu|torch-mps|mlx|jax: 10.8333…   (flip, clip, astype, mean via xp)
FAIL in-place ellipsis write jax: TypeError: JAX arrays are immutable …
OK   in-place write torch-mps from mlx (cross-lib): 3.0
FAIL jax.dlpack.from_dlpack(torch mps): TypeError: … unsupported device type (DLDeviceType: 8 …)
OK   torch.accelerator.current_accelerator(): device(type='mps')
OK   torch.get_default_device(): device(type='cpu')
```

What the probe shows: torch-MPS ↔ MLX is a true zero-copy alias in both directions. A single
`xp`-only function runs on all five backends. Device helpers and in-place writes are the parts
that break (MLX and jax respectively). The standard itself does not cover picking a GPU.

### 5. kDLMetal consumption in source

- **torch**: MPS↔kDLMetal landed in "[MPS] Enable dlpack integration"
  https://github.com/pytorch/pytorch/pull/158888 (commit `347a97da66`, 2025-07-23). The mapping
  is present at v2.9.0 and absent at v2.8.0 [V] (`curl …/v2.8.0/aten/src/ATen/DLConvertor.cpp |
  grep -i metal` returns nothing; v2.9.0 shows L170-171). The sliced-tensor fix
  https://github.com/pytorch/pytorch/pull/169272 (`5fafc13038`, 2025-12-01, after a revert) adds
  the MPS `storage_offset` branch that is present from v2.10.0 (`DLConvertor.cpp` L408) [V].
  `toDLPackNonOwning` still writes `byte_offset = 0` for MPS in v2.14.0
  (`DLConvertor.cpp#L492-L506`), and its fix #182924 landed 2026-09-12 (`3f24ada281`), after
  2.14 [V]. **Import aliases**: `fromDLPackImpl` wraps the pointer with `at::from_blob(…,
  deleter, …)` and never copies (`DLConvertor.cpp#L453-L483`, v2.14.0) [V]. The probe
  confirmed write-through [P]. The plan's floor of torch ≥ 2.12 is stricter than the source
  minimum (2.9 for the mapping, 2.10 for sliced tensors). The plan records 2.12 as measured;
  why 2.12 rather than 2.10 is **not derivable from the source** (see Unknowns).
- **MLX**: "Add Metal DLPack zero-copy sharing" https://github.com/ml-explore/mlx/pull/3531
  (`0ba7b069`, 2026-06-23), first tag **v0.32.0** [V] (`git tag --contains 0ba7b069`). This
  matches the plan's MLX ≥ 0.32 floor. Import **aliases iff the MTLBuffer is not
  `StorageModePrivate`**: `can_reuse_alien_buffer` returns `buf->storageMode() !=
  MTL::StorageModePrivate`
  (https://github.com/ml-explore/mlx/blob/v0.32.2/mlx/backend/metal/allocator.cpp#L33-L39).
  Otherwise it copies, or with `copy=False` raises "Cannot import a private Metal buffer without
  a copy" (`python/src/convert.cpp#L303-L316`). A dtype change always copies [V]. An
  IOSurface-backed buffer is Shared (plan, `macos-capability-parity.md` §Current state, citing
  MoltenVK `MVKImage.mm:1099`), so the engine's capsule takes the alias path [I].
  `mx.from_dlpack(x, /, *, copy=None)` is the signature (`python/src/ops.cpp#L1946-L1962`) [V].

### 6. Engine precedent: the engine owns the copy

- Unity `Graphics.CopyTexture`: "Copies pixel data from one texture to another", done on the GPU
  [V] https://docs.unity3d.com/ScriptReference/Graphics.CopyTexture.html. `Graphics.Blit`: "Uses
  a shader to copy the pixel data from a texture into a render target" [V]
  https://docs.unity3d.com/ScriptReference/Graphics.Blit.html.
- Unreal RDG: `AddCopyTexturePass(FRDGBuilder&, FRDGTextureRef In, FRDGTextureRef Out, const
  FRHICopyTextureInfo&)`, a `RenderGraphUtils.h` utility that "should be used where possible" [V]
  https://dev.epicgames.com/documentation/en-us/unreal-engine/render-dependency-graph-in-unreal-engine.
- Godot `RenderingDevice.texture_copy(from_texture, to_texture, from_pos, to_pos, size, …)`, next
  to `texture_get_native_handle` for going native [V]
  https://docs.godotengine.org/en/stable/classes/class_renderingdevice.html.
- WebGPU: `copyTextureToTexture` encodes a copy between texture subresources, and
  `copyExternalImageToTexture` "Issues a copy operation of the contents of a platform
  image/canvas into the destination texture", handling colour encoding [V]
  https://www.w3.org/TR/webgpu/#dom-gpucommandencoder-copytexturetotexture and
  `#dom-gpuqueue-copyexternalimagetotexture`.
- GStreamer: memory kinds are negotiated as caps features, and an element converts between them.
  `glupload` (Filter/Video/Uploader) sinks `memory:SystemMemory`, `memory:DMABuf` and
  `memory:GLMemory` [V] https://gstreamer.freedesktop.org/documentation/opengl/glupload.html.
- Counter-example, NVIDIA Holoscan: `holoscan.core.Tensor` "supports both DLPack and NumPy's
  array interface (`__array_interface__` and `__cuda_array_interface__`) so that it can be used
  with other Python libraries such as CuPy" [V]
  https://docs.nvidia.com/holoscan/sdk-user-guide/using-the-sdk/create-an-operator. Nine example
  files `import cupy` (e.g. `examples/cupy_native/matmul.py`,
  `examples/tensor_interop/python/tensor_interop.py`) [V] `gh api
  search/code?q="import cupy"+repo:nvidia-holoscan/holoscan-sdk+path:examples` →
  `total_count 9`. Supported platforms are Linux only (Jetson, IGX, DGX Spark, GH200, Ubuntu,
  RHEL), with no macOS [V] https://docs.nvidia.com/holoscan/sdk-user-guide/sdk_installation.html.
  Holoscan can put a CUDA library in the user's hands because it has one floor.

### 7. Static portability check

- Ruff: "Ruff does not yet support third-party plugins" [V] https://docs.astral.sh/ruff/faq/.
  Built-in `banned-api` (TID251, `lint.flake8-tidy-imports.banned-api`) bans modules and members
  [V] https://docs.astral.sh/ruff/rules/banned-api/. Probe (`/tmp/ruffprobe`, banning `cupy`,
  `torch.cuda`, `numba.cuda`) [P]:
  ```
  proc.py:1:8:  TID251 `cupy` is banned        (import cupy)
  proc.py:2:8:  TID251 `cupy` is banned        (import cupy as cp)
  proc.py:3:1:  TID251 `cupy` is banned        (from cupy import asarray)
  proc.py:6:5:  TID251 `torch.cuda` is banned  (torch.cuda.is_available())
  proc.py:9:19: TID251 `torch.cuda` is banned  (from torch import cuda)
  proc.py:11:19: TID251 `numba.cuda` is banned (from numba import cuda as nc)
  ```
  Not flagged: `torch.device("cuda")`, `torch.zeros(3).cuda()`,
  `importlib.import_module("cu"+"py")` and `obj.__cuda_array_interface__`.
- A stdlib `ast` walk over the same file flagged the `"cuda"` literal (line 7), `.cuda`
  (lines 6 and 8) and `__cuda_array_interface__` (line 12). It did not flag the dynamic import
  on line 11 [P].
- pyright can enforce platform-shaped surfaces if a stub puts methods under `if sys.platform ==
  "linux":` / `"darwin":`. Probe (`/tmp/pyrightprobe`): `pyright --pythonplatform Linux` errors
  on `export_iosurface`, and `--pythonplatform Darwin` errors on `export_dma_buf` [P]. This runs
  against the plan's "one surface, refuse by name" design (see Decisions).
- Dependencies: `sys_platform` "is the most well defined field for use when declaring platform
  specific dependencies" (`linux`, `darwin`) [V]
  https://packaging.python.org/en/latest/specifications/dependency-specifiers/. MLX's own
  metadata does this: `mlx-metal==0.32.2; platform_system == "Darwin"`, `mlx-cuda-13==0.32.2;
  platform_system == "Linux" and extra == "cuda13"` [V] `https://pypi.org/pypi/mlx/json`
  `requires_dist`. A check can read `[project].dependencies` and reject a CUDA-only distribution
  (`cupy-cuda*`, `nvidia-*`, `pycuda`) that has no marker [I].
- No existing Python "GPU portability linter" with a primary source was found. The
  **UNKNOWN** entry below records that absence, not a claim that none exists.
- What no static check can see: device choices inside third-party libraries (a model zoo that
  picks `cuda` internally), strings built at runtime, transitive dependencies, and a capsule
  handed to a consumer that does not accept the other floor's device type [I]. The same test
  suite on both CI lanes (already decided in the parity delta, §Product) is the only backstop
  for those.

### 8. torch's device-agnostic idiom

- `torch.accelerator` is present at v2.6.0 and absent at v2.5.0 [V] (`curl -w %{http_code}
  …/v2.5.0/torch/accelerator/__init__.py → 404`, `v2.6.0 → 200`). Module doc: "This package
  introduces support for the current accelerator in python" [V]
  https://github.com/pytorch/pytorch/blob/v2.14.0/torch/accelerator/__init__.py.
  `current_accelerator(check_available=False)` "Return the device of the accelerator available
  at compilation time" (L103). The 2.14 docs list CUDA, XPU, MPS and MTIA [V]
  https://docs.pytorch.org/docs/2.14/accelerator.html. The probe returns `device(type='mps')`
  [P].
- `torch.get_default_device()` is present from v2.3.0 (absent at v2.2.0), and
  `set_default_device` from v2.0.0 [V] (grep of `torch/__init__.py` at each tag). It defaults to
  `cpu` (probe) [P]. So the idiom is `torch.accelerator.current_accelerator()`, not the default
  device.

## Support matrix

"GPU capsule" means the engine's own capsule on that floor: `kDLCUDA` on Linux, `kDLMetal` on
macOS.

| Library | Array API | `from_dlpack` import | `__dlpack__` export | Device types accepted | macOS arm64 GPU | Reads the engine's GPU capsule |
|---|---|---|---|---|---|---|
| numpy 2.x | native (2025.12), compat wraps | yes | yes, kDLCPU | CPU only; `device='cpu'` asks the exporter to copy | n/a (CPU) | no; copy via `device='cpu'` |
| CuPy 14 | native, compat recommended | yes | yes, kDLCUDA/Managed | CUDA, CUDAManaged (ROCm build: ROCm) | **not available** | Linux only |
| torch (CUDA) | via compat | yes | yes | CPU, CUDA (+ ROCm build) | n/a | Linux: yes (source; not probed here) |
| torch (MPS) ≥2.9 (plan floor 2.12) | via compat; no MPS-specific gaps found | yes, aliases (`from_blob`) | yes, kDLMetal | CPU, Metal | yes | macOS: yes (probed) |
| MLX 0.32 | **partial native**; compat has no wrapper (draft PR #453) | yes (`mx.from_dlpack`); Metal alias if not Private | yes, kDLMetal, or CPU without Metal | CPU, Metal; **CUDA refused both ways** | yes | macOS only |
| jax 0.11 | native since 0.4.32; immutable | yes | yes | CPU, CUDA, ROCm, TPUHost, OneAPI; **no Metal** | CPU only (jax-metal 0.1.1, 2024) | Linux CUDA: yes (source); macOS: no |

## What remains unknown

- **Why the plan's torch floor is 2.12** when source shows kDLMetal import in 2.9 and the
  sliced-tensor fix in 2.10. Possibly a bug fixed in 2.11–2.12 that the hand-built capsule hit,
  or simply the oldest version measured. Not derivable from `DLConvertor.cpp` history.
- **torch-CUDA / CuPy / jax-CUDA against the engine's `kDLCUDA` capsule**: established by source
  only, not probed in this memo (no NVIDIA host used). The Linux lane already runs torch/CuPy
  examples, which is indirect evidence.
- **MLX Array API trajectory**: whether `.device` ever lands (compat #450 says "nothing to do
  about it"), when `std(correction=)` from #4325 ships (merged after 0.32.2; no newer release as
  of 2026-09-22), and whether compat #453 merges.
- **MLX on the Linux CUDA backend** consuming a `kDLCUDA` capsule: refused in 0.32.2. Upstream
  plans are unknown.
- **Stream/queue ordering** between the engine's Vulkan/Metal work and a torch-MPS or MLX
  command queue on the aliased buffer. The Array API explicitly leaves streams out of scope.
  How `as_device_tensor`'s blit-back ordering composes with MLX's lazy evaluation was not
  probed.
- **A primary-sourced GPU-portability linter** in the ML ecosystem: none found. Absence is not
  proven.
- Whether `numpy.from_dlpack(engine_kDLMetal_capsule, device='cpu')` works depends on the
  wheel's `__dlpack__` honouring `dl_device=(1,0)`. That is a wheel design point, not an
  external fact.

## Decisions this raises

These are listed, not decided. They go to `/align` against `docs/plan/ARCHITECTURE.md`
§Packages & extension model and the macOS parity delta.

1. **Expose an engine copy to Python.** Should `GpuContext`'s `copy_pixel_buffer_to_texture` /
   `blit_copy` (or a surface-to-texture copy on the processor context) reach Python, so frame →
   kernel texture needs no third-party array library? Where does it live on the Full vs. limited
   surface, and how is it ordered against the kernel dispatch?
2. **The one cross-platform array path the scaffold and examples model.** The choices: torch
   with `torch.accelerator` (the only library that reads both floors' capsules), or
   `array-api-compat` `xp` code (portable math over torch and numpy, but MLX is partial and there
   is no portable way to name a GPU), or both with a stated rule. Is MLX a supported interop
   target or a floor-specific one?
3. **The portability check's shape.** Candidates: ruff TID251 config shipped in the scaffold, a
   dedicated `ast` rule (literals `"cuda"`/`"mps"`, `.cuda()`, `__cuda_array_interface__`,
   `torch.cuda`), a pyproject rule that CUDA-only distributions need a `sys_platform` marker, or
   an `xtask`/CLI verb that runs all three. Does it gate `streamlib run`, CI, or both? Is it
   advisory to agents or blocking?
4. **Whether any wheel surface may be platform-conditioned in the stub** (`sys.platform` blocks
   plus `pyright --pythonplatform`). That would make floor-specific calls a type error. It
   conflicts with the delta's "no class or method exists on one floor and not the other; refuse
   by name at runtime".
5. **A CPU fallback for the capsule**: should `__dlpack__` honour `dl_device=(kDLCPU,0)` so
   numpy and jax (macOS) can import with an explicit copy, or should they be told to go through
   `as_numpy`?
6. **Converted consumers**: whether the four CuPy examples and fisheye-object-detection are
   re-expressed on the chosen path as part of the parity milestone or filed as backlog, per
   §Consumers.
